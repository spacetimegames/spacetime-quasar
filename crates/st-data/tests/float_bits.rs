//! The float-bit golden test of ADR 0010: literals parse to the same bits on
//! every target. Run on Linux, Windows, macOS and `wasm32-wasip1`.

use serde::Deserialize;

/// (literal, `f32` bits, `f64` bits). Recorded from a correctly rounded parse
/// and verified on x86_64 Linux and wasm32 (ADR 0010); the Windows and macOS
/// runs in CI are what verify those targets.
const TABLE: &[(&str, u32, u64)] = &[
    // The table of ADR 0010.
    ("0.1", 0x3dcc_cccd, 0x3fb9_9999_9999_999a),
    ("16777217", 0x4b80_0000, 0x4170_0000_1000_0000),
    ("1.4e-45", 0x0000_0001, 0x369f_f868_bf4d_956a),
    ("0.30000000000000004", 0x3e99_999a, 0x3fd3_3333_3333_3334),
    ("-0.0", 0x8000_0000, 0x8000_0000_0000_0000),
    ("2.5e-324", 0x0000_0000, 0x0000_0000_0000_0001),
    // Values near f32::MAX and f64::MAX.
    ("3.4028235e38", 0x7f7f_ffff, 0x47ef_ffff_e54d_aff8),
    (
        "340282350000000000000000000000000000000",
        0x7f7f_ffff,
        0x47ef_ffff_e54d_aff8,
    ),
    ("3.4028236e38", 0x7f80_0000, 0x47ef_ffff_f514_a7bc),
    ("3.4028237e38", 0x7f80_0000, 0x47f0_0000_026d_cfc0),
    ("1.7976931348623157e308", 0x7f80_0000, 0x7fef_ffff_ffff_ffff),
    ("1e309", 0x7f80_0000, 0x7ff0_0000_0000_0000),
    // Subnormals and the smallest normals.
    ("1e-45", 0x0000_0001, 0x3696_d601_ad37_6ab9),
    ("1e-46", 0x0000_0000, 0x3662_44ce_242c_5561),
    ("1.1754942e-38", 0x007f_ffff, 0x380f_ffff_bb1d_d6a1),
    ("1.17549435e-38", 0x0080_0000, 0x380f_ffff_ff9f_dba8),
    (
        "2.2250738585072009e-308",
        0x0000_0000,
        0x000f_ffff_ffff_ffff,
    ),
    (
        "2.2250738585072014e-308",
        0x0000_0000,
        0x0010_0000_0000_0000,
    ),
    ("4.9e-324", 0x0000_0000, 0x0000_0000_0000_0001),
    // Integer literals (RON accepts them for float fields).
    ("0", 0x0000_0000, 0x0000_0000_0000_0000),
    ("1", 0x3f80_0000, 0x3ff0_0000_0000_0000),
    ("-1", 0xbf80_0000, 0xbff0_0000_0000_0000),
    ("255", 0x437f_0000, 0x406f_e000_0000_0000),
    ("16777216", 0x4b80_0000, 0x4170_0000_0000_0000),
    ("9007199254740993", 0x5a00_0000, 0x4340_0000_0000_0000),
    ("123456789", 0x4ceb_79a3, 0x419d_6f34_5400_0000),
    // Ordinary decimals.
    ("0.5", 0x3f00_0000, 0x3fe0_0000_0000_0000),
    ("1.0e10", 0x5015_02f9, 0x4202_a05f_2000_0000),
    ("0.000001", 0x3586_37bd, 0x3eb0_c6f7_a0b5_ed8d),
    ("8388608.5", 0x4b00_0000, 0x4160_0000_1000_0000),
];

#[test]
fn core_parse_matches_recorded_bits() {
    for &(literal, f32_bits, f64_bits) in TABLE {
        let a: f32 = literal.parse().unwrap();
        let b: f64 = literal.parse().unwrap();
        assert_eq!(a.to_bits(), f32_bits, "f32 {literal}");
        assert_eq!(b.to_bits(), f64_bits, "f64 {literal}");
    }
}

#[derive(Deserialize)]
struct Floats {
    a: f32,
    b: f64,
}

/// The path that data files take: `ron` parsing a typed float field.
#[test]
fn ron_typed_fields_match_recorded_bits() {
    let options = ron::Options::default();
    for &(literal, f32_bits, f64_bits) in TABLE {
        let text = format!("(a: {literal}, b: {literal})");
        let parsed: Floats = options
            .from_str(&text)
            .unwrap_or_else(|e| panic!("{literal}: {e}"));
        assert_eq!(parsed.a.to_bits(), f32_bits, "ron f32 {literal}");
        assert_eq!(parsed.b.to_bits(), f64_bits, "ron f64 {literal}");
    }
}
