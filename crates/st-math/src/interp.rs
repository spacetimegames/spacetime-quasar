//! Deterministic interpolation: scalars, vectors, quaternions and Hermite curves.
//!
//! ADR 0006 does interpolation between two simulation ticks in `f64`, before the one
//! conversion to `f32` for rendering. These helpers are the `f64` building blocks. They use
//! only `+`, `-`, `*`, `/`, `sqrt` and functions from [`det`], and never a fused
//! multiply-add, so the same inputs give the same bit pattern on every platform.
//!
//! None of the functions panics. The parameter `t` is *not* clamped: `0` gives the first
//! value, `1` the second, and values outside `[0, 1]` extrapolate. Clamp with
//! [`det::clamp`] first when that is not wanted. A `NaN` input gives a
//! `NaN` result for the scalar and vector functions; the quaternion functions document
//! their fallback.
//!
//! ```
//! use st_math::{glam::{DQuat, DVec3}, interp};
//!
//! let a = DVec3::new(0.0, 0.0, 0.0);
//! let b = DVec3::new(10.0, 0.0, -4.0);
//! assert_eq!(interp::lerp_vec3(a, b, 0.5), DVec3::new(5.0, 0.0, -2.0));
//!
//! let q = interp::slerp(DQuat::IDENTITY, DQuat::from_rotation_y(1.0), 0.5);
//! assert!((q.length() - 1.0).abs() < 1e-15);
//! ```

use glam::{DQuat, DVec3};

use crate::det;

/// `|dot|` above which [`slerp`] falls back to [`nlerp`]: `0.9995`, an angle of about
/// 1.8 degrees.
///
/// Why: `sin(theta)` in the denominator of the slerp weights vanishes as the angle goes to
/// zero, and for such small angles a normalised linear blend differs from the true arc by
/// less than `1e-7` of the angle, while the division would lose precision.
const SLERP_LINEAR_THRESHOLD: f64 = 0.9995;

/// Linear interpolation `a * (1 - t) + b * t`.
///
/// This form returns exactly `a` for `t = 0` and exactly `b` for `t = 1` (for finite
/// inputs), unlike `a + (b - a) * t`, which can miss `b` by rounding.
///
/// ```
/// use st_math::interp::lerp;
///
/// assert_eq!(lerp(2.0, 6.0, 0.25), 3.0);
/// assert_eq!(lerp(0.1, 0.3, 0.0), 0.1);
/// assert_eq!(lerp(0.1, 0.3, 1.0), 0.3);
/// assert_eq!(lerp(0.0, 1.0, 2.0), 2.0); // extrapolates
/// ```
#[inline]
#[must_use]
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a * (1.0 - t) + b * t
}

/// Component-wise [`lerp`] of two `f64` vectors.
///
/// ```
/// use st_math::{glam::DVec3, interp::lerp_vec3};
///
/// let a = DVec3::new(1.0, 2.0, 3.0);
/// let b = DVec3::new(3.0, 2.0, -1.0);
/// assert_eq!(lerp_vec3(a, b, 0.5), DVec3::new(2.0, 2.0, 1.0));
/// assert_eq!(lerp_vec3(a, b, 0.0), a);
/// assert_eq!(lerp_vec3(a, b, 1.0), b);
/// ```
#[inline]
#[must_use]
pub fn lerp_vec3(a: DVec3, b: DVec3, t: f64) -> DVec3 {
    DVec3::new(lerp(a.x, b.x, t), lerp(a.y, b.y, t), lerp(a.z, b.z, t))
}

/// Normalises a quaternion given as four components, or returns [`DQuat::IDENTITY`] when
/// its length is zero, `NaN` or infinite.
#[inline]
fn normalize_or_identity(x: f64, y: f64, z: f64, w: f64) -> DQuat {
    let len2 = x * x + y * y + z * z + w * w;
    if len2 > 0.0 && len2.is_finite() {
        let inv = 1.0 / len2.sqrt();
        DQuat::from_xyzw(x * inv, y * inv, z * inv, w * inv)
    } else {
        DQuat::IDENTITY
    }
}

