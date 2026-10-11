//! `phase1-demo [--data <dir>] [--exit-after-ms <n>]`
//!
//! Watches `assets/space_envelope` (or `--data <dir>`), prints the validated
//! values, figures a person can picture and a deterministic final state hash, and
//! prints them again when a file is saved. Stop it with Ctrl-C: it holds
//! nothing that needs cleaning up and changes no terminal mode.
//! `--exit-after-ms` exits with status 0 after that long, for smoke tests.

// The binary reads the clock, sleeps and prints: exempt from PD-06 (ADR 0008).
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "exempt from PD-06: ADR 0008"
)]

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::io::Write as _;
    use std::process::ExitCode;
    use std::time::Duration;

    use phase1_demo::Session;
    use st_platform::Clock;
    use st_time::Nanos;

    const USAGE: &str = "usage: phase1-demo [--data <dir>] [--exit-after-ms <n>]";
    const DEFAULT_DIR: &str = "assets/space_envelope";
    /// Sleep between polls: about 60 Hz, like a frame.
    const FRAME: Duration = Duration::from_millis(16);

    struct Options {
        dir: String,
        exit_after: Option<Nanos>,
    }

    fn parse() -> Option<Options> {
        let mut options = Options {
            dir: DEFAULT_DIR.to_owned(),
            exit_after: None,
        };
        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            let value = args.next()?;
            match flag.as_str() {
                "--data" => options.dir = value,
                "--exit-after-ms" => {
                    options.exit_after = Some(Nanos::from_millis(value.parse().ok()?));
                }
                _ => return None,
            }
        }
        Some(options)
    }

    /// Prints and flushes; false when stdout is gone (a closed pipe).
    fn emit(text: &str) -> bool {
        let mut out = std::io::stdout().lock();
        out.write_all(text.as_bytes()).is_ok() && out.flush().is_ok()
    }

    pub fn run() -> ExitCode {
        let Some(options) = parse() else {
            eprintln!("{USAGE}");
            return ExitCode::from(1);
        };
        let mut session = match Session::open(&options.dir) {
            Ok(session) => session,
            Err(text) => {
                eprintln!("{text}");
                eprintln!(
                    "phase1-demo: cannot start. Run it from the repository root, or pass --data <dir>."
                );
                return ExitCode::from(1);
            }
        };
        let clock = Clock::new();
        let intro = format!(
            "Watching {}. Edit and save a value to see it change; press Ctrl-C to stop.\n\n{}",
            options.dir,
            session.report("started")
        );
        if !emit(&intro) {
            return ExitCode::SUCCESS;
        }
        loop {
            let now = clock.now();
            if options.exit_after.is_some_and(|limit| now >= limit) {
                return ExitCode::SUCCESS;
            }
            if let Some(text) = session.poll(now)
                && !emit(&text)
            {
                return ExitCode::SUCCESS;
            }
            std::thread::sleep(FRAME);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    native::run()
}

#[cfg(target_arch = "wasm32")]
fn main() {
    eprintln!("phase1-demo: the hot-reload demo is native only (ADR 0010)");
}
