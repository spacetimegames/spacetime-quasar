//! Golden tests for `st_math::det` (ADR 0008).
//!
//! Every function is checked on at least 16 inputs against expected results committed as
//! IEEE-754 bit patterns, so a change in `libm` (or any platform difference in the way it
//! is built) shows up as a failing test on every target. The transcendental tables are bit
//! patterns recorded from `st_math::det` (libm 0.2.16) on the reference dev machine; they
//! are the cross-platform contract, not an independent check against another libm.
//! The `min`, `max`, `clamp` and `powi` tables were checked by hand against the
//! definitions in the documentation.

use core::f64::consts::{E, FRAC_1_SQRT_2, FRAC_PI_2, PI};
use st_math::det;

/// Asserts that two values have the same bit pattern, printing both in hex on failure.
#[track_caller]
fn assert_bits(name: &str, input: &dyn core::fmt::Debug, got: f64, want: u64) {
    assert_eq!(
        got.to_bits(),
        want,
        "{name}({input:?}): got {:#018x} ({got:?}), want {want:#018x} ({:?})",
        got.to_bits(),
        f64::from_bits(want),
    );
}

macro_rules! golden1 {
    ($name:ident, [$(($x:expr, $bits:expr)),+ $(,)?]) => {
        #[test]
        fn $name() {
            let cases: &[(f64, u64)] = &[$(($x, $bits)),+];
            assert!(cases.len() >= 16);
            for &(x, bits) in cases {
                assert_bits(stringify!($name), &x, det::$name(x), bits);
            }
        }
    };
}

macro_rules! golden2 {
    ($name:ident, [$(($x:expr, $y:expr, $bits:expr)),+ $(,)?]) => {
        #[test]
        fn $name() {
            let cases: &[(f64, f64, u64)] = &[$(($x, $y, $bits)),+];
            assert!(cases.len() >= 16);
            for &(x, y, bits) in cases {
                assert_bits(stringify!($name), &(x, y), det::$name(x, y), bits);
            }
        }
    };
}

macro_rules! golden_sin_cos {
    ([$(($x:expr, $sin:expr, $cos:expr)),+ $(,)?]) => {
        #[test]
        fn sin_cos() {
            let cases: &[(f64, u64, u64)] = &[$(($x, $sin, $cos)),+];
            assert!(cases.len() >= 16);
            for &(x, sin, cos) in cases {
                let (s, c) = det::sin_cos(x);
                assert_bits("sin_cos.0", &x, s, sin);
                assert_bits("sin_cos.1", &x, c, cos);
            }
        }
    };
}

macro_rules! golden_powi {
    ([$(($x:expr, $n:expr, $bits:expr)),+ $(,)?]) => {
        #[test]
        fn powi() {
            let cases: &[(f64, i32, u64)] = &[$(($x, $n, $bits)),+];
            assert!(cases.len() >= 16);
            for &(x, n, bits) in cases {
                assert_bits("powi", &(x, n), det::powi(x, n), bits);
            }
        }
    };
}

// `min` and `max` share a table shape with the two-argument functions; they are separate
// only so the expected bits stay next to the name of the function under test.
macro_rules! golden_minmax {
    ($name:ident, [$(($a:expr, $b:expr, $bits:expr)),+ $(,)?]) => {
        #[test]
        fn $name() {
            let cases: &[(f64, f64, u64)] = &[$(($a, $b, $bits)),+];
            assert!(cases.len() >= 16);
            for &(a, b, bits) in cases {
                assert_bits(stringify!($name), &(a, b), det::$name(a, b), bits);
            }
        }
    };
}

macro_rules! golden_clamp {
    ([$(($x:expr, $lo:expr, $hi:expr, $bits:expr)),+ $(,)?]) => {
        #[test]
        fn clamp() {
            let cases: &[(f64, f64, f64, u64)] = &[$(($x, $lo, $hi, $bits)),+];
            assert!(cases.len() >= 16);
            for &(x, lo, hi, bits) in cases {
                assert_bits("clamp", &(x, lo, hi), det::clamp(x, lo, hi), bits);
            }
        }
    };
}