/// The dot product of two quaternions, summed in the fixed order x, y, z, w.
#[inline]
fn dot4(a: DQuat, b: DQuat) -> f64 {
    a.x * b.x + a.y * b.y + a.z * b.z + a.w * b.w
}

/// Blends `a` and `b` with weights `wa` and `wb`, after flipping `b` to the same
/// hemisphere as `a`, and normalises the result.
#[inline]
fn blend(a: DQuat, b: DQuat, wa: f64, wb: f64, flip: bool) -> DQuat {
    let wb = if flip { -wb } else { wb };
    normalize_or_identity(
        a.x * wa + b.x * wb,
        a.y * wa + b.y * wb,
        a.z * wa + b.z * wb,
        a.w * wa + b.w * wb,
    )
}

/// Normalised linear interpolation of two unit quaternions along the shorter arc.
///
/// Cheaper than [`slerp`], but the angular speed is not constant: it is fastest in the middle
/// of the arc, and the difference grows with the angle between the two rotations. Use it for small steps such as per-tick smoothing; use [`slerp`]
/// where constant angular speed matters.
///
/// With `t = 0` the result is `a` normalised, and with `t = 1` it is `b` normalised (negated
/// when `dot(a, b) < 0`). Normalising a unit quaternion can change the last bit, so the
/// endpoints are the inputs up to that rounding, not bit for bit.
///
/// `b` is negated first when `dot(a, b) < 0`, because `q` and `-q` are the same rotation.
/// The result is normalised; if the blend has zero, `NaN` or infinite length (for example
/// for `NaN` inputs, or `t = 0.5` between `q` and `-q` exactly opposite), the result is
/// [`DQuat::IDENTITY`].
///
/// ```
/// use st_math::{glam::DQuat, interp::nlerp};
///
/// let a = DQuat::IDENTITY;
/// let b = DQuat::from_rotation_z(1.0);
/// assert_eq!(nlerp(a, b, 0.0), a);
/// assert!((nlerp(a, b, 1.0).dot(b) - 1.0).abs() < 1e-15);
/// assert_eq!(nlerp(a, b, f64::NAN), DQuat::IDENTITY);
/// ```
#[inline]
#[must_use]
pub fn nlerp(a: DQuat, b: DQuat, t: f64) -> DQuat {
    let flip = dot4(a, b) < 0.0;
    blend(a, b, 1.0 - t, t, flip)
}

/// Spherical linear interpolation of two unit quaternions along the shorter arc, at
/// constant angular speed.
///
/// `b` is negated first when `dot(a, b) < 0`. When the quaternions are closer than about
/// 1.8 degrees the function uses [`nlerp`], whose error there is negligible. Otherwise the
/// weights are `sin((1 - t) * theta) / sin(theta)` and `sin(t * theta) / sin(theta)` with
/// `theta = acos(|dot|)`, evaluated with [`det`]. The result is normalised with
/// the same fallback as [`nlerp`]: [`DQuat::IDENTITY`] for a degenerate blend or `NaN`
/// input.
///
/// At `t = 0` and `t = 1` the result is the first or second rotation up to rounding (it can
/// differ from a unit input in the last bit, and `b` can come back negated), not bit for bit.
///
/// ```
/// use st_math::{glam::DQuat, interp::slerp};
///
/// let a = DQuat::IDENTITY;
/// let b = DQuat::from_rotation_y(1.0);
/// let half = slerp(a, b, 0.5);
/// // Halfway along the arc is half the angle.
/// assert!((half.dot(DQuat::from_rotation_y(0.5)).abs() - 1.0).abs() < 1e-14);
/// assert!((slerp(a, b, 1.0).dot(b) - 1.0).abs() < 1e-14);
/// ```
#[must_use]
pub fn slerp(a: DQuat, b: DQuat, t: f64) -> DQuat {
    let d = dot4(a, b);
    let flip = d < 0.0;
    let cos_theta = if flip { -d } else { d };
    if cos_theta > SLERP_LINEAR_THRESHOLD || cos_theta.is_nan() {
        return blend(a, b, 1.0 - t, t, flip);
    }
    let theta = det::acos(det::clamp(cos_theta, -1.0, 1.0));
    let sin_theta = det::sin(theta);
    if sin_theta.is_nan() || sin_theta <= 0.0 {
        // `theta` is 0 or pi (or not a number): no unique arc, so fall back to the blend.
        return blend(a, b, 1.0 - t, t, flip);
    }
    let wa = det::sin((1.0 - t) * theta) / sin_theta;
    let wb = det::sin(t * theta) / sin_theta;
    blend(a, b, wa, wb, flip)
}

