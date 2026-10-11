//! Behaviour tests for `st_math::orbit`: physical invariants, round trips, the frame
//! convention of ADR 0007 and degenerate input. Bit-pattern goldens are in `orbit_golden.rs`.
//!
//! Inputs that need many values come from a small local SplitMix64 generator, so the tests
//! need no dependency and every run is the same.

use core::f64::consts::{PI, TAU};
use st_math::det;
use st_math::glam::DVec3;
use st_math::orbit::{
    Elements, KEPLER_ITERATIONS, MAX_ECCENTRICITY, OrbitError, StateVectors, solve_kepler,
};

const MU: f64 = 3.986004418e14;

/// SplitMix64: a tiny deterministic generator for test inputs.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform value in `[lo, hi)`.
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + (hi - lo) * unit
    }
}

fn elements(a: f64, e: f64, i: f64, raan: f64, argp: f64, m: f64) -> Elements {
    Elements {
        semi_major_axis: a,
        eccentricity: e,
        inclination: i,
        longitude_of_ascending_node: raan,
        argument_of_periapsis: argp,
        mean_anomaly: m,
    }
}

/// Smallest absolute difference between two angles on the circle.
fn angle_gap(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(TAU);
    det::min(d, TAU - d)
}

#[test]
fn kepler_residual_is_at_rounding_level() {
    let mut rng = Rng(1);
    let mut worst = 0.0f64;
    for k in 0..20_000 {
        // Every fifth case is pinned to a hard corner: high e and tiny mean anomaly.
        let e = if k % 5 == 0 {
            rng.range(0.99, MAX_ECCENTRICITY)
        } else {
            rng.range(0.0, 1.0) * MAX_ECCENTRICITY
        };
        let m = if k % 5 == 0 {
            rng.range(-1e-3, 1e-3)
        } else {
            rng.range(-50.0, 50.0)
        };
        let big_e = solve_kepler(m, e).unwrap();
        assert!((0.0..=TAU).contains(&big_e), "E={big_e} for M={m}, e={e}");
        // The residual must be taken modulo a full turn, because M was wrapped.
        let residual = angle_gap(big_e - e * det::sin(big_e), m);
        worst = det::max(worst, residual);
    }
    assert!(worst < 2e-15, "worst residual {worst:e}");
}

#[test]
fn kepler_solver_symmetry_and_special_values() {
    assert_eq!(solve_kepler(0.0, 0.7).unwrap(), 0.0);
    assert_eq!(solve_kepler(PI, 0.7).unwrap(), PI);
    assert_eq!(solve_kepler(1.25, 0.0).unwrap(), 1.25);
    // E(2*pi - M) = 2*pi - E(M).
    let e = solve_kepler(1.0, 0.8).unwrap();
    let mirrored = solve_kepler(TAU - 1.0, 0.8).unwrap();
    assert!((e + mirrored - TAU).abs() < 1e-15);
}

#[test]
fn kepler_rejects_bad_input() {
    assert_eq!(solve_kepler(f64::NAN, 0.5), Err(OrbitError::NonFinite));
    assert_eq!(solve_kepler(f64::INFINITY, 0.5), Err(OrbitError::NonFinite));
    assert_eq!(
        solve_kepler(f64::NEG_INFINITY, 0.5),
        Err(OrbitError::NonFinite)
    );
    assert_eq!(solve_kepler(1.0, f64::NAN), Err(OrbitError::NonFinite));
    assert_eq!(
        solve_kepler(1.0, -0.1),
        Err(OrbitError::InvalidEccentricity)
    );
    assert_eq!(
        solve_kepler(1.0, f64::NEG_INFINITY),
        Err(OrbitError::InvalidEccentricity)
    );
    assert_eq!(solve_kepler(1.0, 1.0), Err(OrbitError::NotElliptic));
    assert_eq!(solve_kepler(1.0, 3.0), Err(OrbitError::NotElliptic));
    assert_eq!(
        solve_kepler(1.0, f64::INFINITY),
        Err(OrbitError::NotElliptic)
    );
    assert_eq!(solve_kepler(1.0, 0.9999), Err(OrbitError::NearParabolic));
    assert_eq!(
        solve_kepler(1.0, 1.0 - f64::EPSILON),
        Err(OrbitError::NearParabolic)
    );
    assert!(solve_kepler(1.0, MAX_ECCENTRICITY).is_ok());
    assert!(solve_kepler(-0.0, 0.0).is_ok());
}

