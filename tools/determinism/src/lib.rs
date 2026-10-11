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
//! Replay files are scenarios too (ADR 0009, [`replays`]). Every scenario that
//! has commands is recorded to a replay with a constant header, read back and
//! played in a fresh simulation, and one more line,
//! `<scenario>.replay <final tick> <hash of the replay bytes>`, makes the
//! platforms compare the encoder's output. [`replays::play_file`] plays a
//! replay file (the binary's `--replay`).
//!
//! Boundary: a sim crate under ADR 0008. It contains no simulation logic; the
//! scenarios live in [`st_sim::scenarios`], except `data-fixtures`, which
//! needs `st-data`. File access is in the binary only.
//!
//! The binary adds the leak detector (module `leak`, x86_64 Linux only) and
//! [`dump`], the canonical hash input at one tick.
//!
//! Non-goals: the replay viewer and sharing of replays (SPAC-121).
//!
//! # Example
//!
//! ```
//! let text = determinism::run(Some("solo-burn"), 120)?;
//! assert!(text.starts_with("solo-burn 00000000 "));
//! assert!(text.contains("\nsolo-burn.replay 00000600 "));
//! # Ok::<(), determinism::Error>(())
//! ```

pub mod fixtures;
pub mod replays;

use st_sim::DumpLine;
use st_sim::replay::ReplayError;
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
    /// The replay writer refused what a scenario did.
    Record {
        /// The scenario.
        scenario: &'static str,
        /// What went wrong.
        error: ReplayError,
    },
    /// The recording of a scenario ends in another state than the live run.
    LiveMismatch(&'static str),
    /// A replay did not verify: it is invalid, the simulation refused it or
    /// its hashes differ (exit status 4).
    Replay {
        /// The scenario, or the stem of the replay file.
        scenario: String,
        /// What went wrong.
        error: ReplayError,
    },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::UnknownScenario(name) => write!(f, "unknown scenario `{name}`"),
            Error::TickPastEnd(t) => write!(f, "no selected scenario reaches tick {t}"),
            Error::Run { scenario, error } => write!(f, "scenario `{scenario}`: {error}"),
            Error::Fixtures(e) => write!(f, "scenario `{}`: {e}", fixtures::NAME),
            Error::Record { scenario, error } => {
                write!(
                    f,
                    "scenario `{scenario}`: cannot record the replay: {error}"
                )
            }
            Error::LiveMismatch(scenario) => write!(
                f,
                "scenario `{scenario}`: the recorded run ends in another state than the live run"
            ),
            Error::Replay { scenario, error } => write!(f, "replay `{scenario}`: {error}"),
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

/// The report of a run, and the replays that failed to verify.
#[derive(Debug)]
pub struct Report {
    /// The sorted report: one line per checkpoint and per `.replay` file.
    pub text: String,
    /// The scenarios whose recording did not play back to the same hashes.
    /// The binary maps a non-empty list to exit status 4, after the leak check.
    pub replay_failures: Vec<Error>,
}

/// Runs one scenario (`only`) or all of them, records the ones with commands
/// to replays and plays those back (ADR 0009), and returns the report.
///
/// `checkpoint_every` only changes how often a hash is taken, which helps to
/// localise a divergence; the reference file is for 120 (the default of the
/// binary). Tick 0 and the final tick are always included. The replays always
/// checkpoint every 120 ticks, as the format requires.
///
/// # Errors
///
/// [`Error`] for an unknown scenario name or a scenario that fails. A replay
/// that does not verify is not an `Err` here: it is in
/// [`Report::replay_failures`], and its lines are still in the text.
pub fn run_report(only: Option<&str>, checkpoint_every: u64) -> Result<Report, Error> {
    let ctx = replays::context();
    let mut lines = Vec::new();
    let mut replay_failures = Vec::new();
    for scenario in &selected(only)? {
        let checkpoints = scenario.run_with_interval(checkpoint_every)?;
        for c in &checkpoints {
            lines.push(format!(
                "{} {:08} {:016x}\n",
                scenario.name(),
                c.tick,
                c.hash
            ));
        }
        let Entry::Sim(s) = scenario else {
            continue;
        };
        if s.commands.is_empty() {
            continue;
        }
        let recording = replays::record(s)?;
        // The recording is a second run in the same process: it must end where
        // the live run did.
        let live = checkpoints.last().map(|c| (c.tick, c.hash));
        if live != Some((recording.final_tick, recording.final_hash)) {
            return Err(Error::LiveMismatch(s.name));
        }
        lines.push(format!(
            "{}.replay {:08} {:016x}\n",
            s.name,
            recording.final_tick,
            replays::fnv1a64(&recording.bytes)
        ));
        if let Err(e) = replays::verify(s.name, &recording.bytes, &ctx) {
            replay_failures.push(e);
        }
    }
    lines.sort_unstable();
    Ok(Report {
        text: lines.concat(),
        replay_failures,
    })
}

/// Like [`run_report`], for callers that treat a replay that does not verify
/// as a failure.
///
/// # Errors
///
/// [`Error`] for an unknown scenario name, a scenario that fails, or a replay
/// that does not verify ([`Error::Replay`]).
///
/// ```
/// let text = determinism::run(Some("orbit-coast"), 120)?;
/// assert!(text.lines().any(|l| l.starts_with("orbit-coast.replay 00001200 ")));
/// # Ok::<(), determinism::Error>(())
/// ```
pub fn run(only: Option<&str>, checkpoint_every: u64) -> Result<String, Error> {
    let report = run_report(only, checkpoint_every)?;
    match report.replay_failures.into_iter().next() {
        Some(e) => Err(e),
        None => Ok(report.text),
    }
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
