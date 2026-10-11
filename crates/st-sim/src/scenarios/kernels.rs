//! Scenario `math-kernels` (ADR 0008): the `st-math` kernels, hashed.
//!
//! The scenario has no [`Sim`](crate::Sim): it feeds a fixed set of inputs
//! through every function of [`st_math::det`], the Kepler and state-vector
//! functions of [`st_math::orbit`] and the helpers of [`st_math::interp`], and
//! folds the bit patterns of the results into the state hash (format version
//! 1). A platform that computes any of them differently hashes differently,
//! at the tick whose inputs expose it.
//!
//! # Inputs
//!
//! * *Tick 0* (the setup) runs a fixed table of edge values, [`EDGE`]: signed
//!   zeros, the smallest subnormal, tiny and huge magnitudes, `pi`, values
//!   beyond the range where an angle needs argument reduction and the
//!   largest finite `f64`. Every `det` function that is total on the table is
//!   applied to every value, the others to the values in their domain, and
//!   the two-argument ones to every pair.
//! * *Tick `t` in `1..=1200`* draws its inputs from the `st-rng` stream
//!   `Stream::from_seed(SEED).derive_label("math-kernels").derive(t)`, read
//!   in source order with a cursor. The order of the draws is the order of
//!   the code in this file; changing it changes the hashes.
//!
//! # Stream
//!
//! Each tick hashes its own stream, and the tick's hash is the checkpoint
//! value (so a checkpoint covers the previous tick's hash, and through it
//! every earlier tick):
//!
//! ```text
//! "STQK" | u32 format version | u64 tick | u64 seed | u64 hash of tick - 1 (0 at tick 0)
//! tick 0:  "EDGE" section
//! tick >0: "DETM", "ORBT" and "INTP" sections, in that order
//! ```
//!
//! An `f64` is its bit pattern (`-0.0` as `+0.0`; the sign of a zero result
//! of `min`, `max` and `clamp` is written as a separate byte at tick 0). A
//! result that is `Result` or `Option` is a tag byte (1 for success) followed
//! by the value, or by a one-byte error code. A NaN result is an error of the
//! scenario, not a value (ADR 0008), so every input is chosen in its
//! function's domain.
//!
//! The harness `--dump-tick` output has one line per section.

use st_math::det;
use st_math::glam::{DQuat, DVec3};
use st_math::interp;
use st_math::orbit::{Elements, OrbitError, StateVectors, solve_kepler};
use st_rng::{Cursor, Stream};
use st_time::Tick;

use super::{Checkpoint, RunError};
use crate::hash::{DumpLine, HASH_FORMAT_VERSION, HashError, StateHasher};
use crate::sim::CHECKPOINT_INTERVAL;

/// The seed of the scenario's `st-rng` stream.
const SEED: u64 = 4;
/// How many ticks the scenario runs (10 s at 120 Hz).
const TICKS: Tick = 1200;

/// The tick-0 edge values (see the module documentation).
pub const EDGE: [f64; 24] = [
    0.0,
    -0.0,
    5e-324,
    -5e-324,
    1e-300,
    1e-17,
    0.5,
    -0.5,
    1.0,
    -1.0,
    2.0,
    -2.0,
    core::f64::consts::PI,
    -core::f64::consts::FRAC_PI_2,
    100.0,
    710.0,
    -745.0,
    1e15,
    1e22,
    -1e22,
    1e300,
    -1e300,
    f64::MAX,
    f64::MIN,
];

/// The integer exponents tried with [`det::powi`] at tick 0.
const EDGE_EXPONENTS: [i32; 9] = [i32::MIN, -1025, -3, -1, 0, 1, 2, 10, i32::MAX];

/// A `det` function with the test of where it is defined.
type Guarded = (fn(f64) -> bool, fn(f64) -> f64);

/// The scenario `math-kernels`: a seed and a tick count, no simulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MathKernels {
    /// Unique, lower-case, hyphenated name used in the reference file.
    pub name: &'static str,
    /// Seed of the input stream.
    pub seed: u64,
    /// How many ticks to run.
    pub ticks: Tick,
}

/// The scenario `math-kernels` (1,200 ticks).
pub const fn math_kernels() -> MathKernels {
    MathKernels {
        name: "math-kernels",
        seed: SEED,
        ticks: TICKS,
    }
}