/// Cubic Hermite interpolation of a position from two positions and two velocities.
///
/// The curve runs over `duration` seconds from `p0` (velocity `v0`) to `p1` (velocity
/// `v1`); `t` in `[0, 1]` is the fraction of the duration elapsed. With `t = 0` the result
/// is exactly `p0` and with `t = 1` exactly `p1`. The tangents are `v0 * duration` and
/// `v1 * duration`, which makes the curve's velocity match `v0` and `v1` at the ends.
/// Use it to draw a body between two simulation ticks with the tick's own velocities, so the
/// path has no corners. `duration` only scales the tangents, so a zero or negative duration
/// gives a valid curve (a straight blend for zero).
///
/// ```
/// use st_math::{glam::DVec3, interp::hermite};
///
/// let p0 = DVec3::new(0.0, 0.0, 0.0);
/// let p1 = DVec3::new(10.0, 0.0, 0.0);
/// // Constant velocity of 10 m/s over one second reproduces the straight line.
/// let v = DVec3::new(10.0, 0.0, 0.0);
/// assert_eq!(hermite(p0, v, p1, v, 1.0, 0.5), DVec3::new(5.0, 0.0, 0.0));
/// assert_eq!(hermite(p0, v, p1, v, 1.0, 0.0), p0);
/// assert_eq!(hermite(p0, v, p1, v, 1.0, 1.0), p1);
/// ```
#[must_use]
pub fn hermite(p0: DVec3, v0: DVec3, p1: DVec3, v1: DVec3, duration: f64, t: f64) -> DVec3 {
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    let m0 = v0 * duration;
    let m1 = v1 * duration;
    p0 * h00 + m0 * h10 + p1 * h01 + m1 * h11
}

/// The velocity of the [`hermite`] curve at fraction `t`, in metres per second.
///
/// This is the derivative of the position with respect to time, so it divides by
/// `duration`. At `t = 0` it is `v0` and at `t = 1` it is `v1` (up to rounding). Returns
/// `None` when `duration` is zero, negative, `NaN` or infinite, because the velocity is not
/// defined then.
///
/// ```
/// use st_math::{glam::DVec3, interp::hermite_velocity};
///
/// let p0 = DVec3::ZERO;
/// let p1 = DVec3::new(10.0, 0.0, 0.0);
/// let v = DVec3::new(10.0, 0.0, 0.0);
/// let mid = hermite_velocity(p0, v, p1, v, 1.0, 0.5).unwrap();
/// assert!((mid - v).length() < 1e-12);
/// assert_eq!(hermite_velocity(p0, v, p1, v, 0.0, 0.5), None);
/// ```
#[must_use]
pub fn hermite_velocity(
    p0: DVec3,
    v0: DVec3,
    p1: DVec3,
    v1: DVec3,
    duration: f64,
    t: f64,
) -> Option<DVec3> {
    if !(duration > 0.0 && duration.is_finite()) {
        return None;
    }
    let t2 = t * t;
    let d00 = 6.0 * t2 - 6.0 * t;
    let d10 = 3.0 * t2 - 4.0 * t + 1.0;
    let d01 = -6.0 * t2 + 6.0 * t;
    let d11 = 3.0 * t2 - 2.0 * t;
    let m0 = v0 * duration;
    let m1 = v1 * duration;
    Some((p0 * d00 + m0 * d10 + p1 * d01 + m1 * d11) / duration)
}
