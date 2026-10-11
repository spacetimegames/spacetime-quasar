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
//! The binary adds the leak detector (module `leak`, x86_64 Linux only) and
//! [`dump`], the canonical hash input at one tick.
//!
//! Non-goals (not yet built): replay files as scenarios.
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
    /// `--dump-tick` is past the end of every selected scenario.
    TickPastEnd(u64),
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
            Error::TickPastEnd(t) => write!(f, "no selected scenario reaches tick {t}"),
            Error::Run { scenario, error } => write!(f, "scenario `{scenario}`: {error}"),
        }
    }
}

impl core::error::Error for Error {}

/// Returns the canonical hash input at `tick` for one scenario (`only`) or
/// all of them, as hex, one line per section header and entity:
/// `<scenario> <tick, 8 digits> <section> <id or -> <hex>`, LF-terminated.
/// Scenarios shorter than `tick` are skipped.
///
/// Two platforms whose checkpoint hashes differ can diff these files to find
/// the entity and the bytes that differ.
///
/// # Errors
///
/// [`Error::UnknownScenario`], [`Error::TickPastEnd`] if no selected scenario
/// reaches `tick`, or [`Error::Run`].
///
/// ```
/// let text = determinism::dump(Some("solo-burn"), 120)?;
/// assert!(text.starts_with("solo-burn 00000120 STQH - "));
/// # Ok::<(), determinism::Error>(())
/// ```
pub fn dump(only: Option<&str>, tick: u64) -> Result<String, Error> {
    let mut text = String::new();
    let mut reached = false;
    for scenario in &selected(only)? {
        let dumped = scenario.dump_at(tick).map_err(|error| Error::Run {
            scenario: scenario.name,
            error,
        })?;
        let Some(lines) = dumped else {
            continue;
        };
        reached = true;
        for line in lines {
            text.push_str(&format!("{} {tick:08} {line}\n", scenario.name));
        }
    }
    if reached {
        Ok(text)
    } else {
        Err(Error::TickPastEnd(tick))
    }
}

fn selected(only: Option<&str>) -> Result<Vec<scenarios::Scenario>, Error> {
    Ok(match only {
        Some(name) => {
            vec![scenarios::by_name(name).ok_or_else(|| Error::UnknownScenario(name.to_owned()))?]
        }
        None => scenarios::all(),
    })
}

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
    let mut lines = Vec::new();
    for scenario in &selected(only)? {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_skips_scenarios_that_are_too_short() {
        // `solo-burn` has 600 ticks, `orbit-coast` 1,200, `swarm-controls` 2,400.
        let text = dump(None, 1_500).unwrap();
        assert!(
            text.lines()
                .all(|l| l.starts_with("swarm-controls 00001500 "))
        );
    }

    #[test]
    fn dump_past_every_scenario_is_an_error() {
        assert!(matches!(
            dump(None, 100_000),
            Err(Error::TickPastEnd(100_000))
        ));
        assert!(matches!(
            dump(Some("nope"), 0),
            Err(Error::UnknownScenario(_))
        ));
    }
}
