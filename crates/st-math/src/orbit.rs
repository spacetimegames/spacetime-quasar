//! Deterministic Keplerian orbits: elements, state vectors and propagation.
//!
//! An *on-rails* body follows a fixed ellipse around its parent. This module converts
//! between orbital elements and state vectors (position and velocity in `f64` metres and
//! metres per second), and advances an orbit through time with a Kepler-equation solver.
//! Every transcendental call goes through [`det`], and no expression uses a
//! fused multiply-add, so a result is the same bit pattern on every platform (ADR 0008).
//!
//! # Frame convention
//!
//! ADR 0007 fixes right-handed frames with +Y up and the gameplay plane in XZ. The orbit
//! reference plane is therefore the XZ plane and its normal is +Y:
//!
//! * inclination `0` is an orbit in the XZ plane, travelled counter-clockwise when seen
//!   from +Y looking down (so a prograde orbit leaves +X towards -Z);
//! * the reference direction, from which the longitude of the ascending node is measured,
//!   is +X;
//! * the angular momentum of an orbit with inclination `0` points along +Y.
//!
//! Internally the maths uses the textbook frame (reference plane XY, normal Z). The
//! textbook vector `(x, y, z)` is the world vector `(x, z, -y)`. That map is a rotation
//! (determinant +1), so handedness is preserved, and it only permutes components and
//! negates one, which is exact.
//!
//! # Scope
//!
//! * Elliptic orbits with an eccentricity up to [`MAX_ECCENTRICITY`]. Parabolic and
//!   hyperbolic orbits are rejected with [`OrbitError::NotElliptic`].
//! * Two-body motion about a fixed centre with gravitational parameter `mu` in m^3/s^2.
//!   Perturbations and moving parents are the caller's concern: a moon's state is added to
//!   its parent's state by the caller.
//! * Angles are radians. Elements keep the mean anomaly at the reference time, so a body is
//!   placed at any time by [`Elements::state_after`] without accumulating error from earlier
//!   steps.
//!
//! # Determinism and cost
//!
//! The Kepler solver performs exactly [`KEPLER_ITERATIONS`] Newton steps for every input.
//! There is no early exit and no convergence test, so the sequence of floating-point
//! operations depends only on the iteration budget. Every function returns a [`Result`];
//! none panics.
//!
//! ```
//! use st_math::orbit::{Elements, StateVectors};
//!
//! // A circular orbit of radius 1e7 m around a body with mu = 4e14 m^3/s^2.
//! let mu = 4.0e14;
//! let orbit = Elements {
//!     semi_major_axis: 1.0e7,
//!     eccentricity: 0.0,
//!     inclination: 0.0,
//!     longitude_of_ascending_node: 0.0,
//!     argument_of_periapsis: 0.0,
//!     mean_anomaly: 0.0,
//! };
//! let state: StateVectors = orbit.state_after(mu, 0.0)?;
//! assert!((state.position.length() - 1.0e7).abs() < 1e-6);
//! // After one period the body is back where it started.
//! let period = orbit.period(mu)?;
//! let again = orbit.state_after(mu, period)?;
//! assert!((again.position - state.position).length() < 1e-3);
//! # Ok::<(), st_math::orbit::OrbitError>(())
//! ```

use core::f64::consts::{PI, TAU};
use core::fmt;

use glam::DVec3;

use crate::det;

/// The largest eccentricity accepted by this module: `0.999`.
///
/// Why a limit below `1`: as the eccentricity approaches `1` the Kepler equation becomes
/// ill-conditioned (the speed at periapsis diverges and `1 - e` shrinks towards the
/// rounding noise of an `f64` state vector), and an orbit that close to parabolic is
/// indistinguishable from an escape trajectory in a state computed by the simulation. The
/// accuracy statement of [`solve_kepler`] holds up to this value.
pub const MAX_ECCENTRICITY: f64 = 0.999;

/// Number of Newton steps [`solve_kepler`] always performs: `24`.
///
/// The convergence bound is documented on [`solve_kepler`].
pub const KEPLER_ITERATIONS: u32 = 24;

/// Threshold below which an orbit is treated as circular when elements are derived from a
/// state vector: eccentricity `1e-11`.
///
/// A circular orbit has no periapsis, so its argument of periapsis is set to `0` and the
/// anomaly is measured from the ascending node.
const CIRCULAR_EPSILON: f64 = 1.0e-11;

