//! The leak detector (ADR 0008, "Physics configuration and how leaks are
//! neutralised").
//!
//! Purpose: catch any call into the platform math library. This module
//! defines the C math symbols itself. The linker binds every reference in the
//! executable to these definitions instead of glibc's, so a call from any
//! crate (Rapier, Parry, glam, a future dependency) is counted. Each
//! definition then forwards to the pure-Rust `libm` crate, so the results are
//! the ones the game gets from `st_math`.
//!
//! The module exists only in the harness binary and only on x86_64 Linux. The
//! tests and the other platforms behave like the game.
//!
//! Boundary: the one place in the workspace that allows `unsafe`. The ADR
//! approves it on this module only: `#[unsafe(no_mangle)]` and raw-pointer
//! writes for `sincos`.

#![allow(
    unsafe_code,
    reason = "ADR 0008: no_mangle C symbols and sincos out-pointers, this module only"
)]

use std::sync::atomic::{AtomicU64, Ordering};

/// Symbols whose calls are expected. Each entry carries its reason.
const ALLOWED: &[(&str, &str)] = &[
    (
        "log2",
        "Parry's BVH, covered by the `parry_bvh_log2_domain` test",
    ),
    ("fma", "correctly rounded on every platform"),
    ("fmaf", "correctly rounded on every platform"),
];

macro_rules! leak_symbols {
    ($( $name:ident ( $($arg:ident : $ty:ty),+ ) -> $ret:ty = $forward:path; )+) => {
        /// Index of each symbol in `COUNTS`.
        #[allow(non_camel_case_types, reason = "variants are the C symbol names")]
        #[derive(Clone, Copy)]
        enum Symbol { $($name),+ }

        /// Symbol names, in `Symbol` order.
        const NAMES: &[&str] = &[$(stringify!($name)),+];

        static COUNTS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];

        $(
            // SAFETY (no_mangle): the name, argument types and return type
            // match the C declaration of the function exactly, and the
            // function neither unwinds nor touches memory.
            #[unsafe(no_mangle)]
            pub extern "C" fn $name($($arg: $ty),+) -> $ret {
                COUNTS[Symbol::$name as usize].fetch_add(1, Ordering::Relaxed);
                $forward($($arg),+)
            }
        )+
    };
}

leak_symbols! {
    acos(x: f64) -> f64 = libm::acos;
    asin(x: f64) -> f64 = libm::asin;
    atan(x: f64) -> f64 = libm::atan;
    atan2(y: f64, x: f64) -> f64 = libm::atan2;
    cos(x: f64) -> f64 = libm::cos;
    sin(x: f64) -> f64 = libm::sin;
    tan(x: f64) -> f64 = libm::tan;
    cosh(x: f64) -> f64 = libm::cosh;
    sinh(x: f64) -> f64 = libm::sinh;
    tanh(x: f64) -> f64 = libm::tanh;
    acosh(x: f64) -> f64 = libm::acosh;
    asinh(x: f64) -> f64 = libm::asinh;
    atanh(x: f64) -> f64 = libm::atanh;
    exp(x: f64) -> f64 = libm::exp;
    exp2(x: f64) -> f64 = libm::exp2;
    expm1(x: f64) -> f64 = libm::expm1;
    log(x: f64) -> f64 = libm::log;
    log2(x: f64) -> f64 = libm::log2;
    log10(x: f64) -> f64 = libm::log10;
    log1p(x: f64) -> f64 = libm::log1p;
    pow(x: f64, y: f64) -> f64 = libm::pow;
    cbrt(x: f64) -> f64 = libm::cbrt;
    hypot(x: f64, y: f64) -> f64 = libm::hypot;
    fma(x: f64, y: f64, z: f64) -> f64 = libm::fma;
    acosf(x: f32) -> f32 = libm::acosf;
    asinf(x: f32) -> f32 = libm::asinf;
    atanf(x: f32) -> f32 = libm::atanf;
    atan2f(y: f32, x: f32) -> f32 = libm::atan2f;
    cosf(x: f32) -> f32 = libm::cosf;
    sinf(x: f32) -> f32 = libm::sinf;
    tanf(x: f32) -> f32 = libm::tanf;
    coshf(x: f32) -> f32 = libm::coshf;
    sinhf(x: f32) -> f32 = libm::sinhf;
    tanhf(x: f32) -> f32 = libm::tanhf;
    acoshf(x: f32) -> f32 = libm::acoshf;
    asinhf(x: f32) -> f32 = libm::asinhf;
    atanhf(x: f32) -> f32 = libm::atanhf;
    expf(x: f32) -> f32 = libm::expf;
    exp2f(x: f32) -> f32 = libm::exp2f;
    expm1f(x: f32) -> f32 = libm::expm1f;
    logf(x: f32) -> f32 = libm::logf;
    log2f(x: f32) -> f32 = libm::log2f;
    log10f(x: f32) -> f32 = libm::log10f;
    log1pf(x: f32) -> f32 = libm::log1pf;
    powf(x: f32, y: f32) -> f32 = libm::powf;
    cbrtf(x: f32) -> f32 = libm::cbrtf;
    hypotf(x: f32, y: f32) -> f32 = libm::hypotf;
    fmaf(x: f32, y: f32, z: f32) -> f32 = libm::fmaf;
}

