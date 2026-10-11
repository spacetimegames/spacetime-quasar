//! Determinism harness: runs the `st-sim` scenarios and prints their state hashes.
//!
//! Purpose: produce, on any platform, the text that every platform must agree
//! on byte for byte (ADR 0003, ADR 0008). One line per checkpoint,
//! `<scenario> <tick> <hash>`, with the tick zero-padded to 8 digits and the
//! hash as 16 lower-case hex digits. Lines are sorted and LF-terminated.
//!
//! Boundary: a sim crate under ADR 0008. It contains no simulation logic; the
//! scenarios live in [`st_sim::scenarios`].
//!
//! Non-goals (not yet built): the leak detector, tick dumps, and replay files
//! as scenarios.
//!
//! # Example
//!
//! ```
//! let text = determinism::run(Some("solo-burn"), 120)?;
//! assert!(text.starts_with("solo-burn 00000000 "));
//! # Ok::<(), determinism::Error>(())
//! ```

use st_sim::scenarios::{self, RunError};

/// The harness failed.
#[derive(Debug)]
pub enum Error {
    /// `--scenario` named a scenario that does not exist.
    UnknownScenario(String),
    /// A scenario failed to run.
    Run {
        /// The scenario.
        scenario: &'static str,
        /// What went wrong.
        error: RunError,
    },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::UnknownScenario(name) => write!(f, "unknown scenario `{name}`"),
            Error::Run { scenario, error } => write!(f, "scenario `{scenario}`: {error}"),
        }
    }
}

impl core::error::Error for Error {}

/// Runs one scenario (`only`) or all of them and returns the sorted report.
///
/// `checkpoint_every` only changes how often a hash is taken, which helps to
/// localise a divergence; the reference file is for 120 (the default of the
/// binary). Tick 0 and the final tick are always included.
///
/// # Errors
///
/// [`Error`] for an unknown scenario name or a scenario that fails.
pub fn run(only: Option<&str>, checkpoint_every: u64) -> Result<String, Error> {
    let selected = match only {
        Some(name) => {
            vec![scenarios::by_name(name).ok_or_else(|| Error::UnknownScenario(name.to_owned()))?]
        }
        None => scenarios::all(),
    };
    let mut lines = Vec::new();
    for scenario in &selected {
        let checkpoints = scenario
            .run_with_interval(checkpoint_every)
            .map_err(|error| Error::Run {
                scenario: scenario.name,
                error,
            })?;
        for c in checkpoints {
            lines.push(format!("{} {:08} {:016x}\n", scenario.name, c.tick, c.hash));
        }
    }
    lines.sort_unstable();
    Ok(lines.concat())
}
