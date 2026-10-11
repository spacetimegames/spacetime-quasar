//! The `SIM_VERSION` pin (ADR 0009, "Version policy").
//!
//! `sim-version.txt` is append-only. Each line is
//! `<sim version> <FNV-1a 64 of the pinned lines> <FNV-1a 64 of the wire golden bytes>`,
//! both hashes as 16 lower-case hex digits. The pinned lines are the lines of
//! `reference.txt` that belong to a code-defined scenario (every scenario
//! except `data-fixtures`, which loads data and is guarded by the `st-data`
//! golden tests) and their `.replay` lines, joined in file order with LF. The
//! last line must match `st_sim::SIM_VERSION` and both current hashes, so a
//! change of behaviour or encoding cannot merge without a new line and a new
//! `SIM_VERSION`.

use determinism::replays::fnv1a64;

const REFERENCE: &str = include_str!("../reference.txt");
const PIN: &str = include_str!("../sim-version.txt");

/// Scenarios whose lines do not move the pin: their content is data, not
/// simulation behaviour.
const UNPINNED: [&str; 1] = ["data-fixtures"];

fn pinned_lines() -> String {
    let lines: Vec<&str> = REFERENCE
        .lines()
        .filter(|line| {
            let name = line.split(' ').next().unwrap_or("");
            let scenario = name.strip_suffix(".replay").unwrap_or(name);
            !UNPINNED.contains(&scenario)
        })
        .collect();
    lines.join("\n")
}

fn current() -> (u32, String, String) {
    (
        st_sim::SIM_VERSION,
        format!("{:016x}", fnv1a64(pinned_lines().as_bytes())),
        format!("{:016x}", fnv1a64(&st_sim::command::wire::golden_bytes())),
    )
}

fn parse(line: &str) -> (u32, &str, &str) {
    let f: Vec<&str> = line.split(' ').collect();
    assert_eq!(f.len(), 3, "bad pin line `{line}`");
    for hash in &f[1..] {
        assert!(
            hash.len() == 16 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "bad hash in `{line}`"
        );
    }
    (f[0].parse().expect("a version number"), f[1], f[2])
}

#[test]
fn sim_version_pin() {
    let (version, lines_hash, golden_hash) = current();
    let last = PIN.lines().last().expect("sim-version.txt has a line");
    let (pinned, pinned_lines_hash, pinned_golden_hash) = parse(last);
    assert_eq!(
        pinned, version,
        "the last line of sim-version.txt is for simulation version {pinned}, but \
         st_sim::SIM_VERSION is {version}"
    );
    assert_eq!(
        pinned_lines_hash, lines_hash,
        "the pinned lines of reference.txt changed without a new SIM_VERSION: bump it and \
         append `{version} {lines_hash} {golden_hash}` to tools/determinism/sim-version.txt"
    );
    assert_eq!(
        pinned_golden_hash, golden_hash,
        "the wire golden bytes changed without a new SIM_VERSION: bump it and append \
         `{version} {lines_hash} {golden_hash}` to tools/determinism/sim-version.txt"
    );
}

#[test]
fn the_pin_file_is_append_only_in_form() {
    assert!(PIN.ends_with('\n'));
    assert!(!PIN.contains('\r'));
    let versions: Vec<u32> = PIN.lines().map(|l| parse(l).0).collect();
    assert_eq!(versions.first(), Some(&1));
    assert!(
        versions.windows(2).all(|w| w[0] < w[1]),
        "versions must increase from line to line: {versions:?}"
    );
}

#[test]
fn the_pinned_lines_include_replays_and_leave_out_the_data_fixtures() {
    let lines = pinned_lines();
    assert!(lines.contains("solo-burn.replay "));
    assert!(lines.contains("swarm-controls.replay "));
    assert!(lines.contains("math-kernels "));
    assert!(!lines.contains("data-fixtures"));
    assert!(!lines.ends_with('\n'));
}
