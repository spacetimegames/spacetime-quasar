//! `determinism --out <path> [--scenario <name>] [--checkpoint-every <n>]
//! [--dump-tick <t> --dump <path>]`, or
//! `determinism --out <path> --replay <file> [--force] [--checkpoint-every <n>]`
//!
//! Runs the scenarios and writes the report (see the library docs). Every
//! scenario with commands is also recorded to a replay, read back and played
//! (ADR 0009). With `--dump-tick` and `--dump` it also writes the canonical
//! hash input at that tick as hex. On x86_64 Linux it runs the leak detector
//! too (ADR 0008).
//!
//! `--replay <file>` plays a replay file instead of the scenarios, as a
//! scenario named after the file stem, and writes its hash every
//! `--checkpoint-every` ticks (default 120; 1 gives one per tick, to find the
//! first divergent tick between two platforms). Its lines are never part of
//! `reference.txt`. `--force` opens it without comparing the simulation
//! version, the hash format and the data set, so that the playback reports
//! the first divergence of an old replay against this build.
//!
//! Exit status: 0 success, 1 usage or I/O error, 2 a scenario failed (for
//! example a `HashError`), 3 the leak detector saw a platform libm call
//! outside its allow-list, 4 a replay did not verify (it is invalid, the
//! simulation refused it, or a checkpoint hash differs). Status 4 is
//! reported after the leak check, so 3 wins when both apply. On x86_64 Linux,
//! setting `DETERMINISM_LEAK_SELF_TEST` makes the run call the platform `cos`
//! once so that the exit-3 path can be tested.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod leak;

const USAGE: &str = "usage: determinism --out <path> [--scenario <name>] [--checkpoint-every <n>] [--dump-tick <t> --dump <path>]\n       determinism --out <path> --replay <file> [--force] [--checkpoint-every <n>]";

fn main() -> ExitCode {
    let mut out: Option<OsString> = None;
    let mut scenario = None;
    let mut every = None;
    let mut dump_tick: Option<u64> = None;
    let mut dump_path: Option<OsString> = None;
    let mut replay: Option<OsString> = None;
    let mut force = false;
    // `args_os`: a non-UTF-8 argument is a usage error, not a panic.
    let mut args = std::env::args_os().skip(1);
    while let Some(flag) = args.next() {
        if flag.to_str() == Some("--force") {
            force = true;
            continue;
        }
        let Some(value) = args.next() else {
            return usage();
        };
        match flag.to_str() {
            // The paths may be any OS string.
            Some("--out") => out = Some(value),
            Some("--dump") => dump_path = Some(value),
            Some("--replay") => replay = Some(value),
            Some("--scenario") => match value.into_string() {
                Ok(v) => scenario = Some(v),
                Err(_) => return usage(),
            },
            Some("--checkpoint-every") => {
                match value.to_str().and_then(|v| v.parse::<u64>().ok()) {
                    Some(n) if n > 0 => every = Some(n),
                    _ => return usage(),
                }
            }
            Some("--dump-tick") => match value.to_str().and_then(|v| v.parse::<u64>().ok()) {
                Some(t) => dump_tick = Some(t),
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    let Some(out) = out else {
        return usage();
    };
    // `--dump-tick` and `--dump` go together.
    let dump = match (dump_tick, dump_path) {
        (Some(t), Some(p)) => Some((t, p)),
        (None, None) => None,
        _ => return usage(),
    };

    if let Some(path) = replay {
        // A replay is played instead of the scenarios.
        if scenario.is_some() || dump.is_some() {
            return usage();
        }
        return play_replay(
            &path,
            &out,
            every.unwrap_or(st_sim::CHECKPOINT_INTERVAL),
            force,
        );
    }
    if force {
        return usage();
    }

    let every = every.unwrap_or(st_sim::CHECKPOINT_INTERVAL);
    let report = match determinism::run_report(scenario.as_deref(), every) {
        Ok(report) => report,
        Err(e) => return fail(&e),
    };
    if let Err(code) = write(&out, &report.text) {
        return code;
    }
    if let Some((tick, path)) = dump {
        match determinism::dump(scenario.as_deref(), tick) {
            Ok(text) => {
                if let Err(code) = write(&path, &text) {
                    return code;
                }
            }
            Err(e) => return fail(&e),
        }
    }
    finish(&report.replay_failures)
}

/// Plays a replay file as a scenario and writes its hashes.
fn play_replay(path: &OsString, out: &OsString, every: u64, force: bool) -> ExitCode {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("determinism: cannot read `{}`: {e}", path.to_string_lossy());
            return ExitCode::from(1);
        }
    };
    // The scenario is named after the file stem.
    let name = Path::new(path)
        .file_stem()
        .map_or_else(|| "replay".to_owned(), |s| s.to_string_lossy().into_owned());
    let run = determinism::replays::play_file(&name, &bytes, every, force);
    // The hashes go out even after a desync: the first differing line of two
    // platforms' files is the first divergent tick.
    if let Err(code) = write(out, &run.text) {
        return code;
    }
    match run.result {
        Ok(verified) => {
            if !verified.complete {
                eprintln!(
                    "determinism: `{name}` is cut short; it verified up to tick {}",
                    verified.final_tick
                );
            }
            finish(&[])
        }
        Err(e) => finish(&[e]),
    }
}

/// The leak check, then the verdict: 3 for a leak, 4 for replays that did not
/// verify, else 0.
fn finish(replay_failures: &[determinism::Error]) -> ExitCode {
    if leak_check() {
        return ExitCode::from(3);
    }
    for e in replay_failures {
        eprintln!("determinism: {e}");
    }
    if replay_failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(4)
    }
}

/// Prints the leak detector's counts; true if a platform libm call outside the
/// allow-list was seen.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn leak_check() -> bool {
    leak::self_test();
    let report = leak::report();
    print!("{}", report.text);
    report.leaked
}

/// The leak detector exists on x86_64 Linux only (ADR 0008).
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn leak_check() -> bool {
    false
}

fn write(path: &OsString, text: &str) -> Result<(), ExitCode> {
    std::fs::write(path, text).map_err(|e| {
        eprintln!(
            "determinism: cannot write `{}`: {e}",
            path.to_string_lossy()
        );
        ExitCode::from(1)
    })
}

fn fail(e: &determinism::Error) -> ExitCode {
    eprintln!("determinism: {e}");
    match e {
        determinism::Error::UnknownScenario(_) | determinism::Error::TickPastEnd(_) => {
            ExitCode::from(1)
        }
        determinism::Error::Run { .. }
        | determinism::Error::Fixtures(_)
        | determinism::Error::Record { .. }
        | determinism::Error::LiveMismatch(_) => ExitCode::from(2),
        determinism::Error::Replay { .. } => ExitCode::from(4),
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(1)
}
