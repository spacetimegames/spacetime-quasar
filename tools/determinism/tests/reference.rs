//! The scenarios' hashes must equal the committed reference on every platform.
//!
//! `cargo test` runs this on Linux, Windows, macOS and, under wasmtime, on
//! `wasm32-wasip1`. A PR that changes simulation behaviour on purpose
//! regenerates `reference.txt` and says why in its description (see
//! `docs/62-determinism-harness.adoc`).

const REFERENCE: &str = include_str!("../reference.txt");

#[test]
fn all_scenarios_match_the_reference() {
    let actual = determinism::run(None, 120).unwrap();
    if actual != REFERENCE {
        let first = actual
            .lines()
            .zip(REFERENCE.lines())
            .find(|(a, r)| a != r)
            .map_or_else(
                || "(different length)".to_owned(),
                |(a, r)| format!("got `{a}`, want `{r}`"),
            );
        panic!("determinism output differs from tools/determinism/reference.txt: {first}");
    }
}

#[test]
fn the_reference_is_sorted_with_a_final_newline() {
    let mut lines: Vec<_> = REFERENCE.lines().collect();
    let before = lines.clone();
    lines.sort_unstable();
    assert_eq!(lines, before);
    assert!(REFERENCE.ends_with('\n'));
    assert!(!REFERENCE.contains('\r'));
}

#[test]
fn a_finer_interval_adds_lines_and_keeps_the_coarse_ones() {
    let fine = determinism::run(Some("solo-burn"), 60).unwrap();
    for line in REFERENCE.lines().filter(|l| l.starts_with("solo-burn ")) {
        assert!(fine.lines().any(|f| f == line), "missing `{line}`");
    }
    assert!(fine.lines().count() > 6);
}

#[test]
fn unknown_scenario_is_an_error() {
    assert!(matches!(
        determinism::run(Some("nope"), 120),
        Err(determinism::Error::UnknownScenario(_))
    ));
}
