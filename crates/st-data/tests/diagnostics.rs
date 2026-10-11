//! Diagnostics: codes, positions, paths, hints and the one-line rendering.

mod common;

use common::{Catalog, ShipDesign, design, load_err};
use serde::{Deserialize, Serialize};
use st_data::{
    Code, DataPath, Genesis, Loader, MigrationError, Schema, Validate, Validator, check_chain,
};

fn one(text: &str) -> st_data::Diagnostic {
    let mut all = load_err(text);
    assert_eq!(all.len(), 1, "{all:?}");
    all.remove(0)
}

fn at(d: &st_data::Diagnostic) -> (u32, u32) {
    let span = d.span.expect("a span");
    (span.start.line, span.start.col)
}

#[test]
fn unknown_field_has_position_path_hint_and_a_one_line_rendering() {
    let d = one(&design(
        "(\n        name: \"S\",\n        mas: 12.5,\n    )",
    ));
    assert_eq!(d.code, Code::UnknownField);
    assert_eq!(at(&d), (6, 9));
    assert_eq!(d.path.to_string(), "mas");
    assert_eq!(d.help.as_deref(), Some("did you mean `mass`?"));
    assert_eq!(
        d.to_string(),
        "error[unknown-field]: designs/t.ron:6:9: mas: unknown field `mas`; \
         expected one of `name`, `mass`, `armor`, `sections`, `tags`"
    );
    assert!(format!("{d:#}").ends_with("\n  help: did you mean `mass`?"));
    assert!(!d.to_string().contains('\n'));
}

#[test]
fn columns_count_unicode_scalar_values() {
    let text = "(\n  kind: \"ship_design\", version: 3, data: (name: \"\u{e9}\u{e9}\", mass: 1.0, mas: 1))";
    // `mas` starts after 65 characters on line 2 (66 bytes would be 68).
    assert_eq!(at(&one(text)), (2, 66));
}

#[test]
fn missing_and_duplicate_fields() {
    let d = one(&design("(\n        name: \"S\",\n    )"));
    assert_eq!(d.code, Code::MissingField);
    assert!(d.message.contains("`mass`"));
    assert_eq!(d.help.as_deref(), Some("add `mass: ...` to this struct"));

    let d = one(&design(
        "(\n        name: \"S\",\n        mass: 1.0,\n        mass: 2.0,\n    )",
    ));
    assert_eq!(d.code, Code::DuplicateField);
    assert_eq!(at(&d).0, 7);
}

#[test]
fn unknown_variant_lists_the_expected_ones_and_suggests() {
    let d = one(&design(
        "(\n name: \"S\", mass: 1.0,\n sections: [Cargo(capacity: 1), Engine(0.5), Engin(1.0)],\n    )",
    ));
    assert_eq!(d.code, Code::UnknownVariant);
    assert_eq!(d.path.to_string(), "sections[2]");
    assert!(d.message.contains("`Cargo`, `Engine`, `Core`"));
    assert_eq!(d.help.as_deref(), Some("did you mean `Engine`?"));
}

#[test]
fn type_errors_are_located_on_the_value() {
    let d = one(&design(
        "(\n name: \"S\", mass: 1.0,\n sections: [Cargo(capacity: 99999999999)],\n    )",
    ));
    assert_eq!(d.code, Code::Type);
    assert_eq!(d.path.to_string(), "sections[0].Cargo.capacity");
    assert_eq!(at(&d), (6, 29));

    let d = one(&design("(\n name: \"S\", mass: \"heavy\",\n    )"));
    assert_eq!(d.code, Code::Type);
    assert_eq!(d.path.to_string(), "mass");
    assert_eq!(at(&d), (5, 18));
}

#[test]
fn syntax_errors_carry_a_position() {
    let d = one("(\n kind: \"ship_design\"\n version: 3,\n data: ())");
    assert_eq!(d.code, Code::Syntax);
    assert_eq!(at(&d).0, 3);
    let d = one(&(design("()") + " x"));
    assert_eq!(d.code, Code::Syntax);
    assert_eq!(at(&d), (5, 2));
}

