//! Reading and playing replay files. Works on a byte slice already in memory.

use std::collections::BTreeMap;
use std::ops::Range;

use st_data::{Code, DataSetHash, Loader};

use super::{
    MAGIC, MAX_HEADER_BYTES, PlayContext, ReplayError, ReplayHeader, TAG_END, TAG_HEAD, TAG_SEGM,
    TAG_SETP, chunk_check,
};
use crate::command::wire::{self, Decoder, WireError};
use crate::command::{Command, MAX_COMMANDS_PER_TICK, PilotId};
use crate::sim::{CHECKPOINT_INTERVAL, Setup, Sim};

/// Fewest bytes of a `SEGM` payload: `first_tick`, `tick_count`, a one-byte
/// record count and `end_hash`.
const SEGM_MIN: usize = 8 + 4 + 1 + 8;
/// Fewest bytes of one record: a tick delta, a command count and a command of
/// four varints.
const RECORD_MIN: usize = 1 + 1 + 4;

/// One chunk, checked.
struct Chunk<'a> {
    tag: [u8; 4],
    payload: &'a [u8],
    /// File offset of the payload.
    payload_offset: usize,
    /// File offset after the chunk.
    end: usize,
}

enum ChunkFault {
    Truncated,
    Corrupt { offset: usize },
}

/// Reads the chunk at `pos`: `Ok(None)` at the end of the bytes.
fn read_chunk(bytes: &[u8], pos: usize) -> Result<Option<Chunk<'_>>, ChunkFault> {
    let Some(rest) = bytes.get(pos..).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    let mut d = Decoder::new(rest);
    let (Ok(tag), Ok(len)) = (d.take(4), d.u32()) else {
        return Err(ChunkFault::Truncated);
    };
    // `usize` is 32 bits on wasm32: stay in checked arithmetic.
    let len = usize::try_from(len).map_err(|_| ChunkFault::Truncated)?;
    let body = len.checked_add(8).ok_or(ChunkFault::Truncated)?;
    let total = body.checked_add(8).ok_or(ChunkFault::Truncated)?;
    if rest.len() < total {
        return Err(ChunkFault::Truncated);
    }
    let (covered, check) = rest[..total].split_at(body);
    let mut want = [0_u8; 8];
    want.copy_from_slice(check);
    if chunk_check(covered) != u64::from_le_bytes(want) {
        return Err(ChunkFault::Corrupt { offset: pos });
    }
    let mut t = [0_u8; 4];
    t.copy_from_slice(tag);
    Ok(Some(Chunk {
        tag: t,
        payload: &rest[8..body],
        payload_offset: pos + 8,
        end: pos + total,
    }))
}

fn malformed(offset: usize, error: WireError) -> ReplayError {
    ReplayError::Malformed { offset, error }
}

/// Checks the magic. Anything that does not start with it, a prefix of it
/// or an empty input included, is not a replay (ADR 0009).
fn check_magic(bytes: &[u8]) -> Result<(), ReplayError> {
    if bytes.starts_with(&MAGIC) {
        Ok(())
    } else {
        Err(ReplayError::NotAReplay)
    }
}

fn load_header(payload: &[u8]) -> Result<ReplayHeader, ReplayError> {
    let loader = Loader::new().with_max_bytes(MAX_HEADER_BYTES);
    match loader.load::<ReplayHeader>("replay header", payload) {
        Ok(loaded) => Ok(loaded.value),
        Err(diagnostics) => {
            if diagnostics.iter().any(|d| d.code == Code::VersionTooNew) {
                let header_version = loader
                    .peek_header("replay header", payload)
                    .map_or(0, |h| h.version);
                return Err(ReplayError::NewerBuild {
                    header_version,
                    supported: <ReplayHeader as st_data::Schema>::VERSION,
                });
            }
            Err(ReplayError::Header(diagnostics))
        }
    }
}

