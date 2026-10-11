//! The streaming replay writer.

use std::collections::BTreeMap;

use st_data::write_envelope;

use super::{
    CHUNK_OVERHEAD, DEFAULT_MAX_BYTES, MAGIC, MAX_HEADER_BYTES, ReplayError, ReplayHeader, TAG_END,
    TAG_HEAD, TAG_SEGM, TAG_SETP, put_chunk,
};
use crate::command::wire::{self, MAX_PAYLOAD_BYTES, MAX_TICK_BYTES};
use crate::command::{Command, MAX_COMMANDS_PER_TICK};
use crate::sim::{CHECKPOINT_INTERVAL, Setup};

/// Size of the `END_` chunk: three `u64` fields and the framing. The writer
/// keeps room for it, so a file that was started can always be finished.
const END_CHUNK_BYTES: usize = 24 + CHUNK_OVERHEAD;

/// The segment being recorded.
#[derive(Debug)]
struct OpenSegment {
    first_tick: u64,
    /// The encoded records.
    body: Vec<u8>,
    records: u64,
    /// The tick of the last record, if any.
    last_record: Option<u64>,
    /// Per pilot, the `seq` of its previous command in this segment: the base
    /// of the next difference.
    base: BTreeMap<u64, u64>,
}

/// Writes a replay as the simulation runs (ADR 0009, "API sketch").
///
/// The writer keeps one open segment. Completed chunks leave through
/// [`take_bytes`](Self::take_bytes), which the caller appends to storage as it
/// goes, so a crash loses at most the open segment (about a second). The
/// per-tick loop is: step, [`push_tick`](Self::push_tick) with the commands
/// that step accepted, and at every multiple of 120 ticks
/// [`checkpoint`](Self::checkpoint) with the state hash; at the end
/// [`finish`](Self::finish).
///
/// Bytes depend only on the header, the setup and the recorded commands.
///
/// ```
/// use st_sim::replay::{ReplayHeader, ReplayWriter};
/// use st_sim::{Setup, Sim};
///
/// let setup = Setup { ships: Vec::new(), pilots: Vec::new(), gravity_mu: 0.0 };
/// let mut sim = Sim::new(&setup, 7)?;
/// let header = ReplayHeader::new(&st_data::data_set_hash(&[])?, 7, "doc", Vec::new());
/// let writer = ReplayWriter::new(&header, &setup, sim.state_hash()?)?;
/// // No ticks were run: the file is the header, the setup and the end.
/// let bytes = writer.finish(0, sim.state_hash()?)?;
/// assert!(bytes.starts_with(&st_sim::replay::MAGIC));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct ReplayWriter {
    /// Completed chunks not yet taken.
    out: Vec<u8>,
    /// Bytes already handed out by `take_bytes`.
    taken: usize,
    limit: usize,
    open: OpenSegment,
    /// The tick the last closed segment ended at (the start tick at first).
    last_end: u64,
    last_end_hash: u64,
    segments: u64,
    /// Per pilot, the last `seq` recorded in the session.
    last_seq: BTreeMap<u64, u64>,
    /// A segment did not fit; nothing more is recorded.
    full: bool,
}

impl ReplayWriter {
    /// Starts a file: the magic, `HEAD` and `SETP`, with the default size
    /// limit ([`DEFAULT_MAX_BYTES`]). `start_hash` is `sim.state_hash()` right
    /// after `Sim::new(setup, header.seed)`.
    ///
    /// # Errors
    ///
    /// [`ReplayError::BadInput`] if `header.start_tick` is not 0, or the setup
    /// has a NaN or infinity; [`ReplayError::HeaderWrite`] if the header
    /// cannot be written or exceeds 64 KiB; [`ReplayError::TooLarge`].
    pub fn new(header: &ReplayHeader, setup: &Setup, start_hash: u64) -> Result<Self, ReplayError> {
        Self::with_limit(header, setup, start_hash, DEFAULT_MAX_BYTES)
    }

    /// Like [`ReplayWriter::new`] with a limit on the size of the file.
    ///
    /// # Errors
    ///
    /// As [`ReplayWriter::new`].
    pub fn with_limit(
        header: &ReplayHeader,
        setup: &Setup,
        start_hash: u64,
        limit: usize,
    ) -> Result<Self, ReplayError> {
        // `Sim::new` always starts at tick 0; a later start needs saves.
        if header.start_tick != 0 {
            return Err(ReplayError::BadInput("start_tick must be 0"));
        }
        let text = write_envelope(header).map_err(|e| ReplayError::HeaderWrite(e.to_string()))?;
        if text.len() > MAX_HEADER_BYTES {
            return Err(ReplayError::HeaderWrite(format!(
                "the header has {} bytes, more than {MAX_HEADER_BYTES}",
                text.len()
            )));
        }
        let mut setp = Vec::new();
        setp.extend_from_slice(&header.seed.to_le_bytes());
        setp.extend_from_slice(&start_hash.to_le_bytes());
        wire::encode_setup(&mut setp, setup)
            .map_err(|_| ReplayError::BadInput("the setup has a number that is not finite"))?;

        let mut out =
            Vec::with_capacity(MAGIC.len() + 2 * CHUNK_OVERHEAD + text.len() + setp.len());
        out.extend_from_slice(&MAGIC);
        put_chunk(&mut out, TAG_HEAD, text.as_bytes())?;
        put_chunk(&mut out, TAG_SETP, &setp)?;
        if out.len() + END_CHUNK_BYTES > limit {
            return Err(ReplayError::TooLarge {
                len: (out.len() + END_CHUNK_BYTES) as u64,
                limit: limit as u64,
            });
        }
        Ok(Self {
            out,
            taken: 0,
            limit,
            open: OpenSegment::new(header.start_tick),
            last_end: header.start_tick,
            last_end_hash: start_hash,
            segments: 0,
            last_seq: BTreeMap::new(),
            full: false,
        })
    }