/// Threshold below which an orbit is treated as equatorial when elements are derived from
/// a state vector: `sin(inclination)` of `1e-11`.
///
/// An equatorial orbit has no ascending node, so the longitude of the ascending node is set
/// to `0` and the inclination to exactly `0` or `pi`.
const EQUATORIAL_EPSILON: f64 = 1.0e-11;

/// Why an orbit computation was refused.
///
/// Every variant describes input the module cannot give a meaningful answer for. The
/// functions in this module never panic and never return a `NaN` that is not signalled by
/// one of these.
///
/// ```
/// use st_math::orbit::{solve_kepler, OrbitError};
///
/// assert_eq!(solve_kepler(f64::NAN, 0.5), Err(OrbitError::NonFinite));
/// assert_eq!(solve_kepler(1.0, 1.0), Err(OrbitError::NotElliptic));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum OrbitError {
    /// An input, or an intermediate result, is `NaN`, infinite or not representable.
    NonFinite,
    /// The gravitational parameter is zero, negative, `NaN` or infinite.
    InvalidGravitationalParameter,
    /// The semi-major axis is zero or negative.
    InvalidSemiMajorAxis,
    /// The eccentricity is negative.
    InvalidEccentricity,
    /// The orbit is not an ellipse: eccentricity `>= 1`, or a state with non-negative
    /// orbital energy.
    NotElliptic,
    /// The eccentricity is an ellipse's but above [`MAX_ECCENTRICITY`].
    NearParabolic,
    /// The state vector has no orbit: the position is zero or the angular momentum is zero
    /// (a straight-line fall).
    DegenerateState,
}

impl fmt::Display for OrbitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::NonFinite => "a value is not finite or not representable",
            Self::InvalidGravitationalParameter => {
                "the gravitational parameter must be finite and positive"
            }
            Self::InvalidSemiMajorAxis => "the semi-major axis must be finite and positive",
            Self::InvalidEccentricity => "the eccentricity must not be negative",
            Self::NotElliptic => "the orbit is not an ellipse",
            Self::NearParabolic => "the eccentricity is above the supported maximum",
            Self::DegenerateState => "the state vector has no orbit",
        };
        f.write_str(text)
    }
}

impl core::error::Error for OrbitError {}

/// Position and velocity relative to the orbited body, in the world frame of ADR 0007.
///
/// ```
/// use st_math::{glam::DVec3, orbit::StateVectors};
///
/// let s = StateVectors { position: DVec3::X, velocity: DVec3::Y };
/// assert_eq!(s.position.x, 1.0);
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StateVectors {
    /// Position in metres.
    pub position: DVec3,
    /// Velocity in metres per second.
    pub velocity: DVec3,
}

/// Classical orbital elements of an elliptic orbit.
///
/// The fields are public so that data files can fill them directly; every function that
/// takes an `Elements` validates it first (see [`Elements::validate`]). Angles are in
/// radians and may be any finite value: they are wrapped where it matters.
///
/// ```
/// use st_math::orbit::Elements;
///
/// let e = Elements {
///     semi_major_axis: 1.5e11,
///     eccentricity: 0.0167,
///     inclination: 0.0,
///     longitude_of_ascending_node: 0.0,
///     argument_of_periapsis: 1.8,
///     mean_anomaly: 0.5,
/// };
/// assert!(e.validate().is_ok());
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Elements {
    /// Semi-major axis `a` in metres. Must be finite and positive.
    pub semi_major_axis: f64,
    /// Eccentricity `e`, in `[0, MAX_ECCENTRICITY]`.
    pub eccentricity: f64,
    /// Inclination `i` of the orbit plane against the XZ plane, in radians.
    pub inclination: f64,
    /// Longitude of the ascending node, in radians from +X about +Y.
    pub longitude_of_ascending_node: f64,
    /// Argument of periapsis, in radians from the ascending node along the orbit.
    pub argument_of_periapsis: f64,
    /// Mean anomaly at the reference time, in radians.
    pub mean_anomaly: f64,
}

/// Validates a gravitational parameter: finite and strictly positive.
fn check_mu(mu: f64) -> Result<(), OrbitError> {
    if mu.is_finite() && mu > 0.0 {
        Ok(())
    } else {
        Err(OrbitError::InvalidGravitationalParameter)
    }
}

