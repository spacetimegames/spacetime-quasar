//! Replay files as scenarios (ADR 0009, "Harness and CI").
//!
//! The harness records each scenario that has commands through
//! [`ReplayWriter`] with a constant header, prints the FNV-1a 64 hash of the
//! bytes, reads them back through [`ReplayReader`] and plays them in a fresh
//! simulation. A replay file given on the command line is played the same way.

use st_data::{DataSetHash, data_set_hash};
use st_sim::replay::{PlayContext, ReplayHeader, ReplayReader, ReplayWriter, Verified};
use st_sim::scenarios::{RunError, Scenario};
use st_sim::{CHECKPOINT_INTERVAL, Sim, StateHasher};

use crate::Error;

/// The `build` string of the constant harness header. It is never compared,
/// but it is in the bytes, so it must not change with the package version.
pub const HEADER_BUILD: &str = "determinism";

/// A scenario recorded to replay bytes.
#[derive(Clone, Debug)]
pub struct Recording {
    /// The whole file.
    pub bytes: Vec<u8>,
    /// The tick the recording ends at.
    pub final_tick: u64,
    /// The state hash there.
    pub final_hash: u64,
}

/// The ADR 0010 hash of the empty data set: what the constant harness header
/// names, because the scenarios are defined in code and load no data.
pub fn empty_data_set() -> DataSetHash {
    // An empty list has no duplicate source names, so this cannot fail; the
    // fallback keeps the function total without a panic.
    data_set_hash(&[]).unwrap_or_else(|_| DataSetHash::from_bytes([0; 32]))
}

/// The context the harness opens its own replays with.
pub fn context() -> PlayContext {
    PlayContext::current(empty_data_set())
}

/// FNV-1a 64 of some bytes (the ADR 0008 implementation).
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = StateHasher::new();
    h.bytes(bytes);
    h.finish()
}

/// Runs a scenario and records every command into a replay, with a
/// checkpoint every 120 ticks, and the final tick at the end.
///
/// # Errors
///
/// [`Error::Run`] if the scenario itself fails and [`Error::Record`] if the
/// writer refuses what the scenario did.
pub fn record(s: &Scenario) -> Result<Recording, Error> {
    let run = |error| Error::Run {
        scenario: s.name,
        error,
    };
    let rec = |error| Error::Record {
        scenario: s.name,
        error,
    };
    let mut sim = Sim::new(&s.setup, s.seed).map_err(|e| run(RunError::Setup(e)))?;
    let header = ReplayHeader::new(&empty_data_set(), s.seed, HEADER_BUILD, Vec::new());
    let start = sim.state_hash().map_err(|e| run(RunError::Hash(e)))?;
    let mut writer = ReplayWriter::new(&header, &s.setup, start).map_err(rec)?;
    let mut bytes = Vec::new();
    let mut next = 0;
    while sim.tick() < s.ticks {
        let tick = sim.tick();
        let first = next;
        while next < s.commands.len() && s.commands[next].tick <= tick {
            next += 1;
        }
        let now = &s.commands[first..next];
        sim.step(now)
            .map_err(|error| run(RunError::Step { tick, error }))?;
        writer.push_tick(tick, now).map_err(rec)?;
        let at = sim.tick();
        // The final tick closes its segment in `finish`.
        if at % CHECKPOINT_INTERVAL == 0 && at != s.ticks {
            let hash = sim.state_hash().map_err(|e| run(RunError::Hash(e)))?;
            writer.checkpoint(at, hash).map_err(rec)?;
            bytes.extend(writer.take_bytes());
        }
    }
    let final_hash = sim.state_hash().map_err(|e| run(RunError::Hash(e)))?;
    bytes.extend(writer.finish(sim.tick(), final_hash).map_err(rec)?);
    Ok(Recording {
        bytes,
        final_tick: sim.tick(),
        final_hash,
    })
}

/// Opens and plays replay bytes. A failure of any kind is an
/// [`Error::Replay`].
///
/// # Errors
///
/// [`Error::Replay`] for an invalid file, a refused tick or a desync.
pub fn verify(name: &str, bytes: &[u8], ctx: &PlayContext) -> Result<Verified, Error> {
    let replay = |error| Error::Replay {
        scenario: name.to_owned(),
        error,
    };
    ReplayReader::open(bytes, ctx)
        .and_then(|reader| reader.verify())
        .map_err(replay)
}

/// A replay file played as a scenario.
#[derive(Debug)]
pub struct ReplayRun {
    /// The report lines, `<name> <tick> <hash>`, for every `every` ticks, the
    /// start and the final tick. After a desync, the lines up to the end of
    /// the segment that failed.
    pub text: String,
    /// How the playback ended.
    pub result: Result<Verified, Error>,
}

/// Plays a replay file and reports its hash every `every` ticks.
///
/// With `force` the version checks are skipped (the header and the container
/// must still read), so the playback reports the first divergence of an old
/// replay against this build.
pub fn play_file(name: &str, bytes: &[u8], every: u64, force: bool) -> ReplayRun {
    let every = every.max(1);
    let mut ctx = context();
    ctx.check_versions = !force;
    let replay = |error| Error::Replay {
        scenario: name.to_owned(),
        error,
    };
    let reader = match ReplayReader::open(bytes, &ctx) {
        Ok(reader) => reader,
        Err(e) => {
            return ReplayRun {
                text: String::new(),
                result: Err(replay(e)),
            };
        }
    };
    let mut lines: Vec<(u64, u64)> = Vec::new();
    let mut hash_error = None;
    let result = reader.play(|sim| {
        if sim.tick() % every == 0 || lines.is_empty() {
            match sim.state_hash() {
                Ok(h) => lines.push((sim.tick(), h)),
                Err(e) => hash_error = hash_error.or(Some(e)),
            }
        }
    });
    let result = match (result, hash_error) {
        (Err(e), _) => Err(replay(e)),
        (Ok(_), Some(e)) => Err(replay(st_sim::replay::ReplayError::Hash(e))),
        (Ok(v), None) => {
            if lines.last().is_none_or(|&(t, _)| t != v.final_tick) {
                lines.push((v.final_tick, v.final_hash));
            }
            Ok(v)
        }
    };
    let mut text: Vec<String> = lines
        .iter()
        .map(|(tick, hash)| format!("{name} {tick:08} {hash:016x}\n"))
        .collect();
    text.sort_unstable();
    ReplayRun {
        text: text.concat(),
        result,
    }
}