impl MathKernels {
    /// Runs the scenario and returns its checkpoints: tick 0, every
    /// [`CHECKPOINT_INTERVAL`] ticks, and the final tick.
    ///
    /// # Errors
    ///
    /// [`RunError::Hash`] if a result is a NaN.
    pub fn run(&self) -> Result<Vec<Checkpoint>, RunError> {
        self.run_with_interval(CHECKPOINT_INTERVAL)
    }

    /// Like [`MathKernels::run`] with a checkpoint every `every` ticks (at
    /// least 1).
    ///
    /// # Errors
    ///
    /// As [`MathKernels::run`].
    pub fn run_with_interval(&self, every: Tick) -> Result<Vec<Checkpoint>, RunError> {
        let every = every.max(1);
        let mut out = Vec::new();
        let mut prev = 0;
        for tick in 0..=self.ticks {
            let mut h = StateHasher::new();
            self.write_tick(tick, prev, &mut h)
                .map_err(RunError::Hash)?;
            prev = h.finish();
            if tick % every == 0 || tick == self.ticks {
                out.push(Checkpoint { tick, hash: prev });
            }
        }
        Ok(out)
    }

    /// The canonical hash input of `tick`, one [`DumpLine`] per section.
    /// `Ok(None)` if the scenario is shorter than `tick`.
    ///
    /// # Errors
    ///
    /// As [`MathKernels::run`].
    pub fn dump_at(&self, tick: Tick) -> Result<Option<Vec<DumpLine>>, RunError> {
        if tick > self.ticks {
            return Ok(None);
        }
        let mut prev = 0;
        for t in 0..tick {
            let mut h = StateHasher::new();
            self.write_tick(t, prev, &mut h).map_err(RunError::Hash)?;
            prev = h.finish();
        }
        let mut h = StateHasher::recording();
        self.write_tick(tick, prev, &mut h)
            .map_err(RunError::Hash)?;
        Ok(Some(h.into_dump()))
    }

    /// The one place that defines the stream of a tick, so the hash and the
    /// dump cannot disagree.
    fn write_tick(&self, tick: Tick, prev: u64, h: &mut StateHasher) -> Result<(), HashError> {
        h.begin_line("STQK", None);
        h.tag(b"STQK");
        h.u32(HASH_FORMAT_VERSION);
        h.u64(tick);
        h.u64(self.seed);
        h.u64(prev);
        if tick == 0 {
            h.begin_line("EDGE", None);
            h.tag(b"EDGE");
            return edge_section(h);
        }
        let mut c = Stream::from_seed(self.seed)
            .derive_label("math-kernels")
            .derive(tick)
            .cursor();
        h.begin_line("DETM", None);
        h.tag(b"DETM");
        det_section(&mut c, h)?;
        h.begin_line("ORBT", None);
        h.tag(b"ORBT");
        orbit_section(&mut c, h)?;
        h.begin_line("INTP", None);
        h.tag(b"INTP");
        interp_section(&mut c, h)
    }
}

// ---------------------------------------------------------------------------
// Tick 0: the edge table.

fn edge_section(h: &mut StateHasher) -> Result<(), HashError> {
    // Total on every edge value.
    let unary: [fn(f64) -> f64; 15] = [
        det::sin,
        det::cos,
        det::tan,
        det::atan,
        det::sinh,
        det::cosh,
        det::tanh,
        det::asinh,
        det::exp,
        det::exp2,
        det::exp_m1,
        det::cbrt,
        det::to_radians,
        det::to_degrees,
        |x| det::clamp(x, -1.0, 1.0),
    ];
    // Defined on part of the line: the guard says where.
    let guarded: [Guarded; 7] = [
        (|x| x.abs() <= 1.0, det::asin),
        (|x| x.abs() <= 1.0, det::acos),
        (|x| x.abs() <= 1.0, det::atanh),
        (|x| x >= 1.0, det::acosh),
        (|x| x >= 0.0, det::ln),
        (|x| x >= -1.0, det::ln_1p),
        (|x| x >= 0.0, det::log2),
    ];
    for f in unary {
        for x in EDGE {
            h.f64(f(x))?;
        }
    }
    for (inside, f) in guarded {
        for x in EDGE.into_iter().filter(|x| inside(*x)) {
            h.f64(f(x))?;
        }
    }
    for x in EDGE.into_iter().filter(|x| *x >= 0.0) {
        h.f64(det::log10(x))?;
    }
    for x in EDGE {
        let (s, c) = det::sin_cos(x);
        h.f64(s)?;
        h.f64(c)?;
        for n in EDGE_EXPONENTS {
            h.f64(det::powi(x, n))?;
        }
    }
    for x in EDGE {
        for y in EDGE {
            h.f64(det::atan2(y, x))?;
            h.f64(det::hypot(x, y))?;
            for r in [det::min(x, y), det::max(x, y)] {
                h.f64(r)?;
                // The sign of a zero is the point of these two.
                h.bool(r.is_sign_negative());
            }
            if x > 0.0 && y.abs() < 1e3 {
                h.f64(det::powf(x, y))?;
            }
        }
        let r = det::clamp(x, -0.0, 0.0);
        h.f64(r)?;
        h.bool(r.is_sign_negative());
    }
    h.bool(det::fp_env_is_default());
    Ok(())
}