#[test]
fn header_errors() {
    let d = one(r#"(kind: "other", version: 3, data: ())"#);
    assert_eq!(d.code, Code::WrongKind);
    assert_eq!(at(&d), (1, 8));

    let d = one(r#"(kind: "ship_design", version: 0, data: ())"#);
    assert_eq!(d.code, Code::Type);

    let d = one(r#"(kind: "ship_design", version: 3)"#);
    assert_eq!(d.code, Code::MissingField);

    let d = one(r#"(kind: 5, version: 3, data: ())"#);
    assert_eq!(d.code, Code::Type);
    assert_eq!(d.path.to_string(), "kind");
}

#[test]
fn newer_files_are_refused_even_with_unknown_header_fields() {
    // The header pass ignores unknown header fields, so a later ADR can add
    // some without breaking `version-too-new`.
    let d = one(r#"(kind: "ship_design", version: 4, extra: 1, data: (anything: 1))"#);
    assert_eq!(d.code, Code::VersionTooNew);
    assert_eq!(at(&d), (1, 32));
    assert!(d.message.contains("version 4") && d.message.contains("up to version 3"));

    // At the right version the strict pass rejects the header field.
    let d = one(r#"(kind: "ship_design", version: 3, extra: 1, data: (name: "a", mass: 1.0))"#);
    assert_eq!(d.code, Code::UnknownField);
    assert_eq!(at(&d), (1, 35));
}

// A kind whose oldest supported version is 2.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Old {
    a: u32,
}
impl Schema for Old {
    const KIND: &'static str = "windowed";
    const VERSION: u32 = 2;
    type Previous = Genesis;
    fn migrate(p: Genesis) -> Result<Self, MigrationError> {
        match p {}
    }
}

#[test]
fn files_older_than_the_oldest_supported_version_are_refused() {
    let err = Loader::new()
        .load_unvalidated::<Old>("x.ron", br#"(kind: "windowed", version: 1, data: (a: 1))"#)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(err[0].code, Code::VersionTooOld);
    assert!(err[0].message.contains("oldest supported version is 2"));
}

#[test]
fn non_finite_floats_are_rejected_with_their_position() {
    for literal in ["NaN", "inf", "-inf", "3.4028237e38"] {
        let d = one(&design(&format!(
            "(name: \"S\", mass: 1.0,\n armor: {literal})"
        )));
        assert_eq!(d.code, Code::NonFinite, "{literal}");
        assert_eq!(d.path.to_string(), "armor");
        assert_eq!(at(&d), (5, 9), "{literal}");
    }
}

#[test]
fn validation_reports_every_failed_rule_with_its_position() {
    let all = load_err(&design(
        "(name: \"S\", mass: -1.0,\n sections: [Core, Cargo(capacity: 5000)])",
    ));
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].code, Code::Validation);
    assert_eq!(all[0].rule.as_deref(), Some("ship_design.mass_positive"));
    assert_eq!(at(&all[0]), (4, 29));
    assert_eq!(all[1].path.to_string(), "sections[1].Cargo.capacity");
    assert_eq!(at(&all[1]), (5, 35));
    assert!(
        all[1]
            .to_string()
            .ends_with("capacity above 1000 [ship_design.capacity]")
    );
}

#[test]
fn duplicate_map_keys_are_rejected() {
    let text =
        "(kind: \"catalog\", version: 1, data: (prices: {\n \"a\": 1,\n \"b\": 2,\n \"a\": 3,\n}))";
    let err = Loader::new()
        .load::<Catalog>("c.ron", text.as_bytes())
        .unwrap_err();
    assert_eq!(err[0].code, Code::DuplicateKey);
    assert_eq!(err[0].path.to_string(), "prices");
    assert_eq!(err[0].span.unwrap().start.line, 4);
}

// A type that forgot `deny_unknown_fields`.
#[derive(Serialize, Deserialize)]
struct Lenient {
    a: u32,
}
impl Schema for Lenient {
    const KIND: &'static str = "lenient";
    const VERSION: u32 = 1;
    type Previous = Genesis;
    fn migrate(p: Genesis) -> Result<Self, MigrationError> {
        match p {}
    }
}

#[test]
fn a_type_without_deny_unknown_fields_is_caught_by_the_backstop() {
    let err = Loader::new()
        .load_unvalidated::<Lenient>(
            "x.ron",
            b"(kind: \"lenient\", version: 1, data: (a: 1, typo: 2))",
        )
        .map(|_| ())
        .unwrap_err();
    assert_eq!(err.len(), 1);
    assert_eq!(err[0].code, Code::SchemaNotStrict);
    assert_eq!(err[0].path.to_string(), "typo");
    assert_eq!(err[0].span.unwrap().start.col, 50);
}

// A two-version kind whose migration can fail.
mod fallible {
    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct V1 {
        pub items: Vec<i64>,
    }
    impl Schema for V1 {
        const KIND: &'static str = "fallible";
        const VERSION: u32 = 1;
        type Previous = Genesis;
        fn migrate(p: Genesis) -> Result<Self, MigrationError> {
            match p {}
        }
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct V2 {
        pub items: Vec<i16>,
    }
    impl Schema for V2 {
        const KIND: &'static str = "fallible";
        const VERSION: u32 = 2;
        type Previous = V1;
        fn migrate(p: V1) -> Result<Self, MigrationError> {
            let mut items = Vec::new();
            for (i, v) in p.items.into_iter().enumerate() {
                let narrow = i16::try_from(v).map_err(|_| {
                    MigrationError::new("does not fit in 16 bits")
                        .at(DataPath::new().key("items").index(i))
                })?;
                items.push(narrow);
            }
            Ok(V2 { items })
        }
    }
    impl Validate for V2 {
        fn validate(&self, _: &mut Validator) {}
    }
}

#[test]
fn a_failed_migration_reports_the_path_of_the_offending_value() {
    let text = "(\n kind: \"fallible\",\n version: 1,\n data: (items: [1, 2, 70000]),\n)";
    let err = Loader::new()
        .load::<fallible::V2>("old/f.ron", text.as_bytes())
        .map(|_| ())
        .unwrap_err();
    assert_eq!(err.len(), 1);
    assert_eq!(err[0].code, Code::Migration);
    assert_eq!(err[0].path.to_string(), "items[2]");
    assert_eq!(err[0].span.unwrap().start.line, 4);
    assert_eq!(
        err[0].to_string(),
        "error[migration]: old/f.ron:4:23: items[2]: does not fit in 16 bits"
    );
}

#[test]
fn a_path_that_only_exists_in_the_current_version_is_noted() {
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wide {
        wide: Vec<u8>,
    }
    impl Schema for Wide {
        const KIND: &'static str = "fallible";
        const VERSION: u32 = 2;
        type Previous = fallible::V1;
        fn migrate(_: fallible::V1) -> Result<Self, MigrationError> {
            Ok(Wide { wide: vec![] })
        }
    }
    impl Validate for Wide {
        fn validate(&self, v: &mut Validator) {
            v.field("wide", |v| v.error("wide.rule", "always fails"));
        }
    }
    let err = Loader::new()
        .load::<Wide>(
            "old/w.ron",
            br#"(kind: "fallible", version: 1, data: (items: []))"#,
        )
        .map(|_| ())
        .unwrap_err();
    assert_eq!(err[0].span, None);
    let help = err[0].help.as_deref().unwrap();
    assert!(
        help.contains("version 1") && help.contains("version 2"),
        "{help}"
    );
}

#[test]
fn schema_chains_are_checked() {
    assert!(check_chain::<ShipDesign>().is_ok());
    assert!(check_chain::<fallible::V2>().is_ok());
    assert!(check_chain::<Old>().is_ok());
    // `Genesis` alone is not a schema.
    assert!(check_chain::<Genesis>().is_err());

    // Mixed kinds in a chain are found.
    #[derive(Serialize, Deserialize)]
    struct Other;
    impl Schema for Other {
        const KIND: &'static str = "something_else";
        const VERSION: u32 = 2;
        type Previous = fallible::V1;
        fn migrate(_: fallible::V1) -> Result<Self, MigrationError> {
            Ok(Other)
        }
    }
    let e = check_chain::<Other>().unwrap_err();
    assert_eq!(e.version, 1);
    assert!(e.message.contains("kind"), "{e}");
}

#[test]
fn diagnostic_builders_and_codes() {
    use st_data::Diagnostic;
    let d = Diagnostic::new(
        "assets/a.ron",
        Code::OutdatedAsset,
        "file is version 2, current is 3",
    );
    assert_eq!(
        d.to_string(),
        "error[outdated-asset]: assets/a.ron: file is version 2, current is 3"
    );
    let all = [
        (Code::TooLarge, "too-large"),
        (Code::NotUtf8, "not-utf8"),
        (Code::Syntax, "syntax"),
        (Code::Type, "type"),
        (Code::UnknownField, "unknown-field"),
        (Code::MissingField, "missing-field"),
        (Code::DuplicateField, "duplicate-field"),
        (Code::DuplicateKey, "duplicate-key"),
        (Code::UnknownVariant, "unknown-variant"),
        (Code::WrongKind, "wrong-kind"),
        (Code::UnknownKind, "unknown-kind"),
        (Code::VersionTooNew, "version-too-new"),
        (Code::VersionTooOld, "version-too-old"),
        (Code::SchemaNotStrict, "schema-not-strict"),
        (Code::NonFinite, "non-finite"),
        (Code::Migration, "migration"),
        (Code::Validation, "validation"),
        (Code::OutdatedAsset, "outdated-asset"),
        (Code::MissingSource, "missing-source"),
    ];
    for (code, text) in all {
        assert_eq!(code.as_str(), text);
        assert_eq!(code.to_string(), text);
    }
}
