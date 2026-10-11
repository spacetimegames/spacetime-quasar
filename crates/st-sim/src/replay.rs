//! Replay files: a command log on disk (ADR 0009, "Replay file format").
//!
//! Purpose: record the commands a simulation accepted and play them back,
//! checking the state hash at every checkpoint. A replay is the seed, the
//! setup and every accepted command in tick order, so playing it in a build
//! with the same [`SIM_VERSION`](crate::SIM_VERSION) reproduces the session bit for bit.
//!
//! Boundary: pure code over byte slices and `Vec<u8>`. There is no file
//! system access here and it compiles for every target; the caller (the
//! game through `st-platform`, a tool through `std::fs`) stores and loads
//! the bytes. A replay is version-locked (PD-33): a build refuses a replay
//! recorded by another [`SIM_VERSION`](crate::SIM_VERSION), with an error that names the
//! recording build.
//!
//! # Layout
//!
//! A file is an 8-byte [`MAGIC`] followed by chunks. A chunk is a 4-byte tag,
//! a `u32` little-endian length, the payload and a `u64` little-endian
//! FNV-1a 64 check over the tag, length and payload. The chunks are `HEAD`
//! (an ADR 0010 envelope of kind `replay`, RON text, [`ReplayHeader`]), `SETP`
//! (the seed, the start hash and the encoded [`Setup`](crate::Setup)), any
//! number of `SEGM` (at most 120 ticks of commands and the state hash at
//! their end) and `END_`. The magic and the `HEAD` chunk keep their layout in
//! every future container version, so [`read_header`] works on any file.
//! A segment ends where the tick is a multiple of 120 and at the final tick,
//! so its hash is exactly an ADR 0008 checkpoint.
//!
//! The encoding is canonical: one command log has one byte encoding. The
//! header carries no clock, so recording the same session twice gives the same
//! bytes.
//!
//! # Non-goals
//!
//! The viewer, seeking, automatic recording and sharing (SPAC-121), snapshots
//! in the file, compression, and data-set changes during a recording.
//!
//! # Example
//!
//! ```
//! use st_sim::replay::{PlayContext, ReplayHeader, ReplayReader, ReplayWriter};
//! use st_sim::{CHECKPOINT_INTERVAL, Sim, command, scenarios};
//!
//! // Record: step the scenario, hand every accepted command to the writer.
//! let scenario = scenarios::solo_burn();
//! let data_set = st_data::data_set_hash(&[])?;
//! let header = ReplayHeader::new(&data_set, scenario.seed, "example", Vec::new());
//! let mut sim = Sim::new(&scenario.setup, scenario.seed)?;
//! let mut writer = ReplayWriter::new(&header, &scenario.setup, sim.state_hash()?)?;
//! let mut file = Vec::new();
//! while sim.tick() < 250 {
//!     let tick = sim.tick();
//!     let mut now: Vec<_> = scenario.commands.iter().filter(|c| c.tick == tick).cloned().collect();
//!     command::sort(&mut now);
//!     sim.step(&now)?;
//!     writer.push_tick(tick, &now)?;
//!     if sim.tick() % CHECKPOINT_INTERVAL == 0 {
//!         writer.checkpoint(sim.tick(), sim.state_hash()?)?;
//!         file.extend(writer.take_bytes());
//!     }
//! }
//! file.extend(writer.finish(sim.tick(), sim.state_hash()?)?);
//!
//! // Play: open checks the versions and the data set, `verify` replays it
//! // and compares every checkpoint.
//! let reader = ReplayReader::open(&file, &PlayContext::current(data_set))?;
//! let verified = reader.verify()?;
//! assert_eq!((verified.final_tick, verified.complete), (250, true));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod reader;
mod writer;

use serde::{Deserialize, Serialize};
use st_data::{DataSetHash, Diagnostic, Genesis, MigrationError, Schema, Validate, Validator};

use crate::command::StepError;
use crate::command::wire::WireError;
use crate::hash::{HASH_FORMAT_VERSION, HashError, StateHasher};
use crate::sim::SetupError;

pub use reader::{ReplayReader, Segment, TickRecord, Verified, read_header};
pub use writer::ReplayWriter;