// ---------------------------------------------------------------------------
// Ticks 1..: random inputs.

/// An `f64` in `[-1, 1)`.
fn signed(c: &mut Cursor) -> f64 {
    c.next_f64() * 2.0 - 1.0
}

/// A value in `(-10^6, 10^6)` with its magnitude spread over seven decades.
fn wide(c: &mut Cursor) -> f64 {
    let s = signed(c);
    s * det::powi(10.0, c.next_below(7) as i32)
}

/// A non-negative value below `10^4` with its magnitude spread over nine decades.
fn positive(c: &mut Cursor) -> f64 {
    let u = c.next_f64();
    u * det::powi(10.0, c.next_below(9) as i32 - 4)
}

fn vec3(c: &mut Cursor, scale: f64) -> DVec3 {
    let x = signed(c) * scale;
    let y = signed(c) * scale;
    let z = signed(c) * scale;
    DVec3::new(x, y, z)
}

/// A unit quaternion from four random components, or the identity if they
/// are too short to normalise.
fn quat(c: &mut Cursor) -> DQuat {
    let x = signed(c);
    let y = signed(c);
    let z = signed(c);
    let w = signed(c);
    unit_or_identity(DQuat::from_xyzw(x, y, z, w))
}

fn unit_or_identity(q: DQuat) -> DQuat {
    if q.length_squared() > 1e-6 {
        q.normalize()
    } else {
        DQuat::IDENTITY
    }
}

fn det_section(c: &mut Cursor, h: &mut StateHasher) -> Result<(), HashError> {
    let x = wide(c);
    let y = wide(c);
    let u = signed(c);
    let p = positive(c);
    let mid = signed(c) * 40.0;
    let b = signed(c) * 10.0;
    let n = c.next_below(41) as i32 - 20;

    let (s, co) = det::sin_cos(x);
    let results = [
        det::sin(x),
        det::cos(x),
        det::tan(x),
        s,
        co,
        det::asin(u),
        det::acos(u),
        det::atan(x),
        det::atan2(y, x),
        det::sinh(mid),
        det::cosh(mid),
        det::tanh(mid),
        det::asinh(x),
        det::acosh(1.0 + p),
        det::atanh(u),
        det::exp(mid),
        det::exp2(mid),
        det::exp_m1(mid),
        det::ln(p),
        det::ln_1p(u),
        det::log2(p),
        det::log10(p),
        det::powf(p, b),
        det::cbrt(x),
        det::hypot(x, y),
        det::powi(b, n),
        det::min(x, y),
        det::max(x, y),
        det::clamp(x, det::min(y, b), det::max(y, b)),
        det::to_radians(x),
        det::to_degrees(x),
    ];
    for r in results {
        h.f64(r)?;
    }
    h.bool(det::fp_env_is_default());
    Ok(())
}

/// A one-byte code for an [`OrbitError`] (the enum is `non_exhaustive`).
fn error_code(e: OrbitError) -> u8 {
    match e {
        OrbitError::NonFinite => 1,
        OrbitError::InvalidGravitationalParameter => 2,
        OrbitError::InvalidSemiMajorAxis => 3,
        OrbitError::InvalidEccentricity => 4,
        OrbitError::NotElliptic => 5,
        OrbitError::NearParabolic => 6,
        OrbitError::DegenerateState => 7,
        _ => 255,
    }
}