#[test]
fn frame_convention_matches_adr_0007() {
    let a = 1.0e7;
    let e = 0.5;
    // In the plane: periapsis on +X, prograde motion towards -Z, angular momentum +Y.
    let s = elements(a, e, 0.0, 0.0, 0.0, 0.0)
        .to_state_vectors(MU)
        .unwrap();
    assert!((s.position - DVec3::new(a * (1.0 - e), 0.0, 0.0)).length() < 1e-6);
    assert!(s.velocity.z < 0.0 && s.velocity.x.abs() < 1e-9 && s.velocity.y.abs() < 1e-9);
    assert!(s.position.cross(s.velocity).y > 0.0);

    // Polar orbit with the argument of periapsis at 90 degrees: periapsis straight up.
    let up = elements(a, e, PI / 2.0, 0.0, PI / 2.0, 0.0)
        .to_state_vectors(MU)
        .unwrap();
    assert!((up.position - DVec3::new(0.0, a * (1.0 - e), 0.0)).length() < 1e-6);

    // A retrograde equatorial orbit has its angular momentum along -Y.
    let retro = elements(a, e, PI, 0.0, 0.0, 1.0)
        .to_state_vectors(MU)
        .unwrap();
    assert!(retro.position.cross(retro.velocity).y < 0.0);
    assert!(retro.position.y.abs() < 1e-6);

    // Longitude of the ascending node turns the periapsis from +X towards -Z.
    let turned = elements(a, e, 0.0, PI / 2.0, 0.0, 0.0)
        .to_state_vectors(MU)
        .unwrap();
    assert!((turned.position - DVec3::new(0.0, 0.0, -a * (1.0 - e))).length() < 1e-6);
}

#[test]
fn circular_orbit_has_constant_radius_and_circular_speed() {
    let a = 7.0e6;
    let orbit = elements(a, 0.0, 0.3, 1.0, 0.0, 0.0);
    let speed = (MU / a).sqrt();
    for k in 0..50 {
        let s = orbit.state_after(MU, 137.0 * k as f64).unwrap();
        assert!((s.position.length() - a).abs() < 1e-6, "k={k}");
        assert!((s.velocity.length() - speed).abs() < 1e-9, "k={k}");
        assert!(s.position.dot(s.velocity).abs() < 1e-3);
    }
}

#[test]
fn energy_and_angular_momentum_are_conserved() {
    let mut rng = Rng(2);
    for _ in 0..300 {
        let orbit = elements(
            rng.range(6.6e6, 4.0e7),
            rng.range(0.0, 0.95),
            rng.range(0.0, PI),
            rng.range(0.0, TAU),
            rng.range(0.0, TAU),
            rng.range(0.0, TAU),
        );
        let s0 = orbit.state_after(MU, 0.0).unwrap();
        let energy =
            |s: &StateVectors| 0.5 * s.velocity.length_squared() - MU / s.position.length();
        let e0 = energy(&s0);
        let h0 = s0.position.cross(s0.velocity);
        // Vis-viva: the energy is -mu / (2a).
        assert!((e0 + MU / (2.0 * orbit.semi_major_axis)).abs() < 1e-9 * e0.abs());
        for step in 1..20 {
            let s = orbit.state_after(MU, 1000.0 * step as f64).unwrap();
            assert!((energy(&s) - e0).abs() < 1e-9 * e0.abs());
            assert!((s.position.cross(s.velocity) - h0).length() < 1e-9 * h0.length());
        }
    }
}