golden1!(
    sin,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (0.5, 0x3fdeaee8744b05f0),
        (-0.5, 0xbfdeaee8744b05f0),
        (1.0, 0x3feaed548f090cee),
        (-1.0, 0xbfeaed548f090cee),
        (2.0, 0x3fed18f6ead1b446),
        (3.0, 0x3fc210386db6d55b),
        (FRAC_PI_2, 0x3ff0000000000000),
        (PI, 0x3ca1a62633145c07),
        (10.0, 0xbfe1689ef5f34f52),
        (100.0, 0xbfe03425b78c4db8),
        (100000.0, 0x3fa24daa9c527e96),
        (1e-8, 0x3e45798ee2308c3a),
        (0.1, 0x3fb98eaecb8bcb2c),
        (123456.789, 0xbfeff50e60ab53f9),
    ]
);

golden1!(
    cos,
    [
        (0.0, 0x3ff0000000000000),
        (-0.0, 0x3ff0000000000000),
        (0.5, 0x3fec1528065b7d50),
        (-0.5, 0x3fec1528065b7d50),
        (1.0, 0x3fe14a280fb5068c),
        (-1.0, 0x3fe14a280fb5068c),
        (2.0, 0xbfdaa22657537205),
        (3.0, 0xbfefae04be85e5d2),
        (FRAC_PI_2, 0x3c91a62633145c07),
        (PI, 0xbff0000000000000),
        (10.0, 0xbfead9ac890c6b1f),
        (100.0, 0x3feb981dbf665fdf),
        (100000.0, 0xbfeffac3841b3da7),
        (1e-8, 0x3ff0000000000000),
        (0.1, 0x3fefd712f9a817c1),
        (123456.789, 0x3faa74d27c41b22a),
    ]
);

golden1!(
    tan,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (0.5, 0x3fe17b4f5bf3474a),
        (-0.5, 0xbfe17b4f5bf3474a),
        (1.0, 0x3ff8eb245cbee3a6),
        (-1.0, 0xbff8eb245cbee3a6),
        (2.0, 0xc0017af62e0950f8),
        (3.0, 0xbfc23ef71254b86f),
        (FRAC_PI_2, 0x434d02967c31cdb5),
        (PI, 0xbca1a62633145c07),
        (10.0, 0x3fe4bf5f34be3782),
        (100.0, 0xbfe2ca74d62b5d38),
        (100000.0, 0xbfa250a9d503313d),
        (1e-8, 0x3e45798ee2308c3a),
        (0.1, 0x3fb9af8877430b80),
        (123456.789, 0xc03353a85fe8d6d5),
    ]
);

golden1!(
    asin,
    [
        (-1.0, 0xbff921fb54442d18),
        (-0.9, 0xbff1ea93705fa172),
        (-0.5, 0xbfe0c152382d7366),
        (-0.1, 0xbfb9a49276037884),
        (-1e-9, 0xbe112e0be826d695),
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (1e-9, 0x3e112e0be826d695),
        (0.1, 0x3fb9a49276037884),
        (0.25, 0x3fd02be9ce0b87cd),
        (0.5, 0x3fe0c152382d7366),
        (FRAC_1_SQRT_2, 0x3fe921fb54442d19),
        (0.9, 0x3ff1ea93705fa172),
        (0.999, 0x3ff86ac9ad18f803),
        (1.0, 0x3ff921fb54442d18),
        (0.3333333333333333, 0x3fd5bfe34f051112),
    ]
);

golden1!(
    acos,
    [
        (-1.0, 0x400921fb54442d18),
        (-0.9, 0x400586476251e745),
        (-0.5, 0x4000c152382d7366),
        (-0.1, 0x3ffabc447ba464a1),
        (-1e-9, 0x3ff921fb5488e548),
        (0.0, 0x3ff921fb54442d18),
        (-0.0, 0x3ff921fb54442d18),
        (1e-9, 0x3ff921fb53ff74e9),
        (0.1, 0x3ff787b22ce3f590),
        (0.25, 0x3ff51700e0c14b25),
        (0.5, 0x3ff0c152382d7366),
        (FRAC_1_SQRT_2, 0x3fe921fb54442d18),
        (0.9, 0x3fdcdd9f8f922e98),
        (0.999, 0x3fa6e634e566a29e),
        (1.0, 0x0000000000000000),
        (0.3333333333333333, 0x3ff3b2028082e8d4),
    ]
);