/// Validates an eccentricity against the supported range `[0, MAX_ECCENTRICITY]`.
fn check_eccentricity(e: f64) -> Result<(), OrbitError> {
    if !e.is_finite() {
        // An infinite eccentricity is not an ellipse; `NaN` is reported as not finite.
        return if e.is_nan() {
            Err(OrbitError::NonFinite)
        } else if e > 0.0 {
            Err(OrbitError::NotElliptic)
        } else {
            Err(OrbitError::InvalidEccentricity)
        };
    }
    if e < 0.0 {
        Err(OrbitError::InvalidEccentricity)
    } else if e >= 1.0 {
        Err(OrbitError::NotElliptic)
    } else if e > MAX_ECCENTRICITY {
        Err(OrbitError::NearParabolic)
    } else {
        Ok(())
    }
}

/// Square root, correctly rounded on every target (ADR 0008 keeps `sqrt` allowed).
#[inline]
fn sqrt_of(x: f64) -> f64 {
    x.sqrt()
}

/// Wraps an angle to `[0, 2*pi)` (up to the rounding of the last bit, which can give
/// exactly `2*pi` for a tiny negative input).
#[inline]
fn wrap_tau(x: f64) -> f64 {
    x.rem_euclid(TAU)
}

/// Maps a textbook-frame vector (reference plane XY, normal Z) to the world frame.
#[inline]
fn to_world(v: DVec3) -> DVec3 {
    DVec3::new(v.x, v.z, -v.y)
}

/// Maps a world-frame vector to the textbook frame. Inverse of [`to_world`].
#[inline]
fn from_world(v: DVec3) -> DVec3 {
    DVec3::new(v.x, -v.z, v.y)
}

/// Solves Kepler's equation `E - e sin E = M` for the eccentric anomaly `E`.
///
/// `mean_anomaly` is in radians and may be any finite value; `eccentricity` must be in
/// `[0, MAX_ECCENTRICITY]`. The result is in `[0, 2*pi]`.
///
/// # Algorithm and convergence bound
///
/// The mean anomaly is reduced to `[0, pi]` by symmetry. On that interval
/// `f(E) = E - e sin E - M` is increasing and convex, and the starting point
/// `min(M + e, pi)` has `f >= 0`, so Newton's method converges monotonically from the
/// right and cannot leave the interval. The solver always performs exactly
/// [`KEPLER_ITERATIONS`] Newton steps: no tolerance test, no early exit, so the number and
/// order of floating-point operations never depends on the data.
///
/// Convergence bound: Newton's method is quadratic near the root, and the start is within
/// `e` of it. Measured with a copy of this loop on a grid of 4,001 mean anomalies in
/// `[0, pi]` plus values down to `1e-30`, for nine eccentricities from `0` to `0.999`, the
/// residual `|E - e sin E - M|` was `4.4e-16` (rounding noise) after 12 steps and not larger
/// after more, while 8 steps left `6.7e-6`. The budget of 24 is twice the steps that grid
/// needed. `tests/orbit.rs` checks a residual below `2e-15` on pseudo-random inputs with
/// the real function. This is an empirical statement over those inputs, not a proof for
/// every `f64`.
///
/// # Errors
///
/// [`OrbitError::NonFinite`] for a `NaN` or infinite input, [`OrbitError::InvalidEccentricity`]
/// for a negative eccentricity, [`OrbitError::NotElliptic`] for `e >= 1` and
/// [`OrbitError::NearParabolic`] for `MAX_ECCENTRICITY < e < 1`.
///
/// ```
/// use st_math::orbit::solve_kepler;
///
/// // A circular orbit: E equals M.
/// assert_eq!(solve_kepler(1.25, 0.0)?, 1.25);
/// // Check the defining equation for an eccentric orbit.
/// let (m, e) = (2.0, 0.6);
/// let big_e = solve_kepler(m, e)?;
/// assert!((big_e - e * st_math::det::sin(big_e) - m).abs() < 1e-14);
/// // Bad input is an error, not a panic.
/// assert!(solve_kepler(0.5, 1.5).is_err());
/// assert!(solve_kepler(f64::INFINITY, 0.5).is_err());
/// # Ok::<(), st_math::orbit::OrbitError>(())
/// ```
pub fn solve_kepler(mean_anomaly: f64, eccentricity: f64) -> Result<f64, OrbitError> {
    check_eccentricity(eccentricity)?;
    if !mean_anomaly.is_finite() {
        return Err(OrbitError::NonFinite);
    }
    let e = eccentricity;
    let m_wrapped = wrap_tau(mean_anomaly);
    // Symmetry: E(2*pi - M) = 2*pi - E(M), so solve on [0, pi].
    let mirrored = m_wrapped > PI;
    let m = if mirrored { TAU - m_wrapped } else { m_wrapped };

    let mut big_e = det::min(m + e, PI);
    for _ in 0..KEPLER_ITERATIONS {
        let (s, c) = det::sin_cos(big_e);
        let f = big_e - e * s - m;
        let df = 1.0 - e * c;
        // `df >= 1 - e >= 0.001`, so the division is safe.
        big_e -= f / df;
    }
    // Rounding near the root can leave a result a few ulps outside the interval.
    let big_e = det::clamp(big_e, 0.0, PI);
    Ok(if mirrored { TAU - big_e } else { big_e })
}

