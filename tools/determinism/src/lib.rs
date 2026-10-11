//! Determinism harness: runs the scenarios and prints their state hashes.
//!
//! Purpose: produce, on any platform, the text that every platform must agree
//! on byte for byte (ADR 0003, ADR 0008). One line per checkpoint,
//! `<scenario> <tick> <hash>`, with the tick zero-padded to 8 digits and the
//! hash as 16 lower-case hex digits. Lines are sorted and LF-terminated.
//!
//! The scenarios are of three kinds: the simulation scenarios of
//! [`st_sim::scenarios`] (`solo-burn`, `orbit-coast`, `swarm-controls`), the
//! `st-math` scenario `math-kernels` ([`st_sim::scenarios::math_kernels`])
//! and `data-fixtures`, the `st-data` golden fixtures as one data set
//! ([`fixtures`]).
//!
//! Boundary: a sim crate under ADR 0008. It contains no simulation logic; the
//! scenarios live in [`st_sim::scenarios`], except `data-fixtures`, which
//! needs `st-data`.
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

pub mod fixtures;

use st_sim::DumpLine;
use st_sim::scenarios::{self, Checkpoint, MathKernels, RunError, Scenario};

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
    /// The `data-fixtures` scenario failed.
    Fixtures(fixtures::FixtureError),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::UnknownScenario(name) => write!(f, "unknown scenario `{name}`"),
            Error::TickPastEnd(t) => write!(f, "no selected scenario reaches tick {t}"),
            Error::Run { scenario, error } => write!(f, "scenario `{scenario}`: {error}"),
            Error::Fixtures(e) => write!(f, "scenario `{}`: {e}", fixtures::NAME),
        }
    }
}

impl core::error::Error for Error {}

/// One scenario of any kind.
enum Entry {
    Sim(Scenario),
    Kernels(MathKernels),
    Fixtures,
}

impl Entry {
    fn name(&self) -> &'static str {
        match self {
            Entry::Sim(s) => s.name,
            Entry::Kernels(k) => k.name,
            Entry::Fixtures => fixtures::NAME,
        }
    }

    fn run_with_interval(&self, every: u64) -> Result<Vec<Checkpoint>, Error> {
        let run = |error| Error::Run {
            scenario: self.name(),
            error,
        };
        match self {
            Entry::Sim(s) => s.run_with_interval(every).map_err(run),
            Entry::Kernels(k) => k.run_with_interval(every).map_err(run),
            Entry::Fixtures => {
                let (hash, _) = fixtures::checkpoint().map_err(Error::Fixtures)?;
                Ok(vec![Checkpoint { tick: 0, hash }])
            }
        }
    }

    fn dump_at(&self, tick: u64) -> Result<Option<Vec<DumpLine>>, Error> {
        let run = |error| Error::Run {
            scenario: self.name(),
            error,
        };
        match self {
            Entry::Sim(s) => s.dump_at(tick).map_err(run),
            Entry::Kernels(k) => k.dump_at(tick).map_err(run),
            Entry::Fixtures if tick == 0 => {
                let (_, line) = fixtures::checkpoint().map_err(Error::Fixtures)?;
                Ok(Some(vec![line]))
            }
            Entry::Fixtures => Ok(None),
        }
    }
}

/// Every scenario, in name order.
fn all() -> Vec<Entry> {
    let mut entries: Vec<Entry> = scenarios::all().into_iter().map(Entry::Sim).collect();
    entries.push(Entry::Kernels(scenarios::math_kernels()));
    entries.push(Entry::Fixtures);
    entries.sort_by_key(Entry::name);
    entries
}

/// The names of all scenarios, in name order.
///
/// ```
/// assert!(determinism::scenario_names().contains(&"math-kernels"));
/// ```
pub fn scenario_names() -> Vec<&'static str> {
    all().iter().map(Entry::name).collect()
}

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
/// reaches `tick`, or [`Error::Run`] or [`Error::Fixtures`].
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
        let Some(lines) = scenario.dump_at(tick)? else {
            continue;
        };
        reached = true;
        for line in lines {
            text.push_str(&format!("{} {tick:08} {line}\n", scenario.name()));
        }
    }
    if reached {
        Ok(text)
    } else {
        Err(Error::TickPastEnd(tick))
    }
}

fn selected(only: Option<&str>) -> Result<Vec<Entry>, Error> {
    let mut entries = all();
    if let Some(name) = only {
        entries.retain(|e| e.name() == name);
        if entries.is_empty() {
            return Err(Error::UnknownScenario(name.to_owned()));
        }
    }
    Ok(entries)
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
        for c in scenario.run_with_interval(checkpoint_every)? {
            lines.push(format!(
                "{} {:08} {:016x}\n",
                scenario.name(),
                c.tick,
                c.hash
            ));
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
        // `solo-burn` has 600 ticks, `orbit-coast` and `math-kernels` 1,200,
        // `swarm-controls` 2,400 and `data-fixtures` only tick 0.
        let text = dump(None, 1_500).unwrap();
        assert!(
            text.lines()
                .all(|l| l.starts_with("swarm-controls 00001500 "))
        );
    }

    #[test]
    fn dump_at_tick_0_has_the_fixtures_and_the_kernel_edge_table() {
        let text = dump(None, 0).unwrap();
        assert!(
            text.lines()
                .any(|l| l.starts_with("data-fixtures 00000000 DSET - "))
        );
        assert!(
            text.lines()
                .any(|l| l.starts_with("math-kernels 00000000 EDGE - "))
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

    #[test]
    fn scenario_names_are_sorted_and_unique() {
        let names = scenario_names();
        assert_eq!(
            names,
            [
                "data-fixtures",
                "math-kernels",
                "orbit-coast",
                "solo-burn",
                "swarm-controls"
            ]
        );
    }
}
