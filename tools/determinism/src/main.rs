//! `determinism --out <path> [--scenario <name>] [--checkpoint-every <n>]`
//!
//! Runs the scenarios and writes the report (see the library docs). Exit
//! status: 0 success, 1 usage or I/O error, 2 a scenario failed.

use std::ffi::OsString;
use std::process::ExitCode;

const USAGE: &str = "usage: determinism --out <path> [--scenario <name>] [--checkpoint-every <n>]";

fn main() -> ExitCode {
    let mut out: Option<OsString> = None;
    let mut scenario = None;
    let mut every = st_sim::CHECKPOINT_INTERVAL;
    // `args_os`: a non-UTF-8 argument is a usage error, not a panic.
    let mut args = std::env::args_os().skip(1);
    while let Some(flag) = args.next() {
        let Some(value) = args.next() else {
            return usage();
        };
        match flag.to_str() {
            // The output path may be any OS string.
            Some("--out") => out = Some(value),
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
            _ => return usage(),
        }
    }
    let Some(out) = out else {
        return usage();
    };
    match determinism::run(scenario.as_deref(), every) {
        Ok(text) => match std::fs::write(&out, text) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("determinism: cannot write `{}`: {e}", out.to_string_lossy());
                ExitCode::from(1)
            }
        },
        Err(determinism::Error::UnknownScenario(name)) => {
            eprintln!("determinism: unknown scenario `{name}`");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("determinism: {e}");
            ExitCode::from(2)
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(1)
}