impl Elements {
    /// Checks that the elements describe an elliptic orbit this module supports.
    ///
    /// The semi-major axis must be finite and positive, the eccentricity in
    /// `[0, MAX_ECCENTRICITY]`, and every angle finite.
    ///
    /// # Errors
    ///
    /// [`OrbitError::InvalidSemiMajorAxis`], [`OrbitError::InvalidEccentricity`],
    /// [`OrbitError::NotElliptic`], [`OrbitError::NearParabolic`] or
    /// [`OrbitError::NonFinite`].
    ///
    /// ```
    /// use st_math::orbit::{Elements, OrbitError};
    ///
    /// let mut e = Elements {
    ///     semi_major_axis: 1.0e7,
    ///     eccentricity: 0.1,
    ///     inclination: 0.2,
    ///     longitude_of_ascending_node: 0.3,
    ///     argument_of_periapsis: 0.4,
    ///     mean_anomaly: 0.5,
    /// };
    /// assert_eq!(e.validate(), Ok(()));
    /// e.eccentricity = 1.2;
    /// assert_eq!(e.validate(), Err(OrbitError::NotElliptic));
    /// e.eccentricity = 0.1;
    /// e.semi_major_axis = -1.0;
    /// assert_eq!(e.validate(), Err(OrbitError::InvalidSemiMajorAxis));
    /// ```
    pub fn validate(&self) -> Result<(), OrbitError> {
        let a = self.semi_major_axis;
        if a.is_nan() {
            return Err(OrbitError::NonFinite);
        }
        if !(a > 0.0 && a.is_finite()) {
            return Err(OrbitError::InvalidSemiMajorAxis);
        }
        check_eccentricity(self.eccentricity)?;
        let angles = [
            self.inclination,
            self.longitude_of_ascending_node,
            self.argument_of_periapsis,
            self.mean_anomaly,
        ];
        if angles.iter().all(|x| x.is_finite()) {
            Ok(())
        } else {
            Err(OrbitError::NonFinite)
        }
    }

    /// Mean motion `n = sqrt(mu / a^3)` in radians per second.
    ///
    /// # Errors
    ///
    /// The errors of [`Elements::validate`], [`OrbitError::InvalidGravitationalParameter`]
    /// for a bad `mu`, and [`OrbitError::NonFinite`] when `a^3` overflows or the result
    /// underflows to zero.
    ///
    /// ```
    /// use st_math::orbit::Elements;
    ///
    /// let e = Elements {
    ///     semi_major_axis: 4.0,
    ///     eccentricity: 0.0,
    ///     inclination: 0.0,
    ///     longitude_of_ascending_node: 0.0,
    ///     argument_of_periapsis: 0.0,
    ///     mean_anomaly: 0.0,
    /// };
    /// // sqrt(64 / 4^3) = 1
    /// assert_eq!(e.mean_motion(64.0)?, 1.0);
    /// assert!(e.mean_motion(0.0).is_err());
    /// # Ok::<(), st_math::orbit::OrbitError>(())
    /// ```
    pub fn mean_motion(&self, mu: f64) -> Result<f64, OrbitError> {
        check_mu(mu)?;
        self.validate()?;
        let a = self.semi_major_axis;
        let n = sqrt_of(mu / (a * a * a));
        if n.is_finite() && n > 0.0 {
            Ok(n)
        } else {
            Err(OrbitError::NonFinite)
        }
    }