#[test]
fn state_returns_after_one_period() {
    let mut rng = Rng(3);
    for _ in 0..200 {
        let a = rng.range(6.6e6, 4.0e7);
        let orbit = elements(
            a,
            rng.range(0.0, 0.9),
            rng.range(0.0, PI),
            rng.range(0.0, TAU),
            rng.range(0.0, TAU),
            rng.range(0.0, TAU),
        );
        let period = orbit.period(MU).unwrap();
        let s0 = orbit.state_after(MU, 0.0).unwrap();
        let s1 = orbit.state_after(MU, period).unwrap();
        let s2 = orbit.state_after(MU, -period).unwrap();
        assert!((s1.position - s0.position).length() < 1e-8 * a);
        assert!((s2.position - s0.position).length() < 1e-8 * a);
        assert!((s1.velocity - s0.velocity).length() < 1e-8 * s0.velocity.length());
    }
}

#[test]
fn elements_state_round_trip() {
    let mut rng = Rng(4);
    for _ in 0..2000 {
        let orbit = elements(
            rng.range(6.6e6, 1.0e9),
            rng.range(0.01, 0.9),
            rng.range(0.05, PI - 0.05),
            rng.range(0.0, TAU),
            rng.range(0.0, TAU),
            rng.range(0.0, TAU),
        );
        let s = orbit.to_state_vectors(MU).unwrap();
        let back = Elements::from_state_vectors(&s, MU).unwrap();
        assert!((back.semi_major_axis / orbit.semi_major_axis - 1.0).abs() < 1e-9);
        assert!((back.eccentricity - orbit.eccentricity).abs() < 1e-9);
        assert!((back.inclination - orbit.inclination).abs() < 1e-9);
        assert!(
            angle_gap(
                back.longitude_of_ascending_node,
                orbit.longitude_of_ascending_node
            ) < 1e-9
        );
        assert!(angle_gap(back.argument_of_periapsis, orbit.argument_of_periapsis) < 1e-8);
        assert!(angle_gap(back.mean_anomaly, orbit.mean_anomaly) < 1e-8);
        // And the recovered elements regenerate the same state.
        let again = back.to_state_vectors(MU).unwrap();
        assert!((again.position - s.position).length() < 1e-8 * orbit.semi_major_axis);
        assert!((again.velocity - s.velocity).length() < 1e-8 * s.velocity.length());
    }
}

#[test]
fn degenerate_orbits_round_trip_through_their_conventions() {
    let a = 8.0e6;
    let cases = [
        // circular and inclined
        elements(a, 0.0, 0.7, 1.1, 0.0, 2.0),
        // circular and equatorial, prograde and retrograde
        elements(a, 0.0, 0.0, 0.0, 0.0, 2.0),
        elements(a, 0.0, PI, 0.0, 0.0, 2.0),
        // eccentric and equatorial, prograde and retrograde
        elements(a, 0.4, 0.0, 0.0, 1.3, 2.0),
        elements(a, 0.4, PI, 0.0, 1.3, 2.0),
        // polar
        elements(a, 0.3, PI / 2.0, 0.5, 0.6, 0.7),
    ];
    for orbit in cases {
        let s = orbit.to_state_vectors(MU).unwrap();
        let back = Elements::from_state_vectors(&s, MU).unwrap();
        let again = back.to_state_vectors(MU).unwrap();
        assert!(
            (again.position - s.position).length() < 1e-6,
            "{orbit:?} -> {back:?}"
        );
        assert!(
            (again.velocity - s.velocity).length() < 1e-9,
            "{orbit:?} -> {back:?}"
        );
        assert!((back.semi_major_axis - a).abs() < 1e-5);
    }
    // Conventions: equatorial gives raan 0 and inclination exactly 0 or pi; circular gives argp 0.
    let eq = Elements::from_state_vectors(
        &elements(a, 0.0, 0.0, 0.5, 0.0, 1.0)
            .to_state_vectors(MU)
            .unwrap(),
        MU,
    )
    .unwrap();
    assert_eq!(
        (
            eq.inclination,
            eq.longitude_of_ascending_node,
            eq.argument_of_periapsis
        ),
        (0.0, 0.0, 0.0)
    );
    let retro = Elements::from_state_vectors(
        &elements(a, 0.2, PI, 0.5, 0.0, 1.0)
            .to_state_vectors(MU)
            .unwrap(),
        MU,
    )
    .unwrap();
    assert_eq!(
        (retro.inclination, retro.longitude_of_ascending_node),
        (PI, 0.0)
    );
}