/// The first 8 bytes of every replay file: `\x89STQR\r\n\x1a`. The first byte
/// is not ASCII and the CR LF pair catches text-mode transfers, as in PNG.
pub const MAGIC: [u8; 8] = *b"\x89STQR\r\n\x1a";

/// The default limit on the size of a file, for the writer and the reader
/// (256 MiB).
pub const DEFAULT_MAX_BYTES: usize = 256 << 20;

/// The largest `HEAD` payload, in bytes (64 KiB).
pub const MAX_HEADER_BYTES: usize = 64 << 10;

pub(crate) const TAG_HEAD: [u8; 4] = *b"HEAD";
pub(crate) const TAG_SETP: [u8; 4] = *b"SETP";
pub(crate) const TAG_SEGM: [u8; 4] = *b"SEGM";
pub(crate) const TAG_END: [u8; 4] = *b"END_";

/// Bytes of framing around a chunk payload: tag, length and check.
pub(crate) const CHUNK_OVERHEAD: usize = 4 + 4 + 8;

/// The `HEAD` chunk: an ADR 0010 envelope of kind `replay`, version 1.
///
/// `sim_version`, `state_hash_version` and `data_set_hash` decide whether the
/// body can be played here. `seed` and `start_tick` are copies for display
/// and tools: `SETP` and `Sim::new` are authoritative, and the reader reports
/// a mismatch as corruption. `build` and `local_pilots` are informational: the
/// first is never compared, the second names the pilots a viewer follows by
/// default, and the simulation never sees it.
///
/// ```
/// use st_sim::replay::ReplayHeader;
///
/// let data_set = st_data::data_set_hash(&[]).unwrap();
/// let header = ReplayHeader::new(&data_set, 42, "spacetime-quasar 0.1.0", vec![1]);
/// assert_eq!(header.sim_version, st_sim::SIM_VERSION);
/// assert_eq!(header.start_tick, 0);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayHeader {
    /// The [`SIM_VERSION`](crate::SIM_VERSION)(crate::SIM_VERSION) of the recording build.
    pub sim_version: u32,
    /// The state hash format version of the recording build.
    pub state_hash_version: u32,
    /// The ADR 0010 data-set hash of the shipped data (`b3:` and 64 hex digits).
    pub data_set_hash: String,
    /// The world seed (a copy of `SETP`'s).
    pub seed: u64,
    /// The tick the recording starts at (0 until saves exist).
    pub start_tick: u64,
    /// The recording build, such as `spacetime-quasar 0.1.0`. Informational.
    pub build: String,
    /// The pilots a viewer follows by default. Informational.
    pub local_pilots: Vec<u64>,
}

impl ReplayHeader {
    /// A header for a recording made by this build, starting at tick 0.
    pub fn new(
        data_set_hash: &DataSetHash,
        seed: u64,
        build: &str,
        local_pilots: Vec<u64>,
    ) -> Self {
        Self {
            sim_version: crate::SIM_VERSION,
            state_hash_version: HASH_FORMAT_VERSION,
            data_set_hash: data_set_hash.to_string(),
            seed,
            start_tick: 0,
            build: build.to_owned(),
            local_pilots,
        }
    }
}

impl Schema for ReplayHeader {
    const KIND: &'static str = "replay";
    const VERSION: u32 = 1;
    type Previous = Genesis;

    fn migrate(previous: Genesis) -> Result<Self, MigrationError> {
        match previous {}
    }
}

impl Validate for ReplayHeader {
    fn validate(&self, v: &mut Validator) {
        if self.data_set_hash.parse::<DataSetHash>().is_err() {
            v.field("data_set_hash", |v| {
                v.error(
                    "replay.data_set_hash",
                    "must be `b3:` and 64 lower-case hex digits",
                );
            });
        }
        if self.build.len() > 256 {
            v.field("build", |v| {
                v.error("replay.build", "must be at most 256 bytes");
            });
        }
        if self.local_pilots.len() > 1024 {
            v.field("local_pilots", |v| {
                v.error("replay.local_pilots", "must name at most 1024 pilots");
            });
        }
    }
}