golden1!(
    atan,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (0.5, 0x3fddac670561bb4f),
        (-0.5, 0xbfddac670561bb4f),
        (1.0, 0x3fe921fb54442d18),
        (-1.0, 0xbfe921fb54442d18),
        (2.0, 0x3ff1b6e192ebbe44),
        (10.0, 0x3ff789bd2c160054),
        (1000.0, 0x3ff91de2c0e658bd),
        (-1000000.0, 0xbff921fa47d4b30d),
        (1e-8, 0x3e45798ee2308c3a),
        (0.1, 0x3fb983e282e2cc4d),
        (1e300, 0x3ff921fb54442d18),
        (f64::INFINITY, 0x3ff921fb54442d18),
        (f64::NEG_INFINITY, 0xbff921fb54442d18),
        (0.3, 0x3fd2a73a661eaf06),
    ]
);

golden2!(
    atan2,
    [
        (0.0, 1.0, 0x0000000000000000),
        (0.0, -1.0, 0x400921fb54442d18),
        (-0.0, -1.0, 0xc00921fb54442d18),
        (1.0, 1.0, 0x3fe921fb54442d18),
        (1.0, -1.0, 0x4002d97c7f3321d2),
        (-1.0, -1.0, 0xc002d97c7f3321d2),
        (-1.0, 1.0, 0xbfe921fb54442d18),
        (1.0, 0.0, 0x3ff921fb54442d18),
        (-1.0, 0.0, 0xbff921fb54442d18),
        (0.5, 2.0, 0x3fcf5b75f92c80dd),
        (3.0, 4.0, 0x3fe4978fa3269ee1),
        (1e-10, 1.0, 0x3ddb7cdfd9d7bdbb),
        (10000000000.0, 1e-10, 0x3ff921fb54442d18),
        (123.456, -789.012, 0x4007e41c43ffefb7),
        (0.0, 0.0, 0x0000000000000000),
        (f64::INFINITY, f64::INFINITY, 0x3fe921fb54442d18),
    ]
);

golden1!(
    sinh,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (0.1, 0x3fb9a487337b59b3),
        (-0.5, 0xbfe0acd00fe63b97),
        (1.0, 0x3ff2cd9fc44eb982),
        (-1.0, 0xbff2cd9fc44eb982),
        (2.0, 0x400d03cf63b6e1a0),
        (5.0, 0x40528d0166f07374),
        (10.0, 0x40c5829dced69992),
        (20.0, 0x41aceb088b68e804),
        (50.0, 0x44619103e4080b45),
        (100.0, 0x48e3494a9b171bf5),
        (700.0, 0x7efd945df4f8ec8e),
        (1e-10, 0x3ddb7cdfd9d7bdbb),
        (0.3, 0x3fd37d42af54b926),
        (-3.0, 0xc0240926e70949ae),
    ]
);

golden1!(
    cosh,
    [
        (0.0, 0x3ff0000000000000),
        (-0.0, 0x3ff0000000000000),
        (0.1, 0x3ff0147f40224b38),
        (-0.5, 0x3ff20ac1862ae8d0),
        (1.0, 0x3ff8b07551d9f551),
        (-1.0, 0x3ff8b07551d9f551),
        (2.0, 0x400e18fa0df2d9bc),
        (5.0, 0x40528d6fcbeff3aa),
        (10.0, 0x40c5829dd053712e),
        (20.0, 0x41aceb088b68e804),
        (50.0, 0x44619103e4080b45),
        (100.0, 0x48e3494a9b171bf5),
        (700.0, 0x7efd945df4f8ec8e),
        (1e-10, 0x3ff0000000000000),
        (0.3, 0x3ff0b9b4e0b6ec4c),
        (-3.0, 0x402422a497d6185e),
    ]
);

golden1!(
    tanh,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (0.1, 0x3fb983d7795f413a),
        (-0.5, 0xbfdd9353d7568af3),
        (1.0, 0x3fe85efab514f394),
        (-1.0, 0xbfe85efab514f394),
        (2.0, 0x3feed9505e1bc3d4),
        (5.0, 0x3fefff419668df11),
        (10.0, 0x3feffffffdc96f35),
        (20.0, 0x3ff0000000000000),
        (50.0, 0x3ff0000000000000),
        (100.0, 0x3ff0000000000000),
        (700.0, 0x3ff0000000000000),
        (1e-10, 0x3ddb7cdfd9d7bdbb),
        (0.3, 0x3fd2a4dda7d914f9),
        (-3.0, 0xbfefd77d111a0b00),
    ]
);

