//! Golden vectors for `st-rng`.
//!
//! These outputs are frozen. They were produced by an independent
//! implementation of the documented algorithms (SplitMix64 finaliser, FNV-1a
//! 64, Lemire bounded draws) and currently run in CI on Linux, macOS and
//! Windows. Execution on `wasm32` will come with the determinism harness
//! (wasmtime on `wasm32-wasip1`, ADR 0008); until then the `wasm32` CI job
//! only builds the crate. If a
//! test here fails, the algorithm or a constant changed, which changes every
//! outcome derived from a seed: that is a simulation-version change.

use st_rng::{Cursor, Stream};

#[test]
fn root_streams_raw_values() {
    // (seed, [counter 0, 1, 2, u64::MAX])
    let cases: [(u64, [u64; 4]); 4] = [
        (
            0,
            [
                0xa370_5447_744b_9bdc,
                0x73fe_d830_d184_499b,
                0xa211_7c4b_cf29_ec1e,
                0xde56_b0a5_fb0f_9a2e,
            ],
        ),
        (
            1,
            [
                0x815b_fa05_4fd6_abdf,
                0xa5b2_684d_9972_d71a,
                0x3dbd_a0b2_4960_8784,
                0x1855_dabc_524a_3eb2,
            ],
        ),
        (
            0xC0FFEE,
            [
                0xd7ab_ed95_7ac8_485a,
                0x9df8_42e5_351d_b0ed,
                0x987b_9313_d218_dd93,
                0xadaa_4692_0c50_983b,
            ],
        ),
        (
            u64::MAX,
            [
                0x69f7_37c6_0335_f15c,
                0x5565_a00f_7c99_14bf,
                0x358c_b04e_b603_1786,
                0xeb15_84ed_4bd7_7fb2,
            ],
        ),
    ];
    for (seed, want) in cases {
        let s = Stream::from_seed(seed);
        let got = [s.u64_at(0), s.u64_at(1), s.u64_at(2), s.u64_at(u64::MAX)];
        assert_eq!(got, want, "seed {seed:#x}");
    }
}

/// seed 0xC0FFEE -> "economy" -> 7 -> 1200
fn decision() -> Stream {
    Stream::from_seed(0xC0FFEE)
        .derive_label("economy")
        .derive(7)
        .derive(1200)
}

#[test]
fn derived_path_raw_values() {
    let s = decision();
    assert_eq!(s.u64_at(0), 0x2a41_88b5_4a9a_bc1b);
    assert_eq!(s.u64_at(1), 0xe084_ff74_afc9_170a);
    assert_eq!(s.u64_at(2), 0xd8f7_cba0_1627_4fd6);
}

#[test]
fn derivation_order_and_domain_matter() {
    let root = Stream::from_seed(1);
    let ab = root.derive(1).derive(2);
    let ba = root.derive(2).derive(1);
    assert_eq!(
        ab.u64_at(0),
        Stream::from_seed(1).derive(1).derive(2).u64_at(0)
    );
    assert_ne!(ab.u64_at(0), ba.u64_at(0));
    // Empty and non-empty labels are valid and distinct.
    assert_ne!(
        root.derive_label("").u64_at(0),
        root.derive_label("combat").u64_at(0)
    );
}

#[test]
fn bounded_golden() {
    let s = decision();
    let six: [u64; 8] = [0, 5, 5, 2, 5, 2, 2, 3];
    for (c, want) in six.iter().enumerate() {
        assert_eq!(s.below_at(c as u64, 6), *want, "below 6 at {c}");
    }
    let wide: [u64; 4] = [
        165_062_469_730,
        877_029_386_508,
        847_531_057_914,
        399_938_489_281,
    ];
    for (c, want) in wide.iter().enumerate() {
        assert_eq!(s.below_at(c as u64, 1_000_000_000_000), *want);
    }
}

#[test]
fn bounded_rejection_path_golden() {
    // Counter 0 of this stream is rejected for this bound, so the first
    // value exercises the retry sub-stream.
    let s = decision();
    let bound = (1u64 << 63) + 12345;
    let want: [u64; 6] = [
        0x42b7_cb98_349c_d115,
        0x7042_7fba_57e4_b5cf,
        0x6c7b_e5d0_0b13_d0c9,
        0x3331_2f35_efe3_f268,
        0x744c_b831_3f4b_cfb8,
        0x3029_36dd_8599_d14b,
    ];
    for (c, w) in want.iter().enumerate() {
        assert_eq!(s.below_at(c as u64, bound), *w, "counter {c}");
    }
}

#[test]
fn float_golden_bits() {
    let s = decision();
    let want: [u64; 3] = [
        0x3fc5_20c4_5aa5_4d5c,
        0x3fec_109f_ee95_f922,
        0x3feb_1ef9_7402_c4e9,
    ];
    for (c, w) in want.iter().enumerate() {
        assert_eq!(s.f64_at(c as u64).to_bits(), *w, "counter {c}");
    }
}

#[test]
fn cursor_reproduces_golden() {
    let mut c = Cursor::at(decision(), 0);
    assert_eq!(c.next_u64(), 0x2a41_88b5_4a9a_bc1b);
    assert_eq!(c.next_u64(), 0xe084_ff74_afc9_170a);
    assert_eq!(c.counter(), 2);
}

#[test]
fn non_ascii_label_golden() {
    // Labels hash their UTF-8 bytes, so non-ASCII text is well defined.
    // Expected values come from an independent BigInt implementation.
    let label = "\u{e9}conomie\u{2192}\u{706b}\u{661f}"; // "économie→火星"
    assert_eq!(st_rng::label_hash(label), 0x135a_d508_bb29_075d);
    let s = Stream::from_seed(1).derive_label(label);
    assert_eq!(s.u64_at(0), 0x10b6_5c6f_df4b_08ea);
    assert_eq!(s.u64_at(1), 0x3191_b73c_fd1e_a6be);
}
