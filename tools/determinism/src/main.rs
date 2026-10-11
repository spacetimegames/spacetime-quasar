//! `determinism --out <path> [--scenario <name>] [--checkpoint-every <n>]`
//!
//! Runs the scenarios and writes the report (see the library docs). Exit
//! status: 0 success, 1 usage or I/O error, 2 a scenario failed.

use std::process::ExitCode;

const USAGE: &str = "usage: determinism --out <path> [--scenario <name>] [--checkpoint-every <n>]";

fn main() -> ExitCode {
    let mut out = None;
    let mut scenario = None;
    let mut every = st_sim::CHECKPOINT_INTERVAL;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next();
        match (arg.as_str(), value) {
            ("--out", Some(v)) => out = Some(v),
            ("--scenario", Some(v)) => scenario = Some(v),
            ("--checkpoint-every", Some(v)) => match v.parse::<u64>() {
                Ok(n) if n > 0 => every = n,
                _ => return usage(),
            },
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
                eprintln!("determinism: cannot write `{out}`: {e}");
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