golden1!(
    asinh,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (0.1, 0x3fb98eb9e7e5fc3f),
        (-0.5, 0xbfdecc2caec5160a),
        (1.0, 0x3fec34366179d427),
        (-1.0, 0xbfec34366179d427),
        (2.0, 0x3ff719218313d087),
        (10.0, 0x4007fc5c506d2bdb),
        (100.0, 0x40153180a93d3c2a),
        (100000.0, 0x402869825a9284f4),
        (-10000000000.0, 0xc037b810429a7c2a),
        (1e300, 0x40859bbfd8b83e43),
        (1e-10, 0x3ddb7cdfd9d7bdbb),
        (0.3, 0x3fd2ec4ea565fcc1),
        (-3.0, 0xbffd185b507edc0e),
        (7.5, 0x4005b321028a694d),
    ]
);

golden1!(
    acosh,
    [
        (1.0, 0x0000000000000000),
        (1.0000001, 0x3f3d4effc065cae4),
        (1.5, 0x3feecc2caec5160a),
        (2.0, 0x3ff5124271980434),
        (3.0, 0x3ffc34366179d426),
        (10.0, 0x4007f21ed1ce05d4),
        (100.0, 0x401531738dcbc626),
        (100000.0, 0x402869825a921700),
        (10000000000.0, 0x4037b810429a7c2a),
        (1e300, 0x40859bbfd8b83e43),
        (1.1, 0x3fdc636c1a882f2c),
        (E, 0x3ffa84eef63326e7),
        (7.0, 0x4005124271980435),
        (50.0, 0x40126b9783d0a89d),
        (1000.0, 0x401e6752e8a84ed4),
        (1.0000000000001, 0x3e9dffffffffffba),
    ]
);

golden1!(
    atanh,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (0.1, 0x3fb9af93cd234412),
        (-0.1, 0xbfb9af93cd234412),
        (0.5, 0x3fe193ea7aad030a),
        (-0.5, 0xbfe193ea7aad030a),
        (0.9, 0x3ff78e360604b32d),
        (0.99, 0x40052c581997cd85),
        (0.999999, 0x401d046eb8b86c1c),
        (1e-9, 0x3e112e0be826d695),
        (0.25, 0x3fd058aefa811452),
        (0.75, 0x3fef2272ae325a57),
        (-0.75, 0xbfef2272ae325a57),
        (0.3, 0x3fd3cf2b50617c96),
        (0.6, 0x3fe62e42fefa39ef),
        (1.0, 0x7ff0000000000000),
    ]
);

golden1!(
    exp,
    [
        (0.0, 0x3ff0000000000000),
        (-0.0, 0x3ff0000000000000),
        (1.0, 0x4005bf0a8b14576a),
        (-1.0, 0x3fd78b56362cef38),
        (0.5, 0x3ffa61298e1e069c),
        (2.0, 0x401d8e64b8d4ddae),
        (10.0, 0x40d5829dcf950560),
        (-10.0, 0x3f07cd79b5647c9a),
        (100.0, 0x48f3494a9b171bf5),
        (-100.0, 0x36ea8c1f14e2af5d),
        (700.0, 0x7f0d945df4f8ec8e),
        (-700.0, 0x00d14f2b0fb9307f),
        (1e-10, 0x3ff000000006df38),
        (0.1, 0x3ff1aec7b35a00d4),
        (709.78, 0x7fefe9ce5c4c52b4),
        (-745.0, 0x0000000000000001),
    ]
);

golden1!(
    exp2,
    [
        (0.0, 0x3ff0000000000000),
        (1.0, 0x4000000000000000),
        (-1.0, 0x3fe0000000000000),
        (0.5, 0x3ff6a09e667f3bcd),
        (2.0, 0x4010000000000000),
        (10.0, 0x4090000000000000),
        (-10.0, 0x3f50000000000000),
        (100.0, 0x4630000000000000),
        (1023.0, 0x7fe0000000000000),
        (-1022.0, 0x0010000000000000),
        (-1074.0, 0x0000000000000001),
        (0.1, 0x3ff125fbee250664),
        (1e-10, 0x3ff000000004c366),
        (3.3, 0x4023b2c47bff8328),
        (-0.3, 0x3fe9fdf8bcce533e),
        (52.0, 0x4330000000000000),
    ]
);