    /// Orbital period `2*pi / n` in seconds.
    ///
    /// # Errors
    ///
    /// As [`Elements::mean_motion`]; also [`OrbitError::NonFinite`] if the period overflows.
    ///
    /// ```
    /// use st_math::orbit::Elements;
    ///
    /// let e = Elements {
    ///     semi_major_axis: 4.0,
    ///     eccentricity: 0.5,
    ///     inclination: 0.0,
    ///     longitude_of_ascending_node: 0.0,
    ///     argument_of_periapsis: 0.0,
    ///     mean_anomaly: 0.0,
    /// };
    /// assert_eq!(e.period(64.0)?, core::f64::consts::TAU);
    /// # Ok::<(), st_math::orbit::OrbitError>(())
    /// ```
    pub fn period(&self, mu: f64) -> Result<f64, OrbitError> {
        let p = TAU / self.mean_motion(mu)?;
        if p.is_finite() {
            Ok(p)
        } else {
            Err(OrbitError::NonFinite)
        }
    }

    /// The same orbit with the mean anomaly advanced by `dt` seconds.
    ///
    /// The new mean anomaly is `M + n * dt` wrapped to `[0, 2*pi]`. `dt` may be negative.
    /// For a body that is followed for a long time, keep the original elements and call
    /// [`Elements::state_after`] with the total elapsed time instead of chaining many small
    /// steps: it rounds once, where a chain rounds once per step.
    ///
    /// # Errors
    ///
    /// As [`Elements::mean_motion`]; [`OrbitError::NonFinite`] for a non-finite `dt` or
    /// result.
    ///
    /// ```
    /// use st_math::orbit::Elements;
    ///
    /// let e = Elements {
    ///     semi_major_axis: 4.0,
    ///     eccentricity: 0.0,
    ///     inclination: 0.0,
    ///     longitude_of_ascending_node: 0.0,
    ///     argument_of_periapsis: 0.0,
    ///     mean_anomaly: 1.0,
    /// };
    /// // n = 1 rad/s for mu = 64
    /// assert_eq!(e.advanced(64.0, 0.5)?.mean_anomaly, 1.5);
    /// assert!(e.advanced(64.0, f64::NAN).is_err());
    /// # Ok::<(), st_math::orbit::OrbitError>(())
    /// ```
    pub fn advanced(&self, mu: f64, dt: f64) -> Result<Elements, OrbitError> {
        let n = self.mean_motion(mu)?;
        let m = self.mean_anomaly + n * dt;
        if !m.is_finite() {
            return Err(OrbitError::NonFinite);
        }
        Ok(Elements {
            mean_anomaly: wrap_tau(m),
            ..*self
        })
    }

    /// Position and velocity at the reference time (the elements' own mean anomaly).
    ///
    /// The result is relative to the orbited body, in the world frame described in the
    /// [module documentation](self).
    ///
    /// # Errors
    ///
    /// The errors of [`Elements::validate`] and [`OrbitError::InvalidGravitationalParameter`];
    /// [`OrbitError::NonFinite`] if the result is not finite.
    ///
    /// ```
    /// use st_math::orbit::Elements;
    ///
    /// // Periapsis of an orbit in the XZ plane is on +X, moving towards -Z.
    /// let e = Elements {
    ///     semi_major_axis: 1.0e7,
    ///     eccentricity: 0.5,
    ///     inclination: 0.0,
    ///     longitude_of_ascending_node: 0.0,
    ///     argument_of_periapsis: 0.0,
    ///     mean_anomaly: 0.0,
    /// };
    /// let s = e.to_state_vectors(4.0e14)?;
    /// assert!((s.position.x - 5.0e6).abs() < 1e-6);
    /// assert!(s.velocity.z < 0.0);
    /// assert!(s.position.y.abs() < 1e-6 && s.velocity.y.abs() < 1e-6);
    /// # Ok::<(), st_math::orbit::OrbitError>(())
    /// ```
    pub fn to_state_vectors(&self, mu: f64) -> Result<StateVectors, OrbitError> {
        check_mu(mu)?;
        self.validate()?;
        let (a, e) = (self.semi_major_axis, self.eccentricity);
        let big_e = solve_kepler(self.mean_anomaly, e)?;
        let (sin_e, cos_e) = det::sin_cos(big_e);
        let b_over_a = sqrt_of(1.0 - e * e);
        let r = a * (1.0 - e * cos_e);
        // Perifocal position and velocity (x towards periapsis, y 90 degrees ahead).
        let px = a * (cos_e - e);
        let py = a * b_over_a * sin_e;
        let k = sqrt_of(mu * a) / r;
        let vx = -k * sin_e;
        let vy = k * b_over_a * cos_e;

        // Rotation Rz(raan) * Rx(i) * Rz(argp), written out column by column.
        let (so, co) = det::sin_cos(self.longitude_of_ascending_node);
        let (si, ci) = det::sin_cos(self.inclination);
        let (sw, cw) = det::sin_cos(self.argument_of_periapsis);
        let p_axis = DVec3::new(co * cw - so * sw * ci, so * cw + co * sw * ci, sw * si);
        let q_axis = DVec3::new(-co * sw - so * cw * ci, -so * sw + co * cw * ci, cw * si);

        let position = to_world(p_axis * px + q_axis * py);
        let velocity = to_world(p_axis * vx + q_axis * vy);
        if position.is_finite() && velocity.is_finite() {
            Ok(StateVectors { position, velocity })
        } else {
            Err(OrbitError::NonFinite)
        }
    }

