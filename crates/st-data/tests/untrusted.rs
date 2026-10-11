//! Untrusted input: truncated, oversized, deeply nested, non-UTF-8 and
//! mutated files produce diagnostics, never a panic.

mod common;

use common::{ShipDesign, design};
use st_data::{Code, Loader};

const FIXTURE: &str = include_str!("data/ship_design/v3/shuttle.ron");

fn load(bytes: &[u8]) -> Result<(), Vec<st_data::Diagnostic>> {
    Loader::new()
        .load::<ShipDesign>("untrusted.ron", bytes)
        .map(|_| ())
}

#[test]
fn every_truncation_of_a_valid_file_is_handled() {
    let bytes = FIXTURE.as_bytes();
    assert!(load(bytes).is_ok());
    let mut errors = 0;
    for len in 0..bytes.len() {
        // Cutting inside a multi-byte character gives invalid UTF-8; that is
        // also an input the loader must survive.
        match load(&bytes[..len]) {
            Ok(()) => {}
            Err(diagnostics) => {
                errors += 1;
                assert!(!diagnostics.is_empty());
                for d in &diagnostics {
                    assert_eq!(d.source, "untrusted.ron");
                    let _ = d.to_string();
                }
            }
        }
    }
    // Only the whole file, or trailing whitespace and comments cut off, loads.
    assert!(errors > bytes.len() / 2);
}

#[test]
fn single_byte_mutations_are_handled() {
    let original = FIXTURE.as_bytes();
    for i in 0..original.len() {
        for replacement in [0u8, b'(', b')', b'"', b',', 0xff, b'/', b'*'] {
            let mut bytes = original.to_vec();
            bytes[i] = replacement;
            let _ = load(&bytes);
        }
    }
}

#[test]
fn pseudo_random_bytes_are_handled() {
    // A fixed linear congruential generator keeps the test reproducible.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as u8
    };
    for round in 0..200 {
        let len = (round * 7) % 300;
        let bytes: Vec<u8> = (0..len).map(|_| next()).collect();
        assert!(load(&bytes).is_err());
    }
}

#[test]
fn oversized_input_is_refused_before_parsing() {
    let big = vec![b' '; 1024];
    let err = Loader::new()
        .with_max_bytes(1023)
        .load::<ShipDesign>("big.ron", &big)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(err.len(), 1);
    assert_eq!(err[0].code, Code::TooLarge);
    assert!(err[0].span.is_none());

    // At the limit it is parsed (and fails: there is only whitespace).
    let err = Loader::new()
        .with_max_bytes(1024)
        .load::<ShipDesign>("big.ron", &big)
        .map(|_| ())
        .unwrap_err();
    assert_ne!(err[0].code, Code::TooLarge);

    // The default limit is 16 MiB.
    assert_eq!(st_data::DEFAULT_MAX_BYTES, 16 * 1024 * 1024);
    let huge = vec![b' '; st_data::DEFAULT_MAX_BYTES + 1];
    assert_eq!(load(&huge).unwrap_err()[0].code, Code::TooLarge);
}

#[test]
fn bytes_that_are_not_utf8_are_refused_with_a_position() {
    let mut bytes = b"(\n  kind: \"ship_design\", // \xff\n".to_vec();
    bytes.extend_from_slice(b"version: 3, data: ())");
    let err = load(&bytes).unwrap_err();
    assert_eq!(err.len(), 1);
    assert_eq!(err[0].code, Code::NotUtf8);
    let span = err[0].span.unwrap();
    assert_eq!((span.start.line, span.start.col), (2, 27));

    // A lone continuation byte, an overlong sequence and a truncated sequence.
    for bad in [&b"\x80"[..], b"\xc0\xaf", b"(\xe2\x82"] {
        assert_eq!(load(bad).unwrap_err()[0].code, Code::NotUtf8);
    }
}

#[test]
fn deep_nesting_is_refused_not_overflowed() {
    for open_close in [("(", ")"), ("[", "]"), ("{", "}"), ("Some(", ")")] {
        for depth in [129usize, 1_000, 100_000] {
            let nested = format!(
                "(kind: \"ship_design\", version: 3, data: {}{})",
                open_close.0.repeat(depth),
                open_close.1.repeat(depth)
            );
            let err = load(nested.as_bytes()).unwrap_err();
            assert_eq!(err.len(), 1);
            assert_eq!(err[0].code, Code::Syntax, "{} x {depth}", open_close.0);
        }
    }
    // Deep nesting in the envelope itself.
    let deep = "(".repeat(100_000);
    assert_eq!(load(deep.as_bytes()).unwrap_err()[0].code, Code::Syntax);
    // Nesting within the limit is only a type error for this schema.
    let ok_depth = design(&format!(
        "(name: \"a\", mass: 1.0, tags: {}{})",
        "[".repeat(50),
        "]".repeat(50)
    ));
    assert_eq!(load(ok_depth.as_bytes()).unwrap_err()[0].code, Code::Type);
}

#[test]
fn many_diagnostics_are_bounded_work() {
    // Thousands of failing sections: every failure is reported, and only the
    // first few hundred are located in the text.
    let sections: Vec<String> = (0..2_000)
        .map(|_| "Cargo(capacity: 5000)".to_owned())
        .collect();
    let text = design(&format!(
        "(name: \"a\", mass: 1.0, sections: [{}])",
        sections.join(", ")
    ));
    let err = load(text.as_bytes()).unwrap_err();
    assert_eq!(err.len(), 2_000);
    assert!(err[0].span.is_some());
    assert!(err[1_999].span.is_none());
}

#[test]
fn the_empty_input_and_whitespace_are_errors() {
    assert!(load(b"").is_err());
    assert!(load(b"   \n\t ").is_err());
    assert!(load(b"// only a comment").is_err());
    assert!(Loader::new().peek_header("a.ron", b"").is_err());
}

#[test]
fn peek_header_reads_kind_and_version_only() {
    let header = Loader::new()
        .peek_header(
            "a.ron",
            b"(kind: \"x.y\", version: 7, data: (anything: [1, 2]), extra: ())",
        )
        .unwrap();
    assert_eq!((header.kind.as_str(), header.version), ("x.y", 7));
}
