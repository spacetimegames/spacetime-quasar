//! `determinism --out <path> [--scenario <name>] [--checkpoint-every <n>]
//! [--dump-tick <t> --dump <path>]`
//!
//! Runs the scenarios and writes the report (see the library docs). With
//! `--dump-tick` and `--dump` it also writes the canonical hash input at that
//! tick as hex. On x86_64 Linux it runs the leak detector too (ADR 0008).
//!
//! Exit status: 0 success, 1 usage or I/O error, 2 a scenario failed (for
//! example a `HashError`), 3 the leak detector saw a platform libm call
//! outside its allow-list. On x86_64 Linux, setting
//! `DETERMINISM_LEAK_SELF_TEST` makes the run call the platform `cos` once so
//! that the exit-3 path can be tested.

use std::ffi::OsString;
use std::process::ExitCode;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod leak;

const USAGE: &str = "usage: determinism --out <path> [--scenario <name>] [--checkpoint-every <n>] [--dump-tick <t> --dump <path>]";

fn main() -> ExitCode {
    let mut out: Option<OsString> = None;
    let mut scenario = None;
    let mut every = st_sim::CHECKPOINT_INTERVAL;
    let mut dump_tick: Option<u64> = None;
    let mut dump_path: Option<OsString> = None;
    // `args_os`: a non-UTF-8 argument is a usage error, not a panic.
    let mut args = std::env::args_os().skip(1);
    while let Some(flag) = args.next() {
        let Some(value) = args.next() else {
            return usage();
        };
        match flag.to_str() {
            // The output paths may be any OS string.
            Some("--out") => out = Some(value),
            Some("--dump") => dump_path = Some(value),
            Some("--scenario") => match value.into_string() {
                Ok(v) => scenario = Some(v),
                Err(_) => return usage(),
            },
            Some("--checkpoint-every") => {
                match value.to_str().and_then(|v| v.parse::<u64>().ok()) {
                    Some(n) if n > 0 => every = n,
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

    let report = match determinism::run(scenario.as_deref(), every) {
        Ok(text) => text,
        Err(e) => return fail(&e),
    };
    if let Err(code) = write(&out, &report) {
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
    leak_check()
}

/// Prints the leak detector's counts and maps a leak to exit status 3.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn leak_check() -> ExitCode {
    leak::self_test();
    let report = leak::report();
    print!("{}", report.text);
    if report.leaked {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}

/// The leak detector exists on x86_64 Linux only (ADR 0008).
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn leak_check() -> ExitCode {
    ExitCode::SUCCESS
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
        determinism::Error::Run { .. } => ExitCode::from(2),
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(1)
}