/// Writes a tag byte and the value, or tag 0 and the error code.
fn put<T>(
    h: &mut StateHasher,
    result: &Result<T, OrbitError>,
    write: impl FnOnce(&mut StateHasher, &T) -> Result<(), HashError>,
) -> Result<(), HashError> {
    match result {
        Ok(v) => {
            h.u8(1);
            write(h, v)
        }
        Err(e) => {
            h.u8(0);
            h.u8(error_code(*e));
            Ok(())
        }
    }
}

fn put_scalar(h: &mut StateHasher, r: &Result<f64, OrbitError>) -> Result<(), HashError> {
    put(h, r, |h, v| h.f64(*v))
}

fn put_state(h: &mut StateHasher, r: &Result<StateVectors, OrbitError>) -> Result<(), HashError> {
    put(h, r, |h, s| {
        h.vec3(s.position)?;
        h.vec3(s.velocity)
    })
}

fn put_elements(h: &mut StateHasher, r: &Result<Elements, OrbitError>) -> Result<(), HashError> {
    put(h, r, |h, e| {
        h.f64(e.semi_major_axis)?;
        h.f64(e.eccentricity)?;
        h.f64(e.inclination)?;
        h.f64(e.longitude_of_ascending_node)?;
        h.f64(e.argument_of_periapsis)?;
        h.f64(e.mean_anomaly)
    })
}

fn orbit_section(c: &mut Cursor, h: &mut StateHasher) -> Result<(), HashError> {
    // Elements: a from 1e6 to 1e12 m, mu from 1e10 to 1e16 m^3/s^2. Some
    // draws land on the special cases (circular, equatorial, retrograde
    // equatorial, the largest supported eccentricity).
    let a = det::powf(10.0, 6.0 + 6.0 * c.next_f64());
    let mu = det::powf(10.0, 10.0 + 6.0 * c.next_f64());
    let ecc = match c.next_below(8) {
        0 => 0.0,
        1 => 0.999,
        2 => 0.9 + 0.099 * c.next_f64(),
        _ => 0.9 * c.next_f64(),
    };
    let tau = 2.0 * core::f64::consts::PI;
    let inclination = match c.next_below(6) {
        0 => 0.0,
        1 => core::f64::consts::PI,
        _ => c.next_f64() * core::f64::consts::PI,
    };
    let node = c.next_f64() * tau;
    let periapsis = c.next_f64() * tau;
    let anomaly_span = if c.next_below(4) == 0 { 1.0e3 } else { tau };
    let mean_anomaly = signed(c) * anomaly_span;
    let elements = Elements {
        semi_major_axis: a,
        eccentricity: ecc,
        inclination,
        longitude_of_ascending_node: node,
        argument_of_periapsis: periapsis,
        mean_anomaly,
    };
    let span = signed(c);

    put_scalar(h, &solve_kepler(mean_anomaly, ecc))?;
    put_scalar(h, &elements.mean_motion(mu))?;
    let period = elements.period(mu);
    put_scalar(h, &period)?;
    // Up to three periods either way, or 0 if there is no period.
    let dt = period.map_or(0.0, |p| span * 3.0 * p);

    let state = elements.to_state_vectors(mu);
    put_state(h, &state)?;
    if let Ok(state) = &state {
        put_elements(h, &Elements::from_state_vectors(state, mu))?;
        put_state(h, &state.propagate(mu, dt))?;
    }
    put_elements(h, &elements.advanced(mu, dt))?;
    put_state(h, &elements.state_after(mu, dt))?;

    // A state that is not made from elements: it is an ellipse when its speed
    // is below sqrt(2) times the circular speed, which most draws are,
    // and a hyperbola (an error) otherwise.
    let position = vec3(c, 1.0e7);
    let circular = (mu / position.length()).sqrt();
    let free = StateVectors {
        position,
        velocity: vec3(c, circular * 1.1),
    };
    let free_elements = Elements::from_state_vectors(&free, mu);
    put_elements(h, &free_elements)?;
    put_state(h, &free.propagate(mu, dt))
}