/// Reads the `HEAD` chunk of any replay, whatever build wrote it, without
/// looking at the rest.
///
/// This is how a file list shows the recording build, the seed and the
/// versions, and how a tool decides whether a replay can be played here.
///
/// # Errors
///
/// [`ReplayError::NotAReplay`], [`ReplayError::Truncated`],
/// [`ReplayError::Corrupt`] for a failed check, [`ReplayError::NewerBuild`]
/// for a header version this build does not read, and
/// [`ReplayError::Header`] for an invalid header.
///
/// ```
/// use st_sim::replay::{ReplayHeader, ReplayWriter, read_header};
/// use st_sim::{Setup, Sim};
///
/// let setup = Setup { ships: Vec::new(), pilots: Vec::new(), gravity_mu: 0.0 };
/// let sim = Sim::new(&setup, 9)?;
/// let header = ReplayHeader::new(&st_data::data_set_hash(&[])?, 9, "doc", vec![1]);
/// let bytes = ReplayWriter::new(&header, &setup, sim.state_hash()?)?.finish(0, sim.state_hash()?)?;
/// assert_eq!(read_header(&bytes)?, header);
/// assert!(read_header(b"not a replay").is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn read_header(bytes: &[u8]) -> Result<ReplayHeader, ReplayError> {
    check_magic(bytes)?;
    let (chunk, _) = first_chunk(bytes, MAGIC.len())?;
    load_header(chunk.payload)
}

/// The `HEAD` chunk at `pos`, and its position.
fn first_chunk(bytes: &[u8], pos: usize) -> Result<(Chunk<'_>, usize), ReplayError> {
    match read_chunk(bytes, pos) {
        Ok(Some(chunk)) if chunk.tag == TAG_HEAD => {
            if chunk.payload.len() > MAX_HEADER_BYTES {
                return Err(malformed(pos, WireError::TooLong));
            }
            let end = chunk.end;
            Ok((chunk, end))
        }
        Ok(Some(_)) => Err(malformed(
            pos,
            WireError::Invalid("the first chunk is not HEAD"),
        )),
        Ok(None) | Err(ChunkFault::Truncated) => Err(ReplayError::Truncated {
            last_complete_tick: None,
        }),
        Err(ChunkFault::Corrupt { offset }) => Err(ReplayError::Corrupt {
            offset,
            what: "chunk check failed",
        }),
    }
}

/// Where a segment lies in the file, and its fixed fields.
#[derive(Clone, Debug)]
struct SegmentIndex {
    payload: Range<usize>,
    first_tick: u64,
    tick_count: u32,
    end_hash: u64,
}

#[derive(Clone, Copy, Debug)]
struct End {
    final_tick: u64,
    final_hash: u64,
}

/// An opened replay: header, setup and an index of its segments.
///
/// [`ReplayReader::open`] checks the container (every chunk's check, the
/// chunk order, that the segments follow one another and end on checkpoints),
/// the versions and the data set, and builds nothing larger than the setup.
/// The commands are decoded a segment at a time.
#[derive(Clone, Debug)]
pub struct ReplayReader<'a> {
    bytes: &'a [u8],
    header: ReplayHeader,
    setup: Setup,
    start_hash: u64,
    segments: Vec<SegmentIndex>,
    end: Option<End>,
    /// The last tick of the last complete segment, if the file has no `END_`.
    truncated_at: Option<u64>,
    check_hashes: bool,
}

