//! The binary's command line, exit statuses and `--dump-tick` output
//! (ADR 0008, "Determinism harness").
//!
//! Not run on `wasm32-wasip1`: the test would have to spawn the binary.
#![cfg(not(target_family = "wasm"))]

use std::path::PathBuf;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_determinism"))
        .args(args)
        .output()
        .expect("the harness binary starts")
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("determinism-cli-{}-{name}", std::process::id()))
}

#[test]
fn usage_errors_exit_with_1() {
    let out = temp("usage-out.txt");
    let out = out.to_str().unwrap();
    for args in [
        &[][..],
        &["--out"],
        &["--bogus", "x"],
        // `--dump-tick` and `--dump` go together.
        &["--out", out, "--dump-tick", "5"],
        &["--out", out, "--dump", "x"],
        &["--out", out, "--dump-tick", "five", "--dump", "x"],
        // No scenario reaches this tick.
        &["--out", out, "--dump-tick", "99999", "--dump", "x"],
        &["--out", out, "--scenario", "nope"],
        &["--out", out, "--checkpoint-every", "0"],
        &["--out", out, "--replay"],
    ] {
        assert_eq!(run(args).status.code(), Some(1), "{args:?}");
    }
}

#[test]
fn an_unwritable_output_exits_with_1() {
    let missing = temp("missing-dir").join("x.txt");
    let out = run(&[
        "--out",
        missing.to_str().unwrap(),
        "--scenario",
        "solo-burn",
    ]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn dump_tick_writes_one_hex_line_per_section_and_entity() {
    let report = temp("dump-report.txt");
    let dump = temp("dump.txt");
    let out = run(&[
        "--out",
        report.to_str().unwrap(),
        "--scenario",
        "solo-burn",
        "--dump-tick",
        "240",
        "--dump",
        dump.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = std::fs::read_to_string(&dump).unwrap();
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_file(&dump);

    assert_eq!(text, determinism::dump(Some("solo-burn"), 240).unwrap());
    assert!(text.ends_with('\n'));
    for line in text.lines() {
        let f: Vec<_> = line.split(' ').collect();
        assert_eq!(f.len(), 5, "{line}");
        assert_eq!((f[0], f[1]), ("solo-burn", "00000240"));
        assert!(
            f[4].bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }
    let sections: Vec<_> = text.lines().map(|l| l.split(' ').nth(2).unwrap()).collect();
    assert_eq!(
        sections,
        ["STQH", "ENTS", "ENTS", "PILT", "PILT", "PILT", "GLOB"]
    );
}

/// The exit-3 path: a platform libm call outside the allow-list fails the run
/// after the result file has been written. The hook is in the binary because
/// the simulation itself must never make such a call.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_platform_libm_call_exits_with_3() {
    let report = temp("leak-self-test.txt");
    let out = Command::new(env!("CARGO_BIN_EXE_determinism"))
        .args(["--out", report.to_str().unwrap(), "--scenario", "solo-burn"])
        .env("DETERMINISM_LEAK_SELF_TEST", "1")
        .output()
        .expect("the harness binary starts");
    let written = report.exists();
    let _ = std::fs::remove_file(&report);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(written, "the result file is written before the verdict");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("leak-detector: cos 1 LEAK"), "{stdout}");
    assert!(stdout.contains("leak-detector: FAIL"), "{stdout}");
}

#[test]
fn a_clean_run_reports_the_leak_detector_on_x86_64_linux() {
    let report = temp("leak-report.txt");
    let out = run(&["--out", report.to_str().unwrap(), "--scenario", "solo-burn"]);
    let _ = std::fs::remove_file(&report);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        assert!(stdout.contains("leak-detector: ok"), "{stdout}");
    } else {
        assert!(stdout.is_empty(), "{stdout}");
    }
}

// ---- Replays: `--replay`, `--force` and exit status 4 (ADR 0009) -------------

/// Records a scenario to a replay file in the temp directory.
fn write_replay(name: &str, ticks: u64, edit: impl FnOnce(&mut Vec<u8>)) -> PathBuf {
    let mut s = st_sim::scenarios::solo_burn();
    s.ticks = ticks;
    s.commands.retain(|c| c.tick < ticks);
    let mut bytes = determinism::replays::record(&s).unwrap().bytes;
    edit(&mut bytes);
    let path = temp(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Recomputes the check of every chunk.
fn reseal(bytes: &mut [u8]) {
    let mut pos = st_sim::replay::MAGIC.len();
    while pos + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8 + len;
        if body + 8 > bytes.len() {
            return;
        }
        let check = determinism::replays::fnv1a64(&bytes[pos..body]);
        bytes[body..body + 8].copy_from_slice(&check.to_le_bytes());
        pos = body + 8;
    }
}

#[test]
fn a_replay_file_is_played_as_a_scenario_named_after_its_stem() {
    let replay = write_replay("good.stqreplay", 250, |_| {});
    let report = temp("good-report.txt");
    let out = run(&[
        "--out",
        report.to_str().unwrap(),
        "--replay",
        replay.to_str().unwrap(),
    ]);
    let text = std::fs::read_to_string(&report).unwrap();
    let _ = std::fs::remove_file(&replay);
    let _ = std::fs::remove_file(&report);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let want: Vec<String> = text
        .lines()
        .map(|l| l.split(' ').take(2).collect::<Vec<_>>().join(" "))
        .collect();
    let stem = format!("determinism-cli-{}-good", std::process::id());
    assert_eq!(
        want,
        [
            format!("{stem} 00000000"),
            format!("{stem} 00000120"),
            format!("{stem} 00000240"),
            format!("{stem} 00000250"),
        ]
    );
    // The hashes are the scenario's own.
    let reference = determinism::run(Some("solo-burn"), 120).unwrap();
    for tick in ["00000000", "00000120", "00000240"] {
        let hash = |t: &str| {
            t.lines()
                .find(|l| l.split(' ').nth(1) == Some(tick))
                .and_then(|l| l.split(' ').nth(2))
                .map(str::to_owned)
        };
        assert_eq!(hash(&text), hash(&reference), "tick {tick}");
    }
}

#[test]
fn a_replay_can_report_a_hash_for_every_tick() {
    let replay = write_replay("fine.stqreplay", 130, |_| {});
    let report = temp("fine-report.txt");
    let out = run(&[
        "--out",
        report.to_str().unwrap(),
        "--replay",
        replay.to_str().unwrap(),
        "--checkpoint-every",
        "1",
    ]);
    let text = std::fs::read_to_string(&report).unwrap();
    let _ = std::fs::remove_file(&replay);
    let _ = std::fs::remove_file(&report);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(text.lines().count(), 131);
}

#[test]
fn a_desync_exits_with_4_and_still_writes_the_hashes() {
    // Change the end hash of the first segment and recompute the checks: an
    // intact file that the playback cannot match.
    let replay = write_replay("desync.stqreplay", 250, |bytes| {
        let mut pos = st_sim::replay::MAGIC.len();
        loop {
            let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
            let end = pos + 8 + len + 8;
            if &bytes[pos..pos + 4] == b"SEGM" {
                bytes[end - 16] ^= 1;
                break;
            }
            pos = end;
        }
        reseal(bytes);
    });
    let report = temp("desync-report.txt");
    let out = run(&[
        "--out",
        report.to_str().unwrap(),
        "--replay",
        replay.to_str().unwrap(),
        "--checkpoint-every",
        "40",
    ]);
    let written = std::fs::read_to_string(&report).unwrap();
    let _ = std::fs::remove_file(&replay);
    let _ = std::fs::remove_file(&report);
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("desync at the checkpoint of tick 120"),
        "{stderr}"
    );
    // The ticks of the segment that failed are in the report.
    assert!(written.contains(" 00000040 "), "{written}");
    assert!(written.contains(" 00000120 "), "{written}");
}

#[test]
fn a_damaged_or_foreign_file_exits_with_4_or_1() {
    // A flipped bit in a chunk: exit 4, and the message says it is damage.
    let damaged = write_replay("damaged.stqreplay", 250, |bytes| {
        let n = bytes.len();
        bytes[n / 2] ^= 1;
    });
    let report = temp("damaged-report.txt");
    let out = run(&[
        "--out",
        report.to_str().unwrap(),
        "--replay",
        damaged.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&damaged);
    let _ = std::fs::remove_file(&report);
    assert_eq!(out.status.code(), Some(4), "{out:?}");

    // A file that does not exist is an I/O error.
    let missing = temp("missing.stqreplay");
    let out = run(&[
        "--out",
        report.to_str().unwrap(),
        "--replay",
        missing.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&report);
    assert_eq!(out.status.code(), Some(1), "{out:?}");

    // `--force` is for replays only, and a replay replaces the scenarios.
    for args in [
        &["--out", "x", "--force"][..],
        &["--out", "x", "--replay", "y", "--scenario", "solo-burn"],
        &[
            "--out",
            "x",
            "--replay",
            "y",
            "--dump-tick",
            "5",
            "--dump",
            "z",
        ],
    ] {
        assert_eq!(run(args).status.code(), Some(1), "{args:?}");
    }
}

#[test]
fn another_simulation_version_is_refused_unless_forced() {
    // Rewrite the header of a recording to say simulation version 99: a text
    // edit of the HEAD chunk, then the checks are recomputed.
    let replay = write_replay("other-version.stqreplay", 250, |bytes| {
        let pos = st_sim::replay::MAGIC.len();
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let text = String::from_utf8(bytes[pos + 8..pos + 8 + len].to_vec()).unwrap();
        let edited = text.replacen(
            &format!("sim_version: {},", st_sim::SIM_VERSION),
            "sim_version: 99,",
            1,
        );
        assert_ne!(text, edited);
        let mut head = Vec::new();
        head.extend_from_slice(b"HEAD");
        head.extend_from_slice(&(edited.len() as u32).to_le_bytes());
        head.extend_from_slice(edited.as_bytes());
        head.extend_from_slice(&[0; 8]);
        bytes.splice(pos..pos + 8 + len + 8, head);
        reseal(bytes);
    });
    let report = temp("other-version-report.txt");
    let args = [
        "--out",
        report.to_str().unwrap(),
        "--replay",
        replay.to_str().unwrap(),
    ];
    let refused = run(&args);
    assert_eq!(refused.status.code(), Some(4), "{refused:?}");
    let stderr = String::from_utf8(refused.stderr).unwrap();
    assert!(stderr.contains("simulation version 99"), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "plays simulation version {} only",
            st_sim::SIM_VERSION
        )),
        "{stderr}"
    );

    let forced = run(&[args[0], args[1], args[2], args[3], "--force"]);
    let _ = std::fs::remove_file(&replay);
    let _ = std::fs::remove_file(&report);
    assert_eq!(forced.status.code(), Some(0), "{forced:?}");
}

/// Status 3 wins over status 4 when both apply (ADR 0009).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_leak_wins_over_a_failed_replay() {
    let replay = write_replay("leak-and-desync.stqreplay", 250, |bytes| {
        let n = bytes.len();
        bytes[n / 2] ^= 1;
    });
    let report = temp("leak-and-desync-report.txt");
    let out = Command::new(env!("CARGO_BIN_EXE_determinism"))
        .args([
            "--out",
            report.to_str().unwrap(),
            "--replay",
            replay.to_str().unwrap(),
        ])
        .env("DETERMINISM_LEAK_SELF_TEST", "1")
        .output()
        .expect("the harness binary starts");
    let _ = std::fs::remove_file(&replay);
    let _ = std::fs::remove_file(&report);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
}
