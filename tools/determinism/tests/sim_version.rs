//! The `SIM_VERSION` pin (ADR 0009, "Version policy").
//!
//! `sim-version.txt` is append-only. Each line is
//! `<sim version> <FNV-1a 64 of the pinned lines> <FNV-1a 64 of the wire golden bytes>`,
//! both hashes as 16 lower-case hex digits. The pinned lines are the lines of
//! `reference.txt` of the three scenarios ADR 0009 names (`physics-swarm`,
//! `math-kernels`, `combat-60s`) and their `.replay` lines, joined in file order with LF. The
//! last line must match `st_sim::SIM_VERSION` and both current hashes, so a
//! change of behaviour or encoding cannot merge without a new line and a new
//! `SIM_VERSION`.

use determinism::replays::fnv1a64;

const REFERENCE: &str = include_str!("../reference.txt");
const PIN: &str = include_str!("../sim-version.txt");

/// The scenarios whose lines are pinned (ADR 0009, "Version policy"). The others are
/// `data-fixtures`, whose content is data and is guarded by the `st-data` golden tests,
/// and the three interim scenarios (`solo-burn`, `orbit-coast`, `swarm-controls`), which
/// keep the harness quick to localise a divergence but are not part of the replay contract.
const PINNED: [&str; 3] = ["combat-60s", "math-kernels", "physics-swarm"];

fn pinned_lines() -> String {
    let lines: Vec<&str> = REFERENCE
        .lines()
        .filter(|line| {
            let name = line.split(' ').next().unwrap_or("");
            let scenario = name.strip_suffix(".replay").unwrap_or(name);
            PINNED.contains(&scenario)
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
fn the_pinned_lines_are_the_three_named_scenarios_and_their_replays() {
    let lines = pinned_lines();
    assert!(lines.contains("physics-swarm "));
    assert!(lines.contains("physics-swarm.replay "));
    assert!(lines.contains("combat-60s "));
    assert!(lines.contains("combat-60s.replay "));
    assert!(lines.contains("math-kernels "));
    assert!(!lines.contains("data-fixtures"));
    for interim in ["solo-burn", "orbit-coast", "swarm-controls"] {
        assert!(!lines.contains(interim), "{interim} is not pinned");
    }
    assert!(!lines.ends_with('\n'));
}