/// What the running build offers to a replay, and the limits for reading it.
///
/// ```
/// use st_sim::replay::PlayContext;
///
/// let data_set = st_data::data_set_hash(&[]).unwrap();
/// let ctx = PlayContext::current(data_set);
/// assert_eq!(ctx.sim_version, st_sim::SIM_VERSION);
/// assert!(ctx.check_versions);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayContext {
    /// The running build's [`SIM_VERSION`](crate::SIM_VERSION)(crate::SIM_VERSION).
    pub sim_version: u32,
    /// The running build's state hash format version.
    pub state_hash_version: u32,
    /// The hash of the data set that is loaded.
    pub data_set_hash: DataSetHash,
    /// Files larger than this are refused ([`DEFAULT_MAX_BYTES`]).
    pub max_bytes: usize,
    /// Whether `open` compares `sim_version`, `state_hash_version` and
    /// `data_set_hash`. Turn it off only to measure how a change affects an old
    /// replay (the harness `--force`). The container and the header must
    /// still be readable.
    pub check_versions: bool,
}

impl PlayContext {
    /// The context of this build with the given loaded data set: the current
    /// versions, the default size limit, version checks on.
    pub fn current(data_set_hash: DataSetHash) -> Self {
        Self {
            sim_version: crate::SIM_VERSION,
            state_hash_version: HASH_FORMAT_VERSION,
            data_set_hash,
            max_bytes: DEFAULT_MAX_BYTES,
            check_versions: true,
        }
    }
}

/// A replay could not be written, read or played. None of these is a panic;
/// all of them are for the person or the tool to read.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayError {
    /// The bytes do not start with the replay magic.
    NotAReplay,
    /// The file is larger than the size limit.
    TooLarge {
        /// The size the file has or would have.
        len: u64,
        /// The limit.
        limit: u64,
    },
    /// The file ends inside a chunk, or before its header and setup are
    /// complete. After the setup this is not an error of `open`: the reader
    /// plays the complete segments (see [`ReplayReader::truncated_at`]) and
    /// [`ReplayReader::segment`] reports this past the last one.
    Truncated {
        /// The last tick of the last complete segment, if there is one.
        last_complete_tick: Option<u64>,
    },
    /// A chunk fails its check, or two copies of a value disagree. The file
    /// was damaged; it is not a desync.
    Corrupt {
        /// Byte offset of the chunk or value.
        offset: usize,
        /// What failed.
        what: &'static str,
    },
    /// A chunk with a good check holds something the format does not allow.
    Malformed {
        /// Byte offset in the file, near where decoding stopped.
        offset: usize,
        /// What was wrong.
        error: WireError,
    },
    /// The header is not a valid ADR 0010 `replay` envelope.
    Header(Vec<Diagnostic>),
    /// The header is a newer envelope version than this build reads.
    NewerBuild {
        /// The header version in the file.
        header_version: u32,
        /// The newest header version this build reads.
        supported: u32,
    },
    /// The replay was recorded by another simulation version (PD-33).
    SimVersionMismatch {
        /// The recorded `sim_version`.
        recorded: u32,
        /// The `build` string of the recording, for the message.
        recorded_build: String,
        /// This build's version.
        current: u32,
    },
    /// The replay was recorded with another state hash format.
    HashFormatMismatch {
        /// The recorded version.
        recorded: u32,
        /// This build's version.
        current: u32,
    },
    /// The replay ran on another data set than the one loaded.
    DataSetMismatch {
        /// The recorded data-set hash.
        recorded: String,
        /// The loaded data-set hash.
        current: String,
    },
    /// The recorded setup was refused by `Sim::new`.
    Setup(SetupError),
    /// A recorded tick was refused by `Sim::step`.
    Step {
        /// The tick being stepped.
        tick: u64,
        /// Why.
        error: StepError,
    },
    /// The state could not be hashed (a NaN).
    Hash(HashError),
    /// A checkpoint hash differs from the state hash of the playback: the
    /// simulation diverged from the recording. The first divergent tick is in
    /// `(last_good_tick, checkpoint_tick]`; with `last_good_tick` of `None`
    /// the state was already different at the start.
    Desync {
        /// The tick of the checkpoint that failed.
        checkpoint_tick: u64,
        /// The hash in the file.
        expected: u64,
        /// The hash the playback computed.
        actual: u64,
        /// The previous checkpoint that matched.
        last_good_tick: Option<u64>,
    },
    /// The writer was misused, or was given input that could not be recorded.
    BadInput(&'static str),
    /// `Segment` index past the last segment of a complete file.
    NoSuchSegment(usize),
    /// The header could not be written.
    HeaderWrite(String),
}