golden1!(
    exp_m1,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (1e-10, 0x3ddb7cdfd9dda4e3),
        (-1e-10, 0xbddb7cdfd9d1d693),
        (1e-5, 0x3ee4f8bc681cdfb6),
        (0.1, 0x3fbaec7b35a00d3a),
        (-0.1, 0xbfb85c933156a62c),
        (0.5, 0x3fe4c2531c3c0d38),
        (1.0, 0x3ffb7e151628aed2),
        (-1.0, 0xbfe43a54e4e98864),
        (2.0, 0x40198e64b8d4ddae),
        (10.0, 0x40d5825dcf950560),
        (-10.0, 0xbfefffa0ca192a6e),
        (50.0, 0x44719103e4080b45),
        (700.0, 0x7f0d945df4f8ec8e),
        (-50.0, 0xbff0000000000000),
    ]
);

golden1!(
    ln,
    [
        (1.0, 0x0000000000000000),
        (2.0, 0x3fe62e42fefa39ef),
        (0.5, 0xbfe62e42fefa39ef),
        (10.0, 0x40026bb1bbb55516),
        (0.1, 0xc0026bb1bbb55515),
        (E, 0x3ff0000000000000),
        (100.0, 0x40126bb1bbb55516),
        (1e-10, 0xc037069e2aa2aa5b),
        (10000000000.0, 0x4037069e2aa2aa5b),
        (1e300, 0x4085963447f87fb5),
        (1e-300, 0xc085963447f87fb5),
        (0.9999999, 0xbe7ad7f2b1049b9f),
        (1.0000001, 0x3e7ad7f2847b6492),
        (3.0, 0x3ff193ea7aad030a),
        (7.0, 0x3fff2272ae325a57),
        (12345.678, 0x4022d79559791e31),
    ]
);

golden1!(
    ln_1p,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (1e-10, 0x3ddb7cdfd9d1d693),
        (-1e-10, 0xbddb7cdfd9dda4e3),
        (1e-5, 0x3ee4f8aea9ae7317),
        (0.1, 0x3fb8663f793c46c7),
        (-0.1, 0xbfbaf8e8210a415e),
        (-0.5, 0xbfe62e42fefa39ef),
        (1.0, 0x3fe62e42fefa39ef),
        (2.0, 0x3ff193ea7aad030a),
        (10.0, 0x40032ee3b77f374c),
        (10000000000.0, 0x4037069e2aa3184e),
        (-0.9, 0xc0026bb1bbb55516),
        (1e-300, 0x01a56e1fc2f8f359),
        (0.5, 0x3fd9f323ecbf984c),
        (1.718281828459045, 0x3ff0000000000000),
    ]
);

golden1!(
    log2,
    [
        (1.0, 0x0000000000000000),
        (2.0, 0x3ff0000000000000),
        (0.5, 0xbff0000000000000),
        (8.0, 0x4008000000000000),
        (1024.0, 0x4024000000000000),
        (0.125, 0xc008000000000000),
        (3.0, 0x3ff95c01a39fbd68),
        (10.0, 0x400a934f0979a371),
        (0.1, 0xc00a934f0979a371),
        (1e-10, 0xc0409c1165ec0627),
        (10000000000.0, 0x40409c1165ec0627),
        (1e300, 0x408f24a09f1a8b89),
        (1e-300, 0xc08f24a09f1a8b89),
        (5e-324, 0xc090c80000000000),
        (7.0, 0x400675767f54042d),
        (12345.678, 0x402b2ef5b86e63d0),
    ]
);

golden1!(
    log10,
    [
        (1.0, 0x0000000000000000),
        (10.0, 0x3ff0000000000000),
        (0.1, 0xbff0000000000000),
        (1000.0, 0x4008000000000000),
        (1e-5, 0xc014000000000000),
        (2.0, 0x3fd34413509f79ff),
        (3.0, 0x3fde8927964fd5fd),
        (0.5, 0xbfd34413509f79ff),
        (100.0, 0x4000000000000000),
        (10000000000.0, 0x4024000000000000),
        (1e300, 0x4072c00000000000),
        (1e-300, 0xc072c00000000000),
        (5e-324, 0xc07434e6420f4374),
        (7.0, 0x3feb0b0b0b78cc3f),
        (E, 0x3fdbcb7b1526e50e),
        (12345.678, 0x40105db618083a9e),
    ]
);