    /// Position and velocity `dt` seconds after the reference time.
    ///
    /// Equivalent to [`Elements::advanced`] followed by [`Elements::to_state_vectors`].
    ///
    /// # Errors
    ///
    /// The errors of those two functions.
    ///
    /// ```
    /// use st_math::orbit::Elements;
    ///
    /// let e = Elements {
    ///     semi_major_axis: 1.0e7,
    ///     eccentricity: 0.3,
    ///     inclination: 0.4,
    ///     longitude_of_ascending_node: 0.5,
    ///     argument_of_periapsis: 0.6,
    ///     mean_anomaly: 0.7,
    /// };
    /// let mu = 4.0e14;
    /// let t = 1234.5;
    /// let direct = e.state_after(mu, t)?;
    /// let stepped = e.advanced(mu, t)?.to_state_vectors(mu)?;
    /// assert_eq!(direct, stepped);
    /// # Ok::<(), st_math::orbit::OrbitError>(())
    /// ```
    pub fn state_after(&self, mu: f64, dt: f64) -> Result<StateVectors, OrbitError> {
        self.advanced(mu, dt)?.to_state_vectors(mu)
    }

    /// Orbital elements of a state vector, with the mean anomaly at the state's instant.
    ///
    /// Degenerate cases follow fixed conventions so the function is total on valid states:
    /// an equatorial orbit (`sin i` below `1e-11`) gets longitude of ascending node `0` and
    /// inclination exactly `0` or `pi`; a circular orbit (eccentricity below `1e-11`) gets
    /// argument of periapsis `0` and the mean anomaly measured from the ascending node
    /// (from +X when also equatorial). Angles are in `[0, 2*pi]`, the inclination in
    /// `[0, pi]`.
    ///
    /// # Errors
    ///
    /// * [`OrbitError::InvalidGravitationalParameter`] for a bad `mu`;
    /// * [`OrbitError::NonFinite`] for non-finite components;
    /// * [`OrbitError::DegenerateState`] for a zero position or zero angular momentum;
    /// * [`OrbitError::NotElliptic`] for non-negative orbital energy;
    /// * [`OrbitError::NearParabolic`] above [`MAX_ECCENTRICITY`].
    ///
    /// ```
    /// use st_math::{glam::DVec3, orbit::{Elements, OrbitError, StateVectors}};
    ///
    /// let mu = 4.0e14;
    /// let original = Elements {
    ///     semi_major_axis: 1.0e7,
    ///     eccentricity: 0.2,
    ///     inclination: 0.5,
    ///     longitude_of_ascending_node: 1.0,
    ///     argument_of_periapsis: 2.0,
    ///     mean_anomaly: 3.0,
    /// };
    /// let state = original.to_state_vectors(mu)?;
    /// let back = Elements::from_state_vectors(&state, mu)?;
    /// assert!((back.semi_major_axis - 1.0e7).abs() < 1e-3);
    /// assert!((back.eccentricity - 0.2).abs() < 1e-12);
    /// assert!((back.mean_anomaly - 3.0).abs() < 1e-9);
    ///
    /// // Escape velocity is not an ellipse.
    /// let fast = StateVectors { position: DVec3::X * 1.0e7, velocity: DVec3::Z * 1.0e5 };
    /// assert_eq!(Elements::from_state_vectors(&fast, mu), Err(OrbitError::NotElliptic));
    /// # Ok::<(), OrbitError>(())
    /// ```
    pub fn from_state_vectors(state: &StateVectors, mu: f64) -> Result<Elements, OrbitError> {
        check_mu(mu)?;
        if !(state.position.is_finite() && state.velocity.is_finite()) {
            return Err(OrbitError::NonFinite);
        }
        let r = from_world(state.position);
        let v = from_world(state.velocity);
        let r_len = r.length();
        let h = r.cross(v);
        let h2 = h.dot(h);
        if !(r_len > 0.0 && h2 > 0.0) {
            return Err(OrbitError::DegenerateState);
        }
        if !(r_len.is_finite() && h2.is_finite()) {
            return Err(OrbitError::NonFinite);
        }

        let energy = 0.5 * v.dot(v) - mu / r_len;
        if !energy.is_finite() {
            return Err(OrbitError::NonFinite);
        }
        if energy >= 0.0 {
            return Err(OrbitError::NotElliptic);
        }
        let a = -mu / (2.0 * energy);
        if !(a.is_finite() && a > 0.0) {
            return Err(OrbitError::NonFinite);
        }
        let e_vec = v.cross(h) / mu - r / r_len;
        let e = e_vec.length();
        check_eccentricity(e)?;

        let h_len = sqrt_of(h2);
        let h_hat = h / h_len;
        let node_len = sqrt_of(h.x * h.x + h.y * h.y);
        let equatorial = node_len <= EQUATORIAL_EPSILON * h_len;
        let (inclination, raan, node_hat) = if equatorial {
            (if h.z > 0.0 { 0.0 } else { PI }, 0.0, DVec3::X)
        } else {
            (
                det::atan2(node_len, h.z),
                wrap_tau(det::atan2(h.x, -h.y)),
                DVec3::new(-h.y, h.x, 0.0) / node_len,
            )
        };

        let (argp, p_axis) = if e <= CIRCULAR_EPSILON {
            (0.0, node_hat)
        } else {
            let q_node = h_hat.cross(node_hat);
            (
                wrap_tau(det::atan2(e_vec.dot(q_node), e_vec.dot(node_hat))),
                e_vec / e,
            )
        };
        let q_axis = h_hat.cross(p_axis);
        let nu = det::atan2(r.dot(q_axis), r.dot(p_axis));
        // Half-angle form of the true-to-eccentric anomaly map; `cos(nu/2) >= 0`.
        let (sin_half, cos_half) = det::sin_cos(0.5 * nu);
        let big_e = 2.0 * det::atan2(sqrt_of(1.0 - e) * sin_half, sqrt_of(1.0 + e) * cos_half);
        let mean_anomaly = wrap_tau(big_e - e * det::sin(big_e));

        let elements = Elements {
            semi_major_axis: a,
            eccentricity: e,
            inclination,
            longitude_of_ascending_node: raan,
            argument_of_periapsis: argp,
            mean_anomaly,
        };
        elements.validate()?;
        Ok(elements)
    }
}

impl StateVectors {
    /// The state `dt` seconds later on the same Kepler orbit.
    ///
    /// Converts to elements, advances the mean anomaly and converts back. Each call rounds
    /// twice (once per conversion), so a body that stays on rails for a long time should
    /// keep its [`Elements`] and use [`Elements::state_after`] with the total time.
    ///
    /// # Errors
    ///
    /// The errors of [`Elements::from_state_vectors`] and [`Elements::state_after`].
    ///
    /// ```
    /// use st_math::{glam::DVec3, orbit::StateVectors};
    ///
    /// let mu: f64 = 4.0e14;
    /// let r: f64 = 7.0e6;
    /// let speed = (mu / r).sqrt();
    /// let start = StateVectors { position: DVec3::X * r, velocity: -DVec3::Z * speed };
    /// let moved = start.propagate(mu, 100.0)?;
    /// // A circular orbit keeps its radius.
    /// assert!((moved.position.length() - r).abs() < 1e-5);
    /// # Ok::<(), st_math::orbit::OrbitError>(())
    /// ```
    pub fn propagate(&self, mu: f64, dt: f64) -> Result<StateVectors, OrbitError> {
        Elements::from_state_vectors(self, mu)?.state_after(mu, dt)
    }
}