impl core::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReplayError::NotAReplay => f.write_str("not a replay file"),
            ReplayError::TooLarge { len, limit } => {
                write!(
                    f,
                    "the replay has {len} bytes, more than the limit of {limit}"
                )
            }
            ReplayError::Truncated {
                last_complete_tick: Some(t),
            } => write!(f, "the replay is cut short; it is complete up to tick {t}"),
            ReplayError::Truncated {
                last_complete_tick: None,
            } => f.write_str("the replay is cut short before its setup is complete"),
            ReplayError::Corrupt { offset, what } => {
                write!(f, "the replay is damaged at byte {offset}: {what}")
            }
            ReplayError::Malformed { offset, error } => {
                write!(f, "the replay is malformed near byte {offset}: {error}")
            }
            ReplayError::Header(diagnostics) => match diagnostics.first() {
                Some(d) => write!(f, "the replay header is not valid: {d}"),
                None => f.write_str("the replay header is not valid"),
            },
            ReplayError::NewerBuild {
                header_version,
                supported,
            } => write!(
                f,
                "this replay was made by a newer build (header version {header_version}; \
                 this build reads up to {supported})"
            ),
            ReplayError::SimVersionMismatch {
                recorded,
                recorded_build,
                current,
            } => write!(
                f,
                "Recorded with {recorded_build} (simulation version {recorded}). \
                 This build plays simulation version {current} only."
            ),
            ReplayError::HashFormatMismatch { recorded, current } => write!(
                f,
                "the replay uses state hash format {recorded}; this build uses {current}"
            ),
            ReplayError::DataSetMismatch { recorded, current } => write!(
                f,
                "the replay was recorded with the data set {recorded}; the loaded data set is {current}"
            ),
            ReplayError::Setup(e) => write!(f, "the recorded setup is refused: {e}"),
            ReplayError::Step { tick, error } => write!(f, "tick {tick} is refused: {error}"),
            ReplayError::Hash(e) => write!(f, "the state cannot be hashed: {e}"),
            ReplayError::Desync {
                checkpoint_tick,
                expected,
                actual,
                last_good_tick,
            } => {
                write!(
                    f,
                    "desync at the checkpoint of tick {checkpoint_tick}: the replay has \
                     {expected:016x}, the playback has {actual:016x}"
                )?;
                match last_good_tick {
                    Some(t) => write!(f, "; the first divergent tick is after tick {t}"),
                    None => f.write_str("; the state differed from the start"),
                }
            }
            ReplayError::BadInput(what) => write!(f, "cannot record: {what}"),
            ReplayError::NoSuchSegment(i) => write!(f, "the replay has no segment {i}"),
            ReplayError::HeaderWrite(e) => write!(f, "the header cannot be written: {e}"),
        }
    }
}

impl core::error::Error for ReplayError {}

/// Appends a chunk: tag, length, payload and the FNV-1a 64 check over those.
pub(crate) fn put_chunk(
    out: &mut Vec<u8>,
    tag: [u8; 4],
    payload: &[u8],
) -> Result<(), ReplayError> {
    let len = u32::try_from(payload.len()).map_err(|_| ReplayError::TooLarge {
        len: payload.len() as u64,
        limit: u64::from(u32::MAX),
    })?;
    let start = out.len();
    out.extend_from_slice(&tag);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    let check = chunk_check(&out[start..]);
    out.extend_from_slice(&check.to_le_bytes());
    Ok(())
}

/// FNV-1a 64 (the ADR 0008 implementation) over tag, length and payload.
pub(crate) fn chunk_check(bytes: &[u8]) -> u64 {
    let mut h = StateHasher::new();
    h.bytes(bytes);
    h.finish()
}
