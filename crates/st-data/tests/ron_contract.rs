//! Pins the behaviour of `ron` that `st-data` depends on.
//!
//! The span locator and the error mapping rely on `ron`'s error variants, on
//! its position format (`Deserializer::span_error`: 1-based line and column,
//! counted in characters) and on the syntax it accepts. If an upgrade of
//! `ron` changes any of that, a test in this file fails; re-check
//! `src/locator.rs` and `from_ron` in `src/load.rs` before adjusting a
//! constant here.

mod common;

use common::{ShipDesign, design, load_err};
use serde::Deserialize;
use st_data::{Code, Loader};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code, reason = "only parsed")]
struct Probe {
    name: String,
    mass: f32,
    #[serde(default)]
    sections: Vec<Section>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code, reason = "only parsed")]
enum Section {
    Cargo { capacity: u8 },
    Core,
}

/// What `ron` reports for a text: the error variant and the start and end
/// positions, as `Deserializer::span_error` gives them after
/// `serde_path_to_error` returns.
fn ron_error(text: &str) -> (ron::Error, (usize, usize), (usize, usize)) {
    let options = ron::Options::default();
    let mut de = ron::Deserializer::from_str_with_options(text, &options).unwrap();
    let mut track = serde_path_to_error::Track::new();
    let result: Result<Probe, ron::Error> =
        Probe::deserialize(serde_path_to_error::Deserializer::new(&mut de, &mut track));
    let err = result.unwrap_err();
    let spanned = de.span_error(err);
    let s = spanned.span;
    (
        spanned.code,
        (s.start.line, s.start.col),
        (s.end.line, s.end.col),
    )
}

#[test]
fn ron_reports_the_variants_and_positions_the_mapping_expects() {
    // Unknown field: the span covers the field name.
    let (err, start, end) = ron_error("(\n    name: \"a\",\n    mas: 1.0,\n)");
    assert!(
        matches!(err, ron::Error::NoSuchStructField { ref found, .. } if found == "mas"),
        "{err:?}"
    );
    assert_eq!((start, end), ((3, 5), (3, 8)));

    // Unknown variant.
    let (err, start, _) =
        ron_error("(name: \"a\", mass: 1.0, sections: [Cargo(capacity: 1), Reactor])");
    assert!(
        matches!(err, ron::Error::NoSuchEnumVariant { ref found, .. } if found == "Reactor"),
        "{err:?}"
    );
    assert_eq!(start, (1, 55));

    // Out-of-range integer: the span covers the number.
    let (err, start, end) = ron_error("(name: \"a\", mass: 1.0, sections: [Cargo(capacity: 300)])");
    assert_eq!(err, ron::Error::IntegerOutOfBounds);
    assert_eq!((start, end), ((1, 51), (1, 54)));

    // Missing field: reported where the struct ends.
    let (err, start, _) = ron_error("(\n    name: \"a\",\n)");
    assert!(
        matches!(err, ron::Error::MissingStructField { field: "mass", .. }),
        "{err:?}"
    );
    assert_eq!(start, (3, 1));

    // Duplicate field: the second occurrence.
    let (err, start, _) = ron_error("(name: \"a\",\n mass: 1.0,\n mass: 2.0)");
    assert!(
        matches!(err, ron::Error::DuplicateStructField { field: "mass", .. }),
        "{err:?}"
    );
    assert_eq!(start.0, 3);

    // A wrong type.
    let (err, start, _) = ron_error("(name: \"a\", mass: \"x\")");
    assert_eq!(err, ron::Error::ExpectedFloat);
    // `ron` points at the end of the previous token (the space after the colon).
    assert_eq!(start, (1, 18));

    // A struct name that differs from the Rust name is an error; the loader's
    // two passes therefore share the name `Envelope`.
    let (err, ..) = ron_error("Other(name: \"a\", mass: 1.0)");
    assert!(
        matches!(
            err,
            ron::Error::ExpectedDifferentStructName {
                expected: "Probe",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn ron_columns_count_characters_not_bytes() {
    // Two 2-byte characters before the unknown field.
    let (_, start, _) = ron_error("(name: \"\u{e9}\u{e9}\", mas: 1.0)");
    assert_eq!(start, (1, 14));
    // Tabs and carriage returns are one column each; lines split on `\n`.
    let (_, start, _) = ron_error("(\r\n\tname: \"a\", mas: 1.0)");
    assert_eq!(start, (2, 13));
}

#[test]
fn ron_accepts_and_the_locator_walks_the_same_syntax() {
    // Everything the locator tokenizes, in one file, with a validation error
    // at the end that only the locator can position.
    let text = "#![enable(implicit_some)]\n\
        // line comment\n\
        /* block /* nested */ comment */\n\
        Envelope(\n\
        kind: \"ship_design\", version: 3,\n\
        data: ShipDesign(\n\
        name: r#\"raw \"quoted\" ) string\"#,\n\
        mass: -2.5e0, // negative\n\
        sections: [Core, Cargo(capacity: 7)],\n\
        tags: ['a'.to_string_placeholder_is_not_valid_ron_so_use_strings],\n\
        ),\n\
        )";
    // The last field is not valid RON for a `Vec<String>`; replace it.
    let text = text.replace(
        "tags: ['a'.to_string_placeholder_is_not_valid_ron_so_use_strings],",
        "tags: [\"x\"],",
    );
    let err = Loader::new()
        .load::<ShipDesign>("designs/syntax.ron", text.as_bytes())
        .unwrap_err();
    assert_eq!(err.len(), 1, "{err:?}");
    assert_eq!(err[0].code, Code::Validation);
    // `mass: -2.5e0` is on line 8; the locator finds the value after the key.
    let span = err[0].span.unwrap();
    assert_eq!((span.start.line, span.start.col), (8, 7));
    assert_eq!((span.end.line, span.end.col), (8, 13));
}

/// The locator and `ron` must agree on where the same value is: a type error
/// positioned by `ron` and a validation error positioned by the locator, in
/// files laid out identically, start at the same line and column.
#[test]
fn locator_and_ron_agree_on_positions() {
    let layout = |value: &str| {
        design(&format!(
            "(\n        name: \"S\",\n        mass: 1.0,\n        sections: [\n            Core,\n            Cargo(capacity: {value}),\n        ],\n    )"
        ))
    };
    // `ron` positions the out-of-range integer ...
    let by_ron = &load_err(&layout("5000000000"))[0];
    assert_eq!(by_ron.code, Code::Type);
    // ... the locator positions the validation failure (capacity > 1000).
    let by_locator = &load_err(&layout("5000"))[0];
    assert_eq!(by_locator.code, Code::Validation);
    assert_eq!(by_ron.path, by_locator.path);
    let a = by_ron.span.unwrap().start;
    let b = by_locator.span.unwrap().start;
    assert_eq!((a.line, a.col), (9, 29));
    assert_eq!((b.line, b.col), (9, 29));
    // Both cover exactly the number: `5000000000` is ten characters, `5000` four.
    assert_eq!(by_ron.span.unwrap().end.col, 29 + 10);
    assert_eq!(by_locator.span.unwrap().end.col, 29 + 4);
}