impl<'a> ReplayReader<'a> {
    /// Opens a replay, in the order of ADR 0009: magic; the header through
    /// the ADR 0010 loader (wrong kind, a newer header version, older
    /// versions migrated); `sim_version` and `state_hash_version` equal to
    /// the context's, then `data_set_hash`; `SETP`, whose seed must match the
    /// header's, then `Sim::new(setup, seed)` and the start tick and hash.
    /// Then it indexes the segments.
    ///
    /// A file that stops after a complete chunk or inside a chunk opens, with
    /// [`truncated_at`](Self::truncated_at) set: it plays up to there.
    ///
    /// # Errors
    ///
    /// A [`ReplayError`]; none is a panic, whatever the bytes are. Notably
    /// [`ReplayError::SimVersionMismatch`], whose message names the recording
    /// build and the version this build plays, and
    /// [`ReplayError::Corrupt`] (a failed check) which is never a
    /// [`ReplayError::Desync`].
    pub fn open(bytes: &'a [u8], ctx: &PlayContext) -> Result<Self, ReplayError> {
        if bytes.len() > ctx.max_bytes {
            return Err(ReplayError::TooLarge {
                len: bytes.len() as u64,
                limit: ctx.max_bytes as u64,
            });
        }
        check_magic(bytes)?;
        let (head, mut pos) = first_chunk(bytes, MAGIC.len())?;
        let header = load_header(head.payload)?;
        if ctx.check_versions {
            if header.sim_version != ctx.sim_version {
                return Err(ReplayError::SimVersionMismatch {
                    recorded: header.sim_version,
                    recorded_build: header.build.clone(),
                    current: ctx.sim_version,
                });
            }
            if header.state_hash_version != ctx.state_hash_version {
                return Err(ReplayError::HashFormatMismatch {
                    recorded: header.state_hash_version,
                    current: ctx.state_hash_version,
                });
            }
            // The header validated, so the hash parses.
            let recorded = header.data_set_hash.parse::<DataSetHash>();
            if recorded.as_ref() != Ok(&ctx.data_set_hash) {
                return Err(ReplayError::DataSetMismatch {
                    recorded: header.data_set_hash.clone(),
                    current: ctx.data_set_hash.to_string(),
                });
            }
        }

        let setp = match read_chunk(bytes, pos) {
            Ok(Some(chunk)) if chunk.tag == TAG_SETP => chunk,
            Ok(Some(_)) => {
                return Err(malformed(
                    pos,
                    WireError::Invalid("the second chunk is not SETP"),
                ));
            }
            Ok(None) | Err(ChunkFault::Truncated) => {
                return Err(ReplayError::Truncated {
                    last_complete_tick: None,
                });
            }
            Err(ChunkFault::Corrupt { offset }) => {
                return Err(ReplayError::Corrupt {
                    offset,
                    what: "chunk check failed",
                });
            }
        };
        pos = setp.end;
        let mut d = Decoder::new(setp.payload);
        let decoded = (|| Ok((d.u64()?, d.u64()?, wire::decode_setup(&mut d)?)))()
            .and_then(|v| d.finish().map(|()| v));
        let (seed, start_hash, setup) =
            decoded.map_err(|e| malformed(setp.payload_offset + d.position(), e))?;
        if seed != header.seed {
            return Err(ReplayError::Corrupt {
                offset: setp.payload_offset,
                what: "the header and SETP name different seeds",
            });
        }

        let mut reader = Self {
            bytes,
            header,
            setup,
            start_hash,
            segments: Vec::new(),
            end: None,
            truncated_at: None,
            check_hashes: ctx.check_versions,
        };
        let sim = reader.new_sim()?;
        if reader.check_hashes {
            reader.check_start(&sim)?;
        }
        reader.index_body(pos)?;
        Ok(reader)
    }

    /// Indexes the `SEGM` chunks and the `END_` chunk from `pos`.
    fn index_body(&mut self, mut pos: usize) -> Result<(), ReplayError> {
        let start_tick = self.header.start_tick;
        let mut next_tick = start_tick;
        loop {
            let chunk = match read_chunk(self.bytes, pos) {
                // The file stops cleanly at a chunk boundary, or inside a
                // chunk, without `END_`: a truncated recording.
                Ok(None) | Err(ChunkFault::Truncated) => {
                    self.truncated_at = Some(next_tick);
                    return Ok(());
                }
                Err(ChunkFault::Corrupt { offset }) => {
                    return Err(ReplayError::Corrupt {
                        offset,
                        what: "chunk check failed",
                    });
                }
                Ok(Some(chunk)) => chunk,
            };
            match chunk.tag {
                TAG_SEGM => {
                    let seg = index_segment(&chunk, next_tick, !self.segments.is_empty())?;
                    next_tick = seg.first_tick + u64::from(seg.tick_count);
                    self.segments.push(seg);
                    pos = chunk.end;
                }
                TAG_END => {
                    let mut d = Decoder::new(chunk.payload);
                    let fields = (|| Ok((d.u64()?, d.u64()?, d.u64()?)))()
                        .and_then(|v| d.finish().map(|()| v));
                    let (final_tick, final_hash, count) =
                        fields.map_err(|e| malformed(chunk.payload_offset + d.position(), e))?;
                    let last_hash = self.segments.last().map_or(self.start_hash, |s| s.end_hash);
                    if final_tick != next_tick
                        || final_hash != last_hash
                        || count != self.segments.len() as u64
                    {
                        return Err(ReplayError::Corrupt {
                            offset: chunk.payload_offset,
                            what: "END_ disagrees with the segments",
                        });
                    }
                    if chunk.end != self.bytes.len() {
                        return Err(malformed(chunk.end, WireError::Invalid("bytes after END_")));
                    }
                    self.end = Some(End {
                        final_tick,
                        final_hash,
                    });
                    return Ok(());
                }
                _ => {
                    return Err(malformed(
                        pos,
                        WireError::Invalid("unknown chunk in container version 1"),
                    ));
                }
            }
        }
    }