#[test]
fn propagate_matches_state_after() {
    let orbit = elements(2.0e7, 0.3, 0.4, 0.5, 0.6, 0.7);
    let s = orbit.to_state_vectors(MU).unwrap();
    let moved = s.propagate(MU, 3600.0).unwrap();
    let direct = orbit.state_after(MU, 3600.0).unwrap();
    assert!((moved.position - direct.position).length() < 1e-5);
    assert!((moved.velocity - direct.velocity).length() < 1e-8);
}

#[test]
fn bad_gravitational_parameter_is_rejected_everywhere() {
    let orbit = elements(1.0e7, 0.1, 0.0, 0.0, 0.0, 0.0);
    let state = orbit.to_state_vectors(MU).unwrap();
    for mu in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0] {
        assert_eq!(
            orbit.mean_motion(mu),
            Err(OrbitError::InvalidGravitationalParameter)
        );
        assert_eq!(
            orbit.period(mu),
            Err(OrbitError::InvalidGravitationalParameter)
        );
        assert_eq!(
            orbit.advanced(mu, 1.0),
            Err(OrbitError::InvalidGravitationalParameter)
        );
        assert_eq!(
            orbit.to_state_vectors(mu),
            Err(OrbitError::InvalidGravitationalParameter)
        );
        assert_eq!(
            orbit.state_after(mu, 1.0),
            Err(OrbitError::InvalidGravitationalParameter)
        );
        assert_eq!(
            Elements::from_state_vectors(&state, mu),
            Err(OrbitError::InvalidGravitationalParameter)
        );
        assert_eq!(
            state.propagate(mu, 1.0),
            Err(OrbitError::InvalidGravitationalParameter)
        );
    }
}

#[test]
fn bad_elements_are_rejected() {
    let good = elements(1.0e7, 0.1, 0.2, 0.3, 0.4, 0.5);
    assert_eq!(good.validate(), Ok(()));
    let with = |f: &dyn Fn(&mut Elements)| {
        let mut e = good;
        f(&mut e);
        e
    };
    assert_eq!(
        with(&|e| e.semi_major_axis = 0.0).validate(),
        Err(OrbitError::InvalidSemiMajorAxis)
    );
    assert_eq!(
        with(&|e| e.semi_major_axis = -5.0).validate(),
        Err(OrbitError::InvalidSemiMajorAxis)
    );
    assert_eq!(
        with(&|e| e.semi_major_axis = f64::INFINITY).validate(),
        Err(OrbitError::InvalidSemiMajorAxis)
    );
    assert_eq!(
        with(&|e| e.semi_major_axis = f64::NAN).validate(),
        Err(OrbitError::NonFinite)
    );
    assert_eq!(
        with(&|e| e.eccentricity = -0.5).validate(),
        Err(OrbitError::InvalidEccentricity)
    );
    assert_eq!(
        with(&|e| e.eccentricity = 1.0).validate(),
        Err(OrbitError::NotElliptic)
    );
    assert_eq!(
        with(&|e| e.eccentricity = 2.0).validate(),
        Err(OrbitError::NotElliptic)
    );
    assert_eq!(
        with(&|e| e.eccentricity = 0.9995).validate(),
        Err(OrbitError::NearParabolic)
    );
    assert_eq!(
        with(&|e| e.eccentricity = f64::NAN).validate(),
        Err(OrbitError::NonFinite)
    );
    assert_eq!(
        with(&|e| e.inclination = f64::NAN).validate(),
        Err(OrbitError::NonFinite)
    );
    assert_eq!(
        with(&|e| e.longitude_of_ascending_node = f64::INFINITY).validate(),
        Err(OrbitError::NonFinite)
    );
    assert_eq!(
        with(&|e| e.argument_of_periapsis = f64::NAN).validate(),
        Err(OrbitError::NonFinite)
    );
    assert_eq!(
        with(&|e| e.mean_anomaly = f64::NEG_INFINITY).validate(),
        Err(OrbitError::NonFinite)
    );
    // Every consumer validates first.
    assert!(
        with(&|e| e.eccentricity = 1.5)
            .to_state_vectors(MU)
            .is_err()
    );
    assert!(with(&|e| e.semi_major_axis = 0.0).mean_motion(MU).is_err());
    // `a^3` overflows and `mu / a^3` underflows to zero: not a usable mean motion.
    assert_eq!(
        with(&|e| e.semi_major_axis = 1.0e200).mean_motion(MU),
        Err(OrbitError::NonFinite)
    );
    // Time that is not a number, or overflows the mean anomaly.
    assert_eq!(good.advanced(MU, f64::NAN), Err(OrbitError::NonFinite));
    assert_eq!(good.advanced(MU, f64::INFINITY), Err(OrbitError::NonFinite));
    assert_eq!(
        good.state_after(MU, f64::NEG_INFINITY),
        Err(OrbitError::NonFinite)
    );
}