golden2!(
    powf,
    [
        (2.0, 10.0, 0x4090000000000000),
        (2.0, 0.5, 0x3ff6a09e667f3bcd),
        (10.0, -3.0, 0x3f50624dd2f1a9fc),
        (0.5, 2.0, 0x3fd0000000000000),
        (3.0, 3.0, 0x403b000000000000),
        (1.5, 2.5, 0x40060b9fd68a4554),
        (9.0, 0.5, 0x4008000000000000),
        (2.0, -1074.0, 0x0000000000000001),
        (0.0, 0.0, 0x3ff0000000000000),
        (0.0, 1.0, 0x0000000000000000),
        (-2.0, 3.0, 0xc020000000000000),
        (-2.0, 2.0, 0x4010000000000000),
        (1.0, 1e300, 0x3ff0000000000000),
        (10.0, 308.0, 0x7fe1ccf385ebc8a0),
        (E, 1.0, 0x4005bf0a8b145769),
        (1.0001, 10000.0, 0x4005bec34aabbfd3),
    ]
);

golden1!(
    cbrt,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (1.0, 0x3ff0000000000000),
        (-1.0, 0xbff0000000000000),
        (8.0, 0x4000000000000000),
        (-8.0, 0xc000000000000000),
        (27.0, 0x4008000000000000),
        (0.001, 0x3fb999999999999a),
        (1000.0, 0x4024000000000000),
        (1e300, 0x54b249ad2594c37d),
        (1e-300, 0x2b2bff2ee48e0530),
        (2.0, 0x3ff428a2f98d728b),
        (3.0, 0x3ff7137449123ef6),
        (10.0, 0x40013c484138704f),
        (0.5, 0x3fe965fea53d6e3d),
        (123456.789, 0x4048e58dab7da808),
    ]
);

golden2!(
    hypot,
    [
        (3.0, 4.0, 0x4014000000000000),
        (5.0, 12.0, 0x402a000000000000),
        (1.0, 1.0, 0x3ff6a09e667f3bcd),
        (0.0, 0.0, 0x0000000000000000),
        (1e200, 1e200, 0x697d8f9811335b57),
        (1e-200, 1e-200, 0x167151f68876f410),
        (1.0, 1e-10, 0x3ff0000000000000),
        (0.1, 0.2, 0x3fcc9f25c5bfedd9),
        (-3.0, 4.0, 0x4014000000000000),
        (f64::INFINITY, 1.0, 0x7ff0000000000000),
        (1e308, 1e308, 0x7fe92c80954c51f5),
        (7.0, 24.0, 0x4039000000000000),
        (0.3, 0.4, 0x3fe0000000000000),
        (123456.789, 98765.4321, 0x41034caea83c70e9),
        (-0.0, -0.0, 0x0000000000000000),
        (2.0, 3.0, 0x400cd82b446159f3),
    ]
);

golden_sin_cos!([
    (0.0, 0x0000000000000000, 0x3ff0000000000000),
    (-0.0, 0x8000000000000000, 0x3ff0000000000000),
    (0.5, 0x3fdeaee8744b05f0, 0x3fec1528065b7d50),
    (-0.5, 0xbfdeaee8744b05f0, 0x3fec1528065b7d50),
    (1.0, 0x3feaed548f090cee, 0x3fe14a280fb5068c),
    (-1.0, 0xbfeaed548f090cee, 0x3fe14a280fb5068c),
    (2.0, 0x3fed18f6ead1b446, 0xbfdaa22657537205),
    (3.0, 0x3fc210386db6d55b, 0xbfefae04be85e5d2),
    (FRAC_PI_2, 0x3ff0000000000000, 0x3c91a62633145c07),
    (PI, 0x3ca1a62633145c07, 0xbff0000000000000),
    (10.0, 0xbfe1689ef5f34f52, 0xbfead9ac890c6b1f),
    (100.0, 0xbfe03425b78c4db8, 0x3feb981dbf665fdf),
    (100000.0, 0x3fa24daa9c527e96, 0xbfeffac3841b3da7),
    (1e-8, 0x3e45798ee2308c3a, 0x3ff0000000000000),
    (0.1, 0x3fb98eaecb8bcb2c, 0x3fefd712f9a817c1),
    (123456.789, 0xbfeff50e60ab53f9, 0x3faa74d27c41b22a),
]);