    /// Records the commands `Sim::step` accepted at `tick`, in step order. A
    /// tick without commands has no record, so an empty slice is a no-op.
    ///
    /// Nothing is recorded if this fails.
    ///
    /// # Errors
    ///
    /// [`ReplayError::BadInput`] if `tick` is not above the previous recorded
    /// tick or is before the open segment; a command has another tick; the
    /// slice is not sorted strictly ascending by (pilot, `seq`), or a `seq` is
    /// 0 or not above the pilot's previous one; or a limit of ADR 0009 is
    /// exceeded (1,024 commands, 1 MiB per payload, 2 MiB per tick).
    /// [`ReplayError::TooLarge`] after an earlier `TooLarge`.
    pub fn push_tick(&mut self, tick: u64, commands: &[Command]) -> Result<(), ReplayError> {
        if self.full {
            return Err(self.too_large(0));
        }
        if commands.is_empty() {
            return Ok(());
        }
        let open = &self.open;
        if tick < open.first_tick || open.last_record.is_some_and(|last| tick <= last) {
            return Err(ReplayError::BadInput("ticks must be recorded in order"));
        }
        if commands.len() > MAX_COMMANDS_PER_TICK {
            return Err(ReplayError::BadInput("too many commands in one tick"));
        }

        // Encode into scratch first, so a refusal changes nothing.
        let base_tick = open.last_record.unwrap_or(open.first_tick);
        let mut record = Vec::new();
        wire::put_varint(&mut record, tick - base_tick);
        wire::put_varint(&mut record, commands.len() as u64);
        let mut previous: Option<(u64, u64)> = None;
        let mut tick_bytes = 0_usize;
        let mut base = open.base.clone();
        let mut last_seq = BTreeMap::new();
        for cmd in commands {
            if cmd.tick != tick {
                return Err(ReplayError::BadInput("a command has another tick"));
            }
            let key = (cmd.pilot.0, cmd.seq);
            if previous.is_some_and(|p| p >= key) {
                return Err(ReplayError::BadInput(
                    "commands must be sorted strictly ascending by pilot and seq",
                ));
            }
            previous = Some(key);
            let session_last = last_seq
                .get(&cmd.pilot.0)
                .or_else(|| self.last_seq.get(&cmd.pilot.0))
                .copied()
                .unwrap_or(0);
            if cmd.seq <= session_last {
                return Err(ReplayError::BadInput(
                    "seq must be above 0 and above the pilot's previous one",
                ));
            }
            let payload = wire::payload_len(&cmd.kind);
            tick_bytes += payload + wire::FRAME_OVERHEAD;
            if payload > MAX_PAYLOAD_BYTES || tick_bytes > MAX_TICK_BYTES {
                return Err(ReplayError::BadInput(
                    "a size limit of ADR 0009 is exceeded",
                ));
            }
            // The first command of a pilot in a segment stores its `seq`, the
            // rest the difference from the pilot's previous one. The bases
            // reset per segment, so each segment decodes on its own.
            let field = match base.get(&cmd.pilot.0) {
                Some(&prev) => cmd.seq - prev,
                None => cmd.seq,
            };
            base.insert(cmd.pilot.0, cmd.seq);
            last_seq.insert(cmd.pilot.0, cmd.seq);
            wire::encode_frame(&mut record, cmd.pilot.0, field, &cmd.kind);
        }

        let open = &mut self.open;
        open.body.extend_from_slice(&record);
        open.records += 1;
        open.last_record = Some(tick);
        open.base = base;
        self.last_seq.extend(last_seq);
        Ok(())
    }

