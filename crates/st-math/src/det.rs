//! Deterministic scalar math: the only door to transcendental functions in simulation crates.
//!
//! Every function takes and returns `f64`. Transcendental functions forward to the
//! software [`libm`] crate, never to the platform's math library, so a result is the same
//! bit pattern on x86_64, aarch64 and wasm32 (ADR 0008). The remaining functions are plain
//! arithmetic whose exact definition is written in their documentation, because the
//! standard library leaves some of their edge cases (the sign of a zero result, the
//! behaviour of `NaN`) unspecified or platform-dependent.
//!
//! Why this module exists: `f64::sin` and friends call whatever libm the platform provides
//! (glibc, the Windows CRT, Apple's libm, the wasm host), and those differ in the last bit.
//! The repository's `clippy.toml` bans those methods in simulation crates and points here.
//!
//! None of the functions panics, for any input including `NaN` and infinities.
//!
//! ```
//! use st_math::det;
//!
//! // Exact definitions give exact results where the maths does.
//! assert_eq!(det::powi(2.0, 10), 1024.0);
//! assert_eq!(det::sin(0.0), 0.0);
//! assert!(det::fp_env_is_default());
//! ```

use core::f64::consts::PI;

/// Defines a public `f64` function that forwards to a `libm` function of the same arity.
///
/// A macro keeps the forwarding list (the part ADR 0008 fixes by name) down to one line per
/// function; each call site still carries its own rustdoc and doc test.
macro_rules! forward {
    ($(#[$meta:meta])* $name:ident($($arg:ident),+) => $target:path) => {
        $(#[$meta])*
        #[inline]
        #[must_use]
        pub fn $name($($arg: f64),+) -> f64 {
            $target($($arg),+)
        }
    };
}

forward! {
    /// Sine of `x` in radians, from `libm::sin`.
    ///
    /// ```
    /// assert_eq!(st_math::det::sin(0.0), 0.0);
    /// assert!(st_math::det::sin(f64::NAN).is_nan());
    /// ```
    sin(x) => libm::sin
}

forward! {
    /// Cosine of `x` in radians, from `libm::cos`.
    ///
    /// ```
    /// assert_eq!(st_math::det::cos(0.0), 1.0);
    /// ```
    cos(x) => libm::cos
}

forward! {
    /// Tangent of `x` in radians, from `libm::tan`.
    ///
    /// ```
    /// assert_eq!(st_math::det::tan(0.0), 0.0);
    /// ```
    tan(x) => libm::tan
}

/// Sine and cosine of `x` in radians, as `(sin, cos)`, from `libm::sincos`.
///
/// ```
/// assert_eq!(st_math::det::sin_cos(0.0), (0.0, 1.0));
/// ```
#[inline]
#[must_use]
pub fn sin_cos(x: f64) -> (f64, f64) {
    libm::sincos(x)
}

forward! {
    /// Arcsine of `x`, in radians within `[-pi/2, pi/2]`, from `libm::asin`.
    ///
    /// Returns `NaN` when `x` is outside `[-1, 1]`.
    ///
    /// ```
    /// assert_eq!(st_math::det::asin(0.0), 0.0);
    /// assert!(st_math::det::asin(2.0).is_nan());
    /// ```
    asin(x) => libm::asin
}

forward! {
    /// Arccosine of `x`, in radians within `[0, pi]`, from `libm::acos`.
    ///
    /// Returns `NaN` when `x` is outside `[-1, 1]`.
    ///
    /// ```
    /// assert_eq!(st_math::det::acos(1.0), 0.0);
    /// assert!(st_math::det::acos(2.0).is_nan());
    /// ```
    acos(x) => libm::acos
}

forward! {
    /// Arctangent of `x`, in radians within `[-pi/2, pi/2]`, from `libm::atan`.
    ///
    /// ```
    /// assert_eq!(st_math::det::atan(0.0), 0.0);
    /// ```
    atan(x) => libm::atan
}

forward! {
    /// Four-quadrant arctangent of `y / x`, in radians within `[-pi, pi]`, from `libm::atan2`.
    ///
    /// The first argument is `y` (the sine-like component), as in `f64::atan2`.
    ///
    /// ```
    /// assert_eq!(st_math::det::atan2(0.0, 1.0), 0.0);
    /// ```
    atan2(y, x) => libm::atan2
}

forward! {
    /// Hyperbolic sine of `x`, from `libm::sinh`.
    ///
    /// ```
    /// assert_eq!(st_math::det::sinh(0.0), 0.0);
    /// ```
    sinh(x) => libm::sinh
}

forward! {
    /// Hyperbolic cosine of `x`, from `libm::cosh`.
    ///
    /// ```
    /// assert_eq!(st_math::det::cosh(0.0), 1.0);
    /// ```
    cosh(x) => libm::cosh
}

forward! {
    /// Hyperbolic tangent of `x`, from `libm::tanh`.
    ///
    /// ```
    /// assert_eq!(st_math::det::tanh(0.0), 0.0);
    /// ```
    tanh(x) => libm::tanh
}

forward! {
    /// Inverse hyperbolic sine of `x`, from `libm::asinh`.
    ///
    /// ```
    /// assert_eq!(st_math::det::asinh(0.0), 0.0);
    /// ```
    asinh(x) => libm::asinh
}

forward! {
    /// Inverse hyperbolic cosine of `x`, from `libm::acosh`.
    ///
    /// Returns `NaN` when `x < 1`.
    ///
    /// ```
    /// assert_eq!(st_math::det::acosh(1.0), 0.0);
    /// assert!(st_math::det::acosh(0.5).is_nan());
    /// ```
    acosh(x) => libm::acosh
}

forward! {
    /// Inverse hyperbolic tangent of `x`, from `libm::atanh`.
    ///
    /// Returns `NaN` when `|x| > 1`, and an infinity at `x = +-1`.
    ///
    /// ```
    /// assert_eq!(st_math::det::atanh(0.0), 0.0);
    /// assert_eq!(st_math::det::atanh(1.0), f64::INFINITY);
    /// ```
    atanh(x) => libm::atanh
}

forward! {
    /// `e` raised to the power `x`, from `libm::exp`.
    ///
    /// ```
    /// assert_eq!(st_math::det::exp(0.0), 1.0);
    /// ```
    exp(x) => libm::exp
}

forward! {
    /// `2` raised to the power `x`, from `libm::exp2`.
    ///
    /// ```
    /// assert_eq!(st_math::det::exp2(10.0), 1024.0);
    /// ```
    exp2(x) => libm::exp2
}

forward! {
    /// `e^x - 1`, accurate for small `x`, from `libm::expm1`.
    ///
    /// Named `exp_m1` to match `f64::exp_m1`.
    ///
    /// ```
    /// assert_eq!(st_math::det::exp_m1(0.0), 0.0);
    /// ```
    exp_m1(x) => libm::expm1
}

forward! {
    /// Natural logarithm of `x`, from `libm::log`.
    ///
    /// Returns `NaN` for `x < 0` and negative infinity for `x = 0`.
    ///
    /// ```
    /// assert_eq!(st_math::det::ln(1.0), 0.0);
    /// assert_eq!(st_math::det::ln(0.0), f64::NEG_INFINITY);
    /// ```
    ln(x) => libm::log
}

forward! {
    /// `ln(1 + x)`, accurate for small `x`, from `libm::log1p`.
    ///
    /// ```
    /// assert_eq!(st_math::det::ln_1p(0.0), 0.0);
    /// ```
    ln_1p(x) => libm::log1p
}

forward! {
    /// Base-2 logarithm of `x`, from `libm::log2`.
    ///
    /// ```
    /// assert_eq!(st_math::det::log2(8.0), 3.0);
    /// ```
    log2(x) => libm::log2
}

forward! {
    /// Base-10 logarithm of `x`, from `libm::log10`.
    ///
    /// ```
    /// assert_eq!(st_math::det::log10(1000.0), 3.0);
    /// ```
    log10(x) => libm::log10
}

forward! {
    /// `x` raised to the real power `y`, from `libm::pow`.
    ///
    /// For an integer exponent known at the call site prefer [`powi`], which uses only
    /// multiplications.
    ///
    /// ```
    /// assert_eq!(st_math::det::powf(2.0, 0.5), core::f64::consts::SQRT_2);
    /// assert_eq!(st_math::det::powf(10.0, 2.0), 100.0);
    /// ```
    powf(x, y) => libm::pow
}

forward! {
    /// Cube root of `x`, from `libm::cbrt`.
    ///
    /// ```
    /// assert_eq!(st_math::det::cbrt(27.0), 3.0);
    /// assert_eq!(st_math::det::cbrt(-8.0), -2.0);
    /// ```
    cbrt(x) => libm::cbrt
}

forward! {
    /// Length of the hypotenuse `sqrt(x^2 + y^2)` without undue overflow, from `libm::hypot`.
    ///
    /// ```
    /// assert_eq!(st_math::det::hypot(3.0, 4.0), 5.0);
    /// ```
    hypot(x, y) => libm::hypot
}

/// `x` raised to the integer power `n`, by exponentiation by squaring.
///
/// Definition: write `|n|` in binary; start with `base = x` (or `base = 1.0 / x` when
/// `n < 0`) and `acc = 1.0`; for each bit of `|n|` from the least significant one, multiply
/// `acc` by `base` when the bit is set, then square `base`. The result is `acc`. The
/// sequence of multiplications is fixed by `n`, so the result is identical on every target.
/// The result is not necessarily the correctly rounded value of `x^n`: it carries the
/// rounding error of up to about `2 * log2(|n|)` multiplications.
///
/// `n = 0` returns `1.0` for every `x`, including `NaN` and zero. `n = i32::MIN` is
/// handled without overflow.
///
/// ```
/// use st_math::det::powi;
/// assert_eq!(powi(2.0, 10), 1024.0);
/// assert_eq!(powi(2.0, -2), 0.25);
/// assert_eq!(powi(-3.0, 3), -27.0);
/// assert_eq!(powi(f64::NAN, 0), 1.0);
/// ```
#[inline]
#[must_use]
pub fn powi(x: f64, n: i32) -> f64 {
    // `unsigned_abs` avoids the overflow of `-i32::MIN`.
    let mut exp = n.unsigned_abs();
    // Why start from `1.0 / x`: a negative power is the positive power of the reciprocal,
    // and doing it once up front keeps the multiplication sequence the same as for `n > 0`.
    let mut base = if n < 0 { 1.0 / x } else { x };
    let mut acc = 1.0;
    while exp != 0 {
        if exp & 1 == 1 {
            acc *= base;
        }
        exp >>= 1;
        base *= base;
    }
    acc
}

/// The smaller of `a` and `b`: `if b < a { b } else { a }`.
///
/// Unlike `f64::min`, the result is fully specified: when the comparison is false (equal
/// values, a `NaN` operand, or `a` smaller) the result is `a`. In particular `min(0.0, -0.0)`
/// is `0.0` and `min(-0.0, 0.0)` is `-0.0`, and `min(NaN, x)` is `NaN` while `min(x, NaN)`
/// is `x`.
///
/// ```
/// use st_math::det::min;
/// assert_eq!(min(1.0, 2.0), 1.0);
/// assert_eq!(min(2.0, 1.0), 1.0);
/// assert!(min(f64::NAN, 1.0).is_nan());
/// assert_eq!(min(1.0, f64::NAN), 1.0);
/// ```
#[inline]
#[must_use]
pub fn min(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// The larger of `a` and `b`: `if b > a { b } else { a }`.
///
/// Unlike `f64::max`, the result is fully specified: when the comparison is false (equal
/// values, a `NaN` operand, or `a` larger) the result is `a`. In particular `max(0.0, -0.0)`
/// is `0.0` and `max(-0.0, 0.0)` is `-0.0`.
///
/// ```
/// use st_math::det::max;
/// assert_eq!(max(1.0, 2.0), 2.0);
/// assert_eq!(max(2.0, 1.0), 2.0);
/// assert!(max(f64::NAN, 1.0).is_nan());
/// assert_eq!(max(1.0, f64::NAN), 1.0);
/// ```
#[inline]
#[must_use]
pub fn max(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

/// Restricts `x` to the range `[lo, hi]`: `if x < lo { lo } else if x > hi { hi } else { x }`.
///
/// Unlike `f64::clamp` this never panics. If `lo > hi` the definition is still followed
/// literally: `x < lo` gives `lo`, otherwise `x > hi` gives `hi`. A `NaN` `x` is returned
/// unchanged, and a `NaN` bound is never selected.
///
/// ```
/// use st_math::det::clamp;
/// assert_eq!(clamp(5.0, 0.0, 1.0), 1.0);
/// assert_eq!(clamp(-5.0, 0.0, 1.0), 0.0);
/// assert_eq!(clamp(0.5, 0.0, 1.0), 0.5);
/// assert!(clamp(f64::NAN, 0.0, 1.0).is_nan());
/// // Inverted bounds follow the definition instead of panicking.
/// assert_eq!(clamp(0.5, 1.0, 0.0), 1.0);
/// ```
#[inline]
#[must_use]
pub fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}

/// Converts degrees to radians: `d * (PI / 180.0)`.
///
/// `PI / 180.0` is evaluated as a constant, so the whole conversion is one multiplication.
///
/// ```
/// use st_math::det::to_radians;
/// assert_eq!(to_radians(0.0), 0.0);
/// assert_eq!(to_radians(180.0), core::f64::consts::PI);
/// ```
#[inline]
#[must_use]
pub fn to_radians(d: f64) -> f64 {
    d * (PI / 180.0)
}

/// Converts radians to degrees: `r * (180.0 / PI)`.
///
/// `180.0 / PI` is evaluated as a constant, so the whole conversion is one multiplication.
///
/// ```
/// use st_math::det::to_degrees;
/// assert_eq!(to_degrees(0.0), 0.0);
/// assert_eq!(to_degrees(core::f64::consts::PI), 180.0);
/// ```
#[inline]
#[must_use]
pub fn to_degrees(r: f64) -> f64 {
    r * (180.0 / PI)
}

/// Returns `true` when the floating-point environment is the default one the simulation
/// requires: round-to-nearest-even and no flush-to-zero of subnormal results.
///
/// The check evaluates three expressions on operands hidden from the optimiser with
/// [`core::hint::black_box`], so the compiler cannot fold them at build time:
///
/// * `f64::MIN_POSITIVE * 0.5` has the bit pattern `0x0008_0000_0000_0000`, which fails
///   when subnormal results are flushed to zero;
/// * `1.0 + 0.75 * f64::EPSILON == 1.0 + f64::EPSILON`;
/// * `-1.0 - 0.75 * f64::EPSILON == -1.0 - f64::EPSILON`.
///
/// Only round-to-nearest satisfies both rounding checks: rounding toward zero or toward
/// negative infinity fails the first, rounding toward zero or toward positive infinity
/// fails the second.
///
/// The simulation calls this at the start of every step and refuses to step when it fails.
/// The function reads no global state and cannot change the environment.
///
/// ```
/// assert!(st_math::det::fp_env_is_default());
/// ```
#[inline(never)]
#[must_use]
pub fn fp_env_is_default() -> bool {
    use core::hint::black_box;

    // Why `black_box` on every operand: with constant operands the compiler evaluates the
    // expressions itself, in round-to-nearest, and the check would test the compiler.
    let tiny = black_box(f64::MIN_POSITIVE) * black_box(0.5);
    let no_flush_to_zero = tiny.to_bits() == 0x0008_0000_0000_0000;

    let eps = black_box(f64::EPSILON);
    let three_quarters = black_box(0.75);
    let one = black_box(1.0);
    let up = one + three_quarters * eps == one + eps;
    let down = -one - three_quarters * eps == -one - eps;

    no_flush_to_zero && up && down
}