golden1!(
    to_radians,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (1.0, 0x3f91df46a2529d39),
        (-1.0, 0xbf91df46a2529d39),
        (45.0, 0x3fe921fb54442d18),
        (90.0, 0x3ff921fb54442d18),
        (180.0, 0x400921fb54442d18),
        (360.0, 0x401921fb54442d18),
        (-270.0, 0xc012d97c7f3321d2),
        (0.5, 0x3f81df46a2529d39),
        (30.0, 0x3fe0c152382d7365),
        (0.001, 0x3ef24d1521193474),
        (1234.5678, 0x40358c1a147a61d9),
        (720.0, 0x402921fb54442d18),
        (10000000000.0, 0x41a4ce527a661c14),
        (57.29577951308232, 0x3ff0000000000000),
    ]
);

golden1!(
    to_degrees,
    [
        (0.0, 0x0000000000000000),
        (-0.0, 0x8000000000000000),
        (1.0, 0x404ca5dc1a63c1f8),
        (-1.0, 0xc04ca5dc1a63c1f8),
        (45.0, 0x40a4249ec28e2462),
        (90.0, 0x40b4249ec28e2462),
        (180.0, 0x40c4249ec28e2462),
        (360.0, 0x40d4249ec28e2462),
        (-270.0, 0xc0ce36ee23d53694),
        (0.5, 0x403ca5dc1a63c1f8),
        (30.0, 0x409adb7e58bd85d8),
        (0.001, 0x3fad55df566a3f75),
        (1234.5678, 0x40f144f864330dc0),
        (720.0, 0x40e4249ec28e2462),
        (10000000000.0, 0x4260acde44675a58),
        (57.29577951308232, 0x40a9a59cd9e8a324),
    ]
);

golden_powi!([
    (2.0, 10, 0x4090000000000000),
    (2.0, -2, 0x3fd0000000000000),
    (-3.0, 3, 0xc03b000000000000),
    (10.0, 0, 0x3ff0000000000000),
    (0.0, 0, 0x3ff0000000000000),
    (f64::NAN, 0, 0x3ff0000000000000),
    (0.0, -1, 0x7ff0000000000000),
    (-0.0, -1, 0xfff0000000000000),
    (1.5, 2, 0x4002000000000000),
    (0.5, -3, 0x4020000000000000),
    (2.0, -1074, 0x0000000000000001),
    (2.0, 1023, 0x7fe0000000000000),
    (2.0, 1024, 0x7ff0000000000000),
    (3.0, 40, 0x43e517168a4523fd),
    (1.0000001, 100, 0x3ff0000a7c5e2b5a),
    (0.1, -7, 0x416312d000000000),
]);

golden_minmax!(
    min,
    [
        (1.0, 2.0, 0x3ff0000000000000),
        (2.0, 1.0, 0x3ff0000000000000),
        (0.0, -0.0, 0x0000000000000000),
        (-0.0, 0.0, 0x8000000000000000),
        (0.0, 0.0, 0x0000000000000000),
        (-0.0, -0.0, 0x8000000000000000),
        (f64::NAN, 1.0, 0x7ff8000000000000),
        (1.0, f64::NAN, 0x3ff0000000000000),
        (-1.0, -2.0, 0xc000000000000000),
        (-2.0, -1.0, 0xc000000000000000),
        (f64::INFINITY, 1.0, 0x3ff0000000000000),
        (f64::NEG_INFINITY, 1.0, 0xfff0000000000000),
        (5e-324, 0.0, 0x0000000000000000),
        (0.0, 5e-324, 0x0000000000000000),
        (1e300, -1e300, 0xfe37e43c8800759c),
        (-3.5, -3.5, 0xc00c000000000000),
    ]
);