    /// Closes the open segment at `tick`, a multiple of 120, with the state
    /// hash taken when `Sim::tick()` was `tick`.
    ///
    /// # Errors
    ///
    /// [`ReplayError::BadInput`] if `tick` is not a multiple of 120, is not
    /// above the segment's first tick, or is not above the last recorded
    /// tick. [`ReplayError::TooLarge`] if the segment would push the file
    /// past the size limit; the segment is dropped, nothing more can be
    /// recorded, and the session calls [`finish`](Self::finish) with the
    /// previous checkpoint's tick and hash and continues unrecorded. The
    /// file is complete and plays up to that checkpoint.
    pub fn checkpoint(&mut self, tick: u64, state_hash: u64) -> Result<(), ReplayError> {
        if !tick.is_multiple_of(CHECKPOINT_INTERVAL) {
            return Err(ReplayError::BadInput(
                "checkpoints are at multiples of 120 ticks",
            ));
        }
        self.close_segment(tick, state_hash)
    }

    /// Drains the completed chunks (and, the first time, the magic, `HEAD` and
    /// `SETP`) for the caller to append to storage. Concatenating everything
    /// taken, then what [`finish`](Self::finish) returns, is the file.
    pub fn take_bytes(&mut self) -> Vec<u8> {
        let bytes = core::mem::take(&mut self.out);
        self.taken += bytes.len();
        bytes
    }

    /// Ends the recording at `final_tick` with the state hash taken there and
    /// returns the bytes not yet taken: the last segment, if one is open, and
    /// `END_`.
    ///
    /// If `final_tick` is the last checkpoint, nothing is added but `END_` (this
    /// is also what ends a recording after a `TooLarge`). If the last segment
    /// does not fit within the limit, the file ends at the previous
    /// checkpoint instead.
    ///
    /// # Errors
    ///
    /// [`ReplayError::BadInput`] if `final_tick` is before the last checkpoint,
    /// the hash differs from the checkpoint's at the same tick, or commands
    /// were recorded past `final_tick`.
    pub fn finish(mut self, final_tick: u64, final_hash: u64) -> Result<Vec<u8>, ReplayError> {
        let (final_tick, final_hash) = if final_tick == self.last_end {
            if self.open.last_record.is_some_and(|t| t >= final_tick) {
                return Err(ReplayError::BadInput("commands were recorded past the end"));
            }
            if final_hash != self.last_end_hash {
                return Err(ReplayError::BadInput(
                    "the final hash differs from the last checkpoint's",
                ));
            }
            (final_tick, final_hash)
        } else if final_tick > self.last_end && !self.full {
            match self.close_segment(final_tick, final_hash) {
                Ok(()) => (final_tick, final_hash),
                Err(ReplayError::TooLarge { .. }) => (self.last_end, self.last_end_hash),
                Err(e) => return Err(e),
            }
        } else {
            return Err(ReplayError::BadInput(
                "the final tick is before the last checkpoint",
            ));
        };
        let mut end = Vec::with_capacity(24);
        end.extend_from_slice(&final_tick.to_le_bytes());
        end.extend_from_slice(&final_hash.to_le_bytes());
        end.extend_from_slice(&self.segments.to_le_bytes());
        put_chunk(&mut self.out, TAG_END, &end)?;
        Ok(self.out)
    }

    fn too_large(&self, extra: usize) -> ReplayError {
        ReplayError::TooLarge {
            len: (self.taken + self.out.len() + extra + END_CHUNK_BYTES) as u64,
            limit: self.limit as u64,
        }
    }

    fn close_segment(&mut self, end_tick: u64, end_hash: u64) -> Result<(), ReplayError> {
        if self.full {
            return Err(self.too_large(0));
        }
        let first = self.open.first_tick;
        let ticks = end_tick.saturating_sub(first);
        if ticks == 0 || ticks > CHECKPOINT_INTERVAL {
            return Err(ReplayError::BadInput(
                "a segment covers 1 to 120 ticks and ends at the tick given",
            ));
        }
        if self.open.last_record.is_some_and(|t| t >= end_tick) {
            return Err(ReplayError::BadInput("commands were recorded past the end"));
        }
        let mut payload = Vec::with_capacity(self.open.body.len() + 32);
        payload.extend_from_slice(&first.to_le_bytes());
        // At most 120, so it fits.
        payload.extend_from_slice(&(ticks as u32).to_le_bytes());
        wire::put_varint(&mut payload, self.open.records);
        payload.extend_from_slice(&self.open.body);
        payload.extend_from_slice(&end_hash.to_le_bytes());

        let size = payload.len() + CHUNK_OVERHEAD;
        if self.taken + self.out.len() + size + END_CHUNK_BYTES > self.limit {
            // Drop the segment (ADR 0009, "Properties and limits").
            self.full = true;
            self.open = OpenSegment::new(first);
            return Err(self.too_large(size));
        }
        put_chunk(&mut self.out, TAG_SEGM, &payload)?;
        self.segments += 1;
        self.last_end = end_tick;
        self.last_end_hash = end_hash;
        self.open = OpenSegment::new(end_tick);
        Ok(())
    }
}

impl OpenSegment {
    fn new(first_tick: u64) -> Self {
        Self {
            first_tick,
            body: Vec::new(),
            records: 0,
            last_record: None,
            base: BTreeMap::new(),
        }
    }
}