#[test]
fn bad_state_vectors_are_rejected() {
    let p = DVec3::new(7.0e6, 0.0, 0.0);
    let v = DVec3::new(0.0, 0.0, -7500.0);
    let ok = StateVectors {
        position: p,
        velocity: v,
    };
    assert!(Elements::from_state_vectors(&ok, MU).is_ok());
    let check = |position, velocity, want| {
        assert_eq!(
            Elements::from_state_vectors(&StateVectors { position, velocity }, MU),
            Err(want)
        );
    };
    check(DVec3::NAN, v, OrbitError::NonFinite);
    check(p, DVec3::new(f64::NAN, 0.0, 0.0), OrbitError::NonFinite);
    check(
        DVec3::new(f64::INFINITY, 0.0, 0.0),
        v,
        OrbitError::NonFinite,
    );
    check(DVec3::ZERO, v, OrbitError::DegenerateState);
    // Zero velocity and a purely radial velocity have no angular momentum.
    check(p, DVec3::ZERO, OrbitError::DegenerateState);
    check(p, DVec3::new(100.0, 0.0, 0.0), OrbitError::DegenerateState);
    // Escape speed (parabola) and above (hyperbola).
    let escape = (2.0 * MU / 7.0e6).sqrt();
    check(p, DVec3::new(0.0, 0.0, -escape), OrbitError::NotElliptic);
    check(
        p,
        DVec3::new(0.0, 0.0, -2.0 * escape),
        OrbitError::NotElliptic,
    );
    // Bound, but with an eccentricity above the supported maximum: at apoapsis of a very
    // long ellipse, speed just below the parabola would give e > 0.999.
    let nearly_escape = escape * (1.0 - 1.0e-7);
    check(
        p,
        DVec3::new(0.0, 0.0, -nearly_escape),
        OrbitError::NearParabolic,
    );
    // Overflow in the intermediate squares is reported, not propagated.
    check(
        DVec3::new(1.0e200, 0.0, 0.0),
        DVec3::new(0.0, 0.0, 1.0e200),
        OrbitError::NonFinite,
    );
}

#[test]
fn no_input_panics() {
    let specials = [
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        1e-300,
        1e300,
        -1e300,
        f64::MAX,
        f64::MIN_POSITIVE,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];
    for &a in &specials {
        for &e in &specials {
            for &x in &specials {
                let orbit = elements(a, e, x, x, x, x);
                for &mu in &specials {
                    let _ = orbit.validate();
                    let _ = orbit.mean_motion(mu);
                    let _ = orbit.period(mu);
                    let _ = orbit.advanced(mu, x);
                    let _ = orbit.to_state_vectors(mu);
                    let _ = orbit.state_after(mu, x);
                    let s = StateVectors {
                        position: DVec3::new(a, e, x),
                        velocity: DVec3::new(x, a, e),
                    };
                    let _ = Elements::from_state_vectors(&s, mu);
                    let _ = s.propagate(mu, x);
                }
                let _ = solve_kepler(x, e);
            }
        }
    }
}