    /// The header.
    pub fn header(&self) -> &ReplayHeader {
        &self.header
    }

    /// The recorded setup.
    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    /// The authoritative world seed (`SETP`'s).
    pub fn seed(&self) -> u64 {
        self.header.seed
    }

    /// The state hash right after `Sim::new`, as recorded.
    pub fn start_hash(&self) -> u64 {
        self.start_hash
    }

    /// How many complete segments the file has.
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// The final tick and hash from `END_`, or `None` for a truncated file.
    pub fn end(&self) -> Option<(u64, u64)> {
        self.end.map(|e| (e.final_tick, e.final_hash))
    }

    /// For a file without `END_`, the last tick of its last complete segment:
    /// the replay plays up to there.
    pub fn truncated_at(&self) -> Option<u64> {
        self.truncated_at
    }

    /// Builds the simulation the replay starts from: `Sim::new(setup, seed)`.
    ///
    /// # Errors
    ///
    /// [`ReplayError::Setup`] if the setup is refused, and
    /// [`ReplayError::Corrupt`] if the start tick in the header is not the
    /// tick of a new simulation.
    pub fn new_sim(&self) -> Result<Sim, ReplayError> {
        let sim = Sim::new(&self.setup, self.header.seed).map_err(ReplayError::Setup)?;
        if sim.tick() != self.header.start_tick {
            return Err(ReplayError::Corrupt {
                offset: MAGIC.len(),
                what: "the header's start_tick is not the start of the simulation",
            });
        }
        Ok(sim)
    }

    fn check_start(&self, sim: &Sim) -> Result<(), ReplayError> {
        let actual = sim.state_hash().map_err(ReplayError::Hash)?;
        if actual == self.start_hash {
            Ok(())
        } else {
            Err(ReplayError::Desync {
                checkpoint_tick: sim.tick(),
                expected: self.start_hash,
                actual,
                last_good_tick: None,
            })
        }
    }