fn interp_section(c: &mut Cursor, h: &mut StateHasher) -> Result<(), HashError> {
    let t = c.next_f64() * 2.0 - 0.5;
    let (a, b) = (wide(c), wide(c));
    let (va, vb) = (vec3(c, 1.0e3), vec3(c, 1.0e3));
    h.f64(interp::lerp(a, b, t))?;
    h.vec3(interp::lerp_vec3(va, vb, t))?;

    // Rotations: unrelated, nearly equal (the linear fallback of `slerp`)
    // and opposite (the sign flip).
    let qa = quat(c);
    let qb = match c.next_below(4) {
        0 => {
            let (dx, dy, dz, dw) = (signed(c), signed(c), signed(c), signed(c));
            let nudge = DQuat::from_xyzw(dx, dy, dz, dw) * 1.0e-3;
            unit_or_identity(qa + nudge)
        }
        1 => -qa,
        _ => quat(c),
    };
    h.quat(interp::nlerp(qa, qb, t))?;
    h.quat(interp::slerp(qa, qb, t))?;

    // Hermite: the duration is 0 now and then, where the velocity is `None`.
    let (p0, v0) = (vec3(c, 1.0e4), vec3(c, 10.0));
    let (p1, v1) = (vec3(c, 1.0e4), vec3(c, 10.0));
    let duration = if c.next_below(8) == 0 {
        0.0
    } else {
        0.1 + 10.0 * c.next_f64()
    };
    h.vec3(interp::hermite(p0, v0, p1, v1, duration, t))?;
    match interp::hermite_velocity(p0, v0, p1, v1, duration, t) {
        Some(v) => {
            h.u8(1);
            h.vec3(v)
        }
        None => {
            h.u8(0);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoints_every_120_ticks_and_at_the_end() {
        let ticks: Vec<_> = math_kernels()
            .run()
            .unwrap()
            .iter()
            .map(|c| c.tick)
            .collect();
        let want: Vec<Tick> = (0..=10).map(|i| i * 120).collect();
        assert_eq!(ticks, want);
    }

    #[test]
    fn a_finer_interval_keeps_the_coarse_checkpoints() {
        let k = math_kernels();
        let coarse = k.run().unwrap();
        let fine = k.run_with_interval(1).unwrap();
        assert_eq!(fine.len(), 1201);
        for c in coarse {
            assert!(fine.contains(&c), "{c:?}");
        }
    }

    #[test]
    fn the_dump_is_the_hashed_stream() {
        let k = math_kernels();
        let checkpoints = k.run_with_interval(1).unwrap();
        for tick in [0, 1, 2, 77, 1200] {
            let lines = k.dump_at(tick).unwrap().unwrap();
            let mut h = StateHasher::new();
            for line in &lines {
                h.bytes(&line.bytes);
            }
            assert_eq!(h.finish(), checkpoints[tick as usize].hash, "tick {tick}");
            let sections: Vec<_> = lines.iter().map(|l| l.section).collect();
            if tick == 0 {
                assert_eq!(sections, ["STQK", "EDGE"]);
            } else {
                assert_eq!(sections, ["STQK", "DETM", "ORBT", "INTP"]);
            }
        }
        assert!(k.dump_at(k.ticks + 1).unwrap().is_none());
    }

    #[test]
    fn the_seed_and_the_chain_matter() {
        let a = math_kernels().run().unwrap();
        let mut other = math_kernels();
        other.seed += 1;
        assert_ne!(a, other.run().unwrap());
        // Each tick covers the previous one: the last hash is not the hash of
        // the last tick's inputs alone.
        let mut h = StateHasher::new();
        math_kernels().write_tick(1200, 0, &mut h).unwrap();
        assert_ne!(h.finish(), a.last().unwrap().hash);
    }

    #[test]
    fn the_edge_values_are_finite_and_distinct() {
        assert!(EDGE.iter().all(|x| x.is_finite()));
        for (i, a) in EDGE.iter().enumerate() {
            for b in &EDGE[i + 1..] {
                // `0.0 == -0.0`, so compare bit patterns.
                assert_ne!(a.to_bits(), b.to_bits());
            }
        }
    }

    #[test]
    fn the_orbit_section_reaches_ok_and_error_results() {
        // The Kepler solver and the conversions are called on inputs that
        // succeed and on inputs that do not (a hyperbolic state, `e` at the
        // limit), so both result paths are in the hash.
        let mu = 1.0e14;
        let hyperbolic = StateVectors {
            position: DVec3::new(1.0e7, 0.0, 0.0),
            velocity: DVec3::new(0.0, 0.0, 1.0e5),
        };
        assert_eq!(
            Elements::from_state_vectors(&hyperbolic, mu).map(|_| ()),
            Err(OrbitError::NotElliptic)
        );
        assert!(solve_kepler(1.0, 0.5).is_ok());
        assert_eq!(error_code(OrbitError::NotElliptic), 5);
    }
}