#[test]
fn errors_have_messages() {
    let all = [
        OrbitError::NonFinite,
        OrbitError::InvalidGravitationalParameter,
        OrbitError::InvalidSemiMajorAxis,
        OrbitError::InvalidEccentricity,
        OrbitError::NotElliptic,
        OrbitError::NearParabolic,
        OrbitError::DegenerateState,
    ];
    for err in all {
        assert!(!err.to_string().is_empty());
    }
}

/// A test-local copy of the `solve_kepler` loop with a chosen number of steps, used only to
/// show how many steps are needed. `kepler_residual_grid` checks that the copy with
/// `KEPLER_ITERATIONS` steps equals the real function bit for bit, so the copy cannot drift.
fn kepler_with_steps(mean_anomaly: f64, e: f64, steps: u32) -> f64 {
    let m_wrapped = mean_anomaly.rem_euclid(TAU);
    let mirrored = m_wrapped > PI;
    let m = if mirrored { TAU - m_wrapped } else { m_wrapped };
    let mut big_e = det::min(m + e, PI);
    for _ in 0..steps {
        let (s, c) = det::sin_cos(big_e);
        big_e -= (big_e - e * s - m) / (1.0 - e * c);
    }
    let big_e = det::clamp(big_e, 0.0, PI);
    if mirrored { TAU - big_e } else { big_e }
}

/// Residual `|E - e sin E - M|` of a candidate solution, against `M` reduced modulo a full turn
/// the way the solver reduces it (`rem_euclid` with `TAU`). The reduction itself is not part of
/// the residual: `TAU` is `2*pi` rounded, so a mean anomaly many turns from zero is off by
/// about `2.4e-16` radians per turn before the solver sees it.
fn kepler_residual(big_e: f64, e: f64, m: f64) -> f64 {
    angle_gap(big_e - e * det::sin(big_e), m.rem_euclid(TAU))
}

/// The bound quoted in the documentation of `solve_kepler`.
const KEPLER_RESIDUAL_BOUND: f64 = 1e-14;

/// The deterministic grid behind the convergence statement in the `solve_kepler`
/// documentation: nine eccentricities from 0 to 0.999 (both included), 4,001 evenly spaced
/// mean anomalies over `[0, pi]` and their mirror images over `[pi, 2*pi]`, plus corner
/// values: tiny (down to 1e-30), zero, around `pi`, around `2*pi`, and negative.
#[test]
fn kepler_residual_grid() {
    let eccentricities = [0.0, 0.1, 0.5, 0.8, 0.9, 0.95, 0.99, 0.995, MAX_ECCENTRICITY];
    let mut anomalies: Vec<f64> = Vec::new();
    for k in 0..=4000 {
        let m = PI * f64::from(k) / 4000.0;
        anomalies.push(m);
        anomalies.push(TAU - m);
    }
    for tiny in [1e-30, 1e-20, 1e-12, 1e-9, 1e-6, 1e-3] {
        anomalies.extend([tiny, -tiny, TAU - tiny, TAU + tiny, PI - tiny, PI + tiny]);
    }
    anomalies.extend([0.0, -0.0, PI, TAU, -TAU, 2.0 * TAU + 1e-9, 1000.0, -1000.0]);

    let (mut worst_real, mut worst_12) = (0.0f64, 0.0f64);
    for &e in &eccentricities {
        for &m in &anomalies {
            let real = solve_kepler(m, e).unwrap();
            let at_budget = kepler_with_steps(m, e, KEPLER_ITERATIONS);
            assert_eq!(
                real.to_bits(),
                at_budget.to_bits(),
                "copy differs at M={m}, e={e}"
            );
            worst_real = det::max(worst_real, kepler_residual(real, e, m));
            worst_12 = det::max(worst_12, kepler_residual(kepler_with_steps(m, e, 12), e, m));
        }
    }
    assert!(
        worst_real < KEPLER_RESIDUAL_BOUND,
        "24 steps: worst residual {worst_real:e}"
    );
    assert!(
        worst_12 < KEPLER_RESIDUAL_BOUND,
        "12 steps: worst residual {worst_12:e}"
    );
    // The documented budget is at least the 12 steps shown to suffice.
    const { assert!(KEPLER_ITERATIONS >= 12) };
}