    /// Segment `index`, ready to decode.
    ///
    /// # Errors
    ///
    /// [`ReplayError::Truncated`] one past the last segment of a truncated
    /// file (with that segment's end tick), and [`ReplayError::NoSuchSegment`]
    /// anywhere else past the end.
    pub fn segment(&self, index: usize) -> Result<Segment<'a>, ReplayError> {
        let Some(seg) = self.segments.get(index) else {
            return Err(match self.truncated_at {
                Some(t) if index == self.segments.len() => ReplayError::Truncated {
                    last_complete_tick: Some(t),
                },
                _ => ReplayError::NoSuchSegment(index),
            });
        };
        let body_start = seg.payload.start + 12;
        let body_end = seg.payload.end - 8;
        Ok(Segment {
            first_tick: seg.first_tick,
            tick_count: seg.tick_count,
            end_hash: seg.end_hash,
            body: &self.bytes[body_start..body_end],
            body_offset: body_start,
        })
    }

    /// Plays the replay in a fresh simulation and compares the state hash at
    /// every checkpoint; see [`ReplayReader::play`].
    ///
    /// # Errors
    ///
    /// As [`ReplayReader::play`].
    pub fn verify(&self) -> Result<Verified, ReplayError> {
        self.play(|_| {})
    }

    /// Plays the replay: builds the simulation, steps it through every
    /// complete segment with the recorded commands, and compares the state
    /// hash at the end of each segment with the file's. `observer` sees the
    /// simulation at the start and after every step (the harness uses it to
    /// print a hash per tick).
    ///
    /// The first mismatch stops the playback with [`ReplayError::Desync`];
    /// the first divergent tick lies after `last_good_tick`. A damaged file
    /// is reported as [`ReplayError::Corrupt`] when it is opened, so it never
    /// looks like a desync.
    ///
    /// # Errors
    ///
    /// [`ReplayError::Desync`], [`ReplayError::Step`] if the simulation refuses
    /// a recorded tick, [`ReplayError::Malformed`] for a segment that does not
    /// decode, [`ReplayError::Setup`] and [`ReplayError::Hash`].
    pub fn play(&self, mut observer: impl FnMut(&Sim)) -> Result<Verified, ReplayError> {
        let mut sim = self.new_sim()?;
        // Always checked here, so `--force` reports a start that differs.
        self.check_start(&sim)?;
        observer(&sim);
        let mut last_good = sim.tick();
        let mut last_hash = self.start_hash;
        for index in 0..self.segments.len() {
            let segment = self.segment(index)?;
            let records = segment.records()?;
            let mut next = records.iter().peekable();
            for _ in 0..segment.tick_count {
                let tick = sim.tick();
                let commands: &[Command] = match next.next_if(|r| r.tick == tick) {
                    Some(record) => &record.commands,
                    None => &[],
                };
                sim.step(commands)
                    .map_err(|error| ReplayError::Step { tick, error })?;
                observer(&sim);
            }
            let actual = sim.state_hash().map_err(ReplayError::Hash)?;
            if actual != segment.end_hash {
                return Err(ReplayError::Desync {
                    checkpoint_tick: sim.tick(),
                    expected: segment.end_hash,
                    actual,
                    last_good_tick: Some(last_good),
                });
            }
            last_good = sim.tick();
            last_hash = actual;
        }
        Ok(Verified {
            final_tick: last_good,
            final_hash: last_hash,
            segments: self.segments.len(),
            complete: self.end.is_some(),
        })
    }
}

/// The result of playing a replay to its end without a desync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verified {
    /// The tick the playback reached.
    pub final_tick: u64,
    /// The state hash there.
    pub final_hash: u64,
    /// How many segments were checked.
    pub segments: usize,
    /// `false` if the file has no `END_` (a truncated recording): everything
    /// that is in it matched, and it ends at `final_tick`.
    pub complete: bool,
}

/// Checks the fixed fields of a `SEGM` chunk.
fn index_segment(
    chunk: &Chunk<'_>,
    expected_first: u64,
    after_a_segment: bool,
) -> Result<SegmentIndex, ReplayError> {
    let at = chunk.payload_offset;
    let p = chunk.payload;
    if p.len() < SEGM_MIN {
        return Err(malformed(at, WireError::Truncated));
    }
    let mut d = Decoder::new(p);
    // The slice is long enough, so these three cannot fail.
    let first_tick = d.u64().map_err(|e| malformed(at, e))?;
    let tick_count = d.u32().map_err(|e| malformed(at + 8, e))?;
    let mut tail = Decoder::new(&p[p.len() - 8..]);
    let end_hash = tail.u64().map_err(|e| malformed(at, e))?;

    // Version 1 segments hold 1 to 120 ticks, end where the tick is a multiple
    // of 120 (except the last one), and follow one another.
    if first_tick != expected_first {
        return Err(malformed(
            at,
            WireError::Invalid("a segment does not continue the previous one"),
        ));
    }
    if after_a_segment && first_tick % CHECKPOINT_INTERVAL != 0 {
        return Err(malformed(
            at,
            WireError::Invalid("a segment ends off a checkpoint and is not the last"),
        ));
    }
    let limit = (first_tick / CHECKPOINT_INTERVAL + 1).saturating_mul(CHECKPOINT_INTERVAL);
    let within = |end: u64| end <= limit;
    first_tick
        .checked_add(u64::from(tick_count))
        .filter(|&end| tick_count >= 1 && within(end))
        .ok_or_else(|| malformed(at + 8, WireError::Invalid("a segment spans too many ticks")))?;
    Ok(SegmentIndex {
        payload: at..at + p.len(),
        first_tick,
        tick_count,
        end_hash,
    })
}

