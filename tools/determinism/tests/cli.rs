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