golden_minmax!(
    max,
    [
        (1.0, 2.0, 0x4000000000000000),
        (2.0, 1.0, 0x4000000000000000),
        (0.0, -0.0, 0x0000000000000000),
        (-0.0, 0.0, 0x8000000000000000),
        (0.0, 0.0, 0x0000000000000000),
        (-0.0, -0.0, 0x8000000000000000),
        (f64::NAN, 1.0, 0x7ff8000000000000),
        (1.0, f64::NAN, 0x3ff0000000000000),
        (-1.0, -2.0, 0xbff0000000000000),
        (-2.0, -1.0, 0xbff0000000000000),
        (f64::INFINITY, 1.0, 0x7ff0000000000000),
        (f64::NEG_INFINITY, 1.0, 0x3ff0000000000000),
        (5e-324, 0.0, 0x0000000000000001),
        (0.0, 5e-324, 0x0000000000000001),
        (1e300, -1e300, 0x7e37e43c8800759c),
        (-3.5, -3.5, 0xc00c000000000000),
    ]
);

golden_clamp!([
    (5.0, 0.0, 1.0, 0x3ff0000000000000),
    (-5.0, 0.0, 1.0, 0x0000000000000000),
    (0.5, 0.0, 1.0, 0x3fe0000000000000),
    (0.0, 0.0, 1.0, 0x0000000000000000),
    (-0.0, 0.0, 1.0, 0x8000000000000000),
    (1.0, 0.0, 1.0, 0x3ff0000000000000),
    (f64::NAN, 0.0, 1.0, 0x7ff8000000000000),
    (0.5, 1.0, 0.0, 0x3ff0000000000000),
    (-1.0, 1.0, 0.0, 0x3ff0000000000000),
    (2.0, 1.0, 0.0, 0x0000000000000000),
    (0.5, f64::NAN, 1.0, 0x3fe0000000000000),
    (0.5, 0.0, f64::NAN, 0x3fe0000000000000),
    (f64::INFINITY, 0.0, 1.0, 0x3ff0000000000000),
    (f64::NEG_INFINITY, 0.0, 1.0, 0x0000000000000000),
    (-0.0, -0.0, 0.0, 0x8000000000000000),
    (0.0, -0.0, -0.0, 0x0000000000000000),
]);

#[test]
fn exact_identities() {
    // Results that are exact in the mathematics must be exact in the implementation.
    assert_eq!(det::exp2(10.0), 1024.0);
    assert_eq!(det::log2(1024.0), 10.0);
    assert_eq!(det::log10(1000.0), 3.0);
    assert_eq!(det::cbrt(27.0), 3.0);
    assert_eq!(det::hypot(3.0, 4.0), 5.0);
    assert_eq!(det::powf(2.0, 10.0), 1024.0);
    assert_eq!(det::sin(0.0), 0.0);
    assert_eq!(det::cos(0.0), 1.0);
    assert_eq!(det::exp(0.0), 1.0);
    assert_eq!(det::ln(1.0), 0.0);
}

#[test]
fn non_finite_and_domain_errors_do_not_panic() {
    assert!(det::sin(f64::INFINITY).is_nan());
    assert!(det::cos(f64::NAN).is_nan());
    assert!(det::asin(1.5).is_nan());
    assert!(det::acos(-1.5).is_nan());
    assert!(det::acosh(0.5).is_nan());
    assert!(det::atanh(1.5).is_nan());
    assert!(det::ln(-1.0).is_nan());
    assert_eq!(det::ln(0.0), f64::NEG_INFINITY);
    assert_eq!(det::exp(1000.0), f64::INFINITY);
    assert_eq!(det::exp(-1000.0), 0.0);
    assert_eq!(det::powi(2.0, i32::MAX), f64::INFINITY);
    assert_eq!(det::powi(2.0, i32::MIN), 0.0);
    assert_eq!(det::powi(1.0, i32::MIN), 1.0);
    assert!(det::powi(f64::NAN, 3).is_nan());
}

#[test]
fn degree_conversions_use_the_documented_constants() {
    for d in [0.0, 1.0, 45.0, 90.0, 180.0, -270.0, 1234.5678] {
        assert_eq!(det::to_radians(d).to_bits(), (d * (PI / 180.0)).to_bits());
        assert_eq!(det::to_degrees(d).to_bits(), (d * (180.0 / PI)).to_bits());
    }
    assert_eq!(det::to_radians(180.0), PI);
    assert_eq!(det::to_degrees(PI), 180.0);
}

#[test]
fn floating_point_environment_is_default() {
    assert!(det::fp_env_is_default());
    // The three conditions the check relies on, evaluated here once without the check's
    // optimiser barriers; if they ever failed, the test above would be meaningless.
    assert_eq!((f64::MIN_POSITIVE * 0.5).to_bits(), 0x0008_0000_0000_0000);
}