/// One decoded segment header and its undecoded records.
#[derive(Clone, Copy, Debug)]
pub struct Segment<'a> {
    /// The tick the segment starts at.
    pub first_tick: u64,
    /// How many ticks it covers (1 to 120).
    pub tick_count: u32,
    /// The state hash when the simulation reaches `first_tick + tick_count`.
    pub end_hash: u64,
    body: &'a [u8],
    body_offset: usize,
}

/// The commands of one tick, as decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct TickRecord {
    /// The tick the commands apply in.
    pub tick: u64,
    /// The commands, in (pilot, `seq`) order, with absolute sequence numbers.
    pub commands: Vec<Command>,
}

impl Segment<'_> {
    /// The tick the playback is at when the segment is done.
    pub fn end_tick(&self) -> u64 {
        self.first_tick + u64::from(self.tick_count)
    }

    /// Decodes the records: ticks in ascending order within the segment, each
    /// with 1 to 1,024 commands in (pilot, `seq`) order.
    ///
    /// # Errors
    ///
    /// [`ReplayError::Malformed`] if anything is outside the format: a tick
    /// delta of 0 (after the first), a record past the segment, an unsorted
    /// tick, a sequence difference of 0, a limit of ADR 0009 exceeded, an
    /// unknown tag, or bytes left over.
    pub fn records(&self) -> Result<Vec<TickRecord>, ReplayError> {
        let mut d = Decoder::new(self.body);
        self.decode(&mut d)
            .map_err(|e| malformed(self.body_offset + d.position(), e))
    }

    fn decode(&self, d: &mut Decoder<'_>) -> Result<Vec<TickRecord>, WireError> {
        let end_tick = self.end_tick();
        let count = d.count(RECORD_MIN)?;
        if count as u64 > u64::from(self.tick_count) {
            return Err(WireError::Invalid("more records than ticks"));
        }
        let mut records = Vec::with_capacity(count);
        let mut base = self.first_tick;
        // Each segment decodes on its own: the sequence bases start empty.
        let mut last_seq: BTreeMap<u64, u64> = BTreeMap::new();
        for i in 0..count {
            let delta = d.varint()?;
            if i > 0 && delta == 0 {
                return Err(WireError::Invalid("a tick delta of zero"));
            }
            let tick = base.checked_add(delta).ok_or(WireError::Overflow)?;
            if tick >= end_tick {
                return Err(WireError::Invalid("a record lies past the segment"));
            }
            base = tick;
            let n = d.count(4)?;
            if n == 0 || n > MAX_COMMANDS_PER_TICK {
                return Err(WireError::Invalid("a tick has no commands or too many"));
            }
            let mut commands = Vec::with_capacity(n);
            let mut previous_pilot = 0_u64;
            let mut tick_bytes = 0_usize;
            for _ in 0..n {
                let (pilot, field, kind) = wire::decode_frame(d)?;
                if pilot < previous_pilot {
                    return Err(WireError::Invalid("commands are not sorted by pilot"));
                }
                previous_pilot = pilot;
                let seq = match last_seq.get(&pilot) {
                    None if field == 0 => {
                        return Err(WireError::Invalid("a sequence number of zero"));
                    }
                    None => field,
                    Some(_) if field == 0 => {
                        return Err(WireError::Invalid("a sequence difference of zero"));
                    }
                    Some(&prev) => prev.checked_add(field).ok_or(WireError::Overflow)?,
                };
                last_seq.insert(pilot, seq);
                tick_bytes =
                    tick_bytes.saturating_add(wire::payload_len(&kind) + wire::FRAME_OVERHEAD);
                if tick_bytes > wire::MAX_TICK_BYTES {
                    return Err(WireError::TooLong);
                }
                commands.push(Command {
                    tick,
                    pilot: PilotId(pilot),
                    seq,
                    kind,
                });
            }
            records.push(TickRecord { tick, commands });
        }
        d.finish()?;
        Ok(records)
    }
}