// `sincos` and `sincosf` return through pointers, so the macro does not fit.
// They are counted under their own names, after the macro's symbols.

/// Counter for `sincos`.
static SINCOS: AtomicU64 = AtomicU64::new(0);
/// Counter for `sincosf`.
static SINCOSF: AtomicU64 = AtomicU64::new(0);

/// C `void sincos(double x, double *sin, double *cos)`.
///
/// # Safety
///
/// `sin` and `cos` must be valid for a write of one `f64`, as the C
/// declaration requires of every caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sincos(x: f64, sin: *mut f64, cos: *mut f64) {
    SINCOS.fetch_add(1, Ordering::Relaxed);
    let (s, c) = libm::sincos(x);
    // SAFETY: the C contract of `sincos` makes both pointers valid for one
    // write; this is the same contract glibc's definition relies on.
    unsafe {
        sin.write(s);
        cos.write(c);
    }
}

/// C `void sincosf(float x, float *sin, float *cos)`.
///
/// # Safety
///
/// `sin` and `cos` must be valid for a write of one `f32`, as the C
/// declaration requires of every caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sincosf(x: f32, sin: *mut f32, cos: *mut f32) {
    SINCOSF.fetch_add(1, Ordering::Relaxed);
    let (s, c) = libm::sincosf(x);
    // SAFETY: as in `sincos`.
    unsafe {
        sin.write(s);
        cos.write(c);
    }
}

/// The outcome of a run, for printing and for the exit status.
pub struct Report {
    /// Text for stdout: one line per symbol that was called, then a verdict.
    pub text: String,
    /// True if a symbol outside the allow-list was called.
    pub leaked: bool,
}

/// Counts of every symbol that was called at least once, sorted by name.
fn called() -> Vec<(&'static str, u64)> {
    let mut v: Vec<(&'static str, u64)> = NAMES
        .iter()
        .zip(COUNTS.iter())
        .map(|(n, c)| (*n, c.load(Ordering::Relaxed)))
        .chain([
            ("sincos", SINCOS.load(Ordering::Relaxed)),
            ("sincosf", SINCOSF.load(Ordering::Relaxed)),
        ])
        .filter(|(_, n)| *n > 0)
        .collect();
    v.sort_unstable();
    v
}

/// Builds the report from the counters. Call it after the scenarios ran.
pub fn report() -> Report {
    let mut text = String::new();
    let mut leaked = false;
    for (name, n) in called() {
        match ALLOWED.iter().find(|(a, _)| *a == name) {
            Some((_, reason)) => {
                text.push_str(&format!("leak-detector: {name} {n} allowed ({reason})\n"));
            }
            None => {
                leaked = true;
                text.push_str(&format!("leak-detector: {name} {n} LEAK\n"));
            }
        }
    }
    if leaked {
        text.push_str("leak-detector: FAIL, the platform libm was called (see LEAK lines)\n");
    } else {
        text.push_str("leak-detector: ok, no platform libm call outside the allow-list\n");
    }
    Report { text, leaked }
}
