//! The wire encoding of commands and of a simulation setup (ADR 0009,
//! "Wire encoding of commands").
//!
//! One hand-written codec, used by replay files ([`crate::replay`]) and, later,
//! by network messages. There is no serde: field order and tags are explicit
//! in code, so a format change is visible in review.
//!
//! * A *varint* is unsigned LEB128, at most 10 bytes for a `u64`. A
//!   non-minimal encoding and an overflow are errors, so each value has
//!   exactly one encoding.
//! * Payload fields are fixed-width little-endian integers. A `bool` is one
//!   byte (0 or 1). An `f64` is its bit pattern, little-endian; NaN and
//!   infinities are errors and `-0.0` is kept. An `Option` is a tag byte, 0 or
//!   one, followed by the value. A sequence is a varint count followed by its
//!   elements.
//! * A *command frame* is `pilot` (varint), `seq` (varint), `tag` (varint),
//!   payload length (varint) and the payload. What the `seq` field means (the
//!   absolute number or the difference from the pilot's previous command) is
//!   up to the container: a replay segment stores differences.
//! * Payloads have no version of their own: replays are version-locked, so
//!   [`SIM_VERSION`](crate::SIM_VERSION) identifies every layout.
//!
//! Nothing here panics on any input; every decoder returns a [`WireError`].
//! Counts and lengths are checked against the bytes that remain before
//! anything is allocated.
//!
//! # Example
//!
//! ```
//! use st_sim::command::wire::{self, Decoder};
//! use st_sim::{CommandKind, ControlState};
//!
//! let kind = CommandKind::Controls(ControlState { thrust: 32767, ..ControlState::default() });
//! let mut bytes = Vec::new();
//! wire::encode_frame(&mut bytes, 1, 1, &kind);
//!
//! let mut d = Decoder::new(&bytes);
//! let (pilot, seq, decoded) = wire::decode_frame(&mut d)?;
//! d.finish()?;
//! assert_eq!((pilot, seq, decoded), (1, 1, kind));
//! # Ok::<(), wire::WireError>(())
//! ```

use st_math::glam::{DQuat, DVec3};

use super::{CommandKind, ControlState, PilotId};
use crate::hull::{HullPart, PartShape, Weapon};
use crate::sim::{AiParams, Setup, ShipInit};

/// Wire tag of [`CommandKind::Controls`]. Tags are never reused, even after
/// a variant is removed. Ranges: 1 to 999 are pilot kinds, 1000 to 1999
/// session kinds and 2000 to 2999 external-provider kinds.
pub const TAG_CONTROLS: u32 = 1;
/// Reserved, not implemented: add a pilot (session kind).
pub const TAG_ADD_PILOT: u32 = 1000;
/// Reserved, not implemented: remove a pilot (session kind).
pub const TAG_REMOVE_PILOT: u32 = 1001;
/// Reserved, not implemented: hand a pilot over between AI and an external
/// source (session kind).
pub const TAG_SET_CONTROLLER: u32 = 1002;
/// Reserved, not implemented: name a data-set hash at a tick boundary
/// (session kind).
pub const TAG_DATA_SET_CHANGE: u32 = 1003;
/// Reserved, not implemented: a narrative provider's output (external
/// provider kind, PD-05).
pub const TAG_NARRATIVE: u32 = 2000;

/// The reserved tags and their variant names, for diagnostics.
pub const RESERVED_TAGS: [(u32, &str); 5] = [
    (TAG_ADD_PILOT, "AddPilot"),
    (TAG_REMOVE_PILOT, "RemovePilot"),
    (TAG_SET_CONTROLLER, "SetController"),
    (TAG_DATA_SET_CHANGE, "DataSetChange"),
    (TAG_NARRATIVE, "Narrative"),
];

/// Largest payload of one command, in bytes (1 MiB).
pub const MAX_PAYLOAD_BYTES: usize = 1 << 20;
/// Bytes of framing counted per command against [`MAX_TICK_BYTES`]: pilot and
/// sequence varints of 10 bytes each, a 5-byte tag and a 10-byte length, plus
/// 5 bytes of slack.
pub const FRAME_OVERHEAD: usize = 40;
/// Largest size of one tick's commands, in bytes (2 MiB), counted as the sum
/// over the tick's commands of payload length plus [`FRAME_OVERHEAD`].
pub const MAX_TICK_BYTES: usize = 2 << 20;

/// Bytes of a `Controls` payload without `aim`: three `i16`, a tag byte and
/// the `u32` button set.
const CONTROLS_BASE: usize = 2 + 2 + 2 + 1 + 4;

/// Why some bytes could not be decoded (or a value not encoded).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The input ends before the value does.
    Truncated,
    /// A varint is not minimal (it has trailing zero groups).
    OverlongVarint,
    /// A varint does not fit in a `u64`, or a number does not fit its field.
    Overflow,
    /// A `bool` or `Option` tag byte is neither 0 nor 1.
    BadFlag,
    /// An `f64` is NaN or infinite.
    NonFinite,
    /// A command tag that no variant uses.
    UnknownTag(u32),
    /// A command tag that is reserved and not implemented yet.
    ReservedTag(u32),
    /// A length or count is above its limit or above the bytes remaining.
    TooLong,
    /// A value was decoded but bytes of its field are left over.
    TrailingBytes,
    /// A structural rule is broken; the text says which.
    Invalid(&'static str),
}

impl core::fmt::Display for WireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WireError::Truncated => f.write_str("the data ends too early"),
            WireError::OverlongVarint => f.write_str("a number is not in its shortest form"),
            WireError::Overflow => f.write_str("a number does not fit its field"),
            WireError::BadFlag => f.write_str("a flag byte is neither 0 nor 1"),
            WireError::NonFinite => f.write_str("a number is NaN or infinite"),
            WireError::UnknownTag(t) => write!(f, "unknown command tag {t}"),
            WireError::ReservedTag(t) => write!(f, "command tag {t} is reserved, not implemented"),
            WireError::TooLong => f.write_str("a length or count is too large"),
            WireError::TrailingBytes => f.write_str("unexpected bytes after a value"),
            WireError::Invalid(what) => f.write_str(what),
        }
    }
}

impl core::error::Error for WireError {}

// ---- Encoding -------------------------------------------------------------

/// Appends `value` as an unsigned LEB128 varint (1 to 10 bytes).
///
/// ```
/// let mut out = Vec::new();
/// st_sim::command::wire::put_varint(&mut out, 300);
/// assert_eq!(out, [0xac, 0x02]);
/// ```
pub fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let low = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

/// The number of bytes [`put_varint`] writes for `value`.
pub fn varint_len(value: u64) -> usize {
    let bits = 64 - (value | 1).leading_zeros() as usize;
    bits.div_ceil(7)
}

pub(crate) fn put_f64(out: &mut Vec<u8>, value: f64) -> Result<(), WireError> {
    if !value.is_finite() {
        return Err(WireError::NonFinite);
    }
    out.extend_from_slice(&value.to_bits().to_le_bytes());
    Ok(())
}

/// The tag of a command kind.
pub fn kind_tag(kind: &CommandKind) -> u32 {
    match kind {
        CommandKind::Controls(_) => TAG_CONTROLS,
    }
}

/// The exact length of the payload that [`encode_payload`] writes. It is
/// computed without encoding, so `Sim::step` can check the size limits
/// without allocating.
///
/// ```
/// use st_sim::command::wire::payload_len;
/// use st_sim::{CommandKind, ControlState};
///
/// let held = CommandKind::Controls(ControlState::default());
/// let aimed = CommandKind::Controls(ControlState { aim: Some(0), ..ControlState::default() });
/// assert_eq!((payload_len(&held), payload_len(&aimed)), (11, 15));
/// ```
pub fn payload_len(kind: &CommandKind) -> usize {
    match kind {
        CommandKind::Controls(c) => CONTROLS_BASE + if c.aim.is_some() { 4 } else { 0 },
    }
}

/// Appends the payload of `kind` (without tag or length).
pub fn encode_payload(out: &mut Vec<u8>, kind: &CommandKind) {
    match kind {
        CommandKind::Controls(c) => {
            out.extend_from_slice(&c.thrust.to_le_bytes());
            out.extend_from_slice(&c.strafe.to_le_bytes());
            out.extend_from_slice(&c.turn.to_le_bytes());
            match c.aim {
                Some(aim) => {
                    out.push(1);
                    out.extend_from_slice(&aim.to_le_bytes());
                }
                None => out.push(0),
            }
            out.extend_from_slice(&c.buttons.to_le_bytes());
        }
    }
}

/// Appends a command frame: `pilot`, `seq_field`, the tag, the payload length
/// and the payload. `seq_field` is stored as given (see the module docs).
pub fn encode_frame(out: &mut Vec<u8>, pilot: u64, seq_field: u64, kind: &CommandKind) {
    put_varint(out, pilot);
    put_varint(out, seq_field);
    put_varint(out, u64::from(kind_tag(kind)));
    put_varint(out, payload_len(kind) as u64);
    encode_payload(out, kind);
}

// ---- Decoding -------------------------------------------------------------

/// Reads values from a byte slice. Every read checks the bytes that remain.
///
/// ```
/// use st_sim::command::wire::{Decoder, WireError};
///
/// let mut d = Decoder::new(&[0xac, 0x02, 7]);
/// assert_eq!(d.varint()?, 300);
/// assert_eq!(d.u8()?, 7);
/// d.finish()?;
/// assert_eq!(Decoder::new(&[0x80, 0x00]).varint(), Err(WireError::OverlongVarint));
/// # Ok::<(), WireError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Decoder<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    /// Starts reading at the first byte of `bytes`.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// How many bytes have been consumed.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// How many bytes are left.
    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    /// Succeeds only if every byte has been consumed.
    ///
    /// # Errors
    ///
    /// [`WireError::TrailingBytes`] if bytes are left.
    pub fn finish(&self) -> Result<(), WireError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(WireError::TrailingBytes)
        }
    }

    /// Takes the next `n` bytes.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] if fewer than `n` remain.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.pos.checked_add(n).ok_or(WireError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(WireError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let slice = self.take(N)?;
        let mut out = [0_u8; N];
        out.copy_from_slice(slice);
        Ok(out)
    }

    /// Reads one byte.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`].
    pub fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.array::<1>()?[0])
    }

    /// Reads a little-endian `i16`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`].
    pub fn i16(&mut self) -> Result<i16, WireError> {
        Ok(i16::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `u32`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`].
    pub fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `u64`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`].
    pub fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// Reads a finite `f64` from its bit pattern.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`], or [`WireError::NonFinite`] for NaN and
    /// infinities.
    pub fn f64(&mut self) -> Result<f64, WireError> {
        let value = f64::from_bits(self.u64()?);
        if value.is_finite() {
            Ok(value)
        } else {
            Err(WireError::NonFinite)
        }
    }

    /// Reads a flag byte: 0 or 1.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`], or [`WireError::BadFlag`] for any other value.
    pub fn flag(&mut self) -> Result<bool, WireError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WireError::BadFlag),
        }
    }

    /// Reads an unsigned LEB128 varint in its shortest form.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`], [`WireError::OverlongVarint`] for a
    /// non-minimal encoding, or [`WireError::Overflow`] above `u64::MAX`.
    pub fn varint(&mut self) -> Result<u64, WireError> {
        let mut value = 0_u64;
        for i in 0..10_usize {
            let byte = self.u8()?;
            // The tenth byte may only carry bit 63: anything above overflows.
            if i == 9 && byte > 1 {
                return Err(WireError::Overflow);
            }
            value |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                // A final zero group after the first byte is a longer
                // encoding of a number that has a shorter one.
                if byte == 0 && i > 0 {
                    return Err(WireError::OverlongVarint);
                }
                return Ok(value);
            }
        }
        Err(WireError::Overflow)
    }

    /// Reads a varint that counts or measures things that each take at least
    /// `min_bytes_each` bytes, and checks that the bytes left can hold them.
    /// This is what keeps a hostile count from allocating anything.
    ///
    /// # Errors
    ///
    /// As [`Decoder::varint`], and [`WireError::TooLong`] if the count cannot
    /// fit in the bytes that remain.
    pub fn count(&mut self, min_bytes_each: usize) -> Result<usize, WireError> {
        let n = self.varint()?;
        let n = usize::try_from(n).map_err(|_| WireError::TooLong)?;
        match n.checked_mul(min_bytes_each.max(1)) {
            Some(needed) if needed <= self.remaining() => Ok(n),
            _ => Err(WireError::TooLong),
        }
    }
}

/// Decodes the payload of the command kind with wire tag `tag`. The payload
/// must be consumed exactly. Ranges are not checked here (an axis of `-32768`
/// decodes; `Sim::step` rejects it), so that an input source's mistakes are
/// reported by one place.
///
/// # Errors
///
/// [`WireError::UnknownTag`] or [`WireError::ReservedTag`] for a tag without a
/// variant, and any decoding error of the payload, including
/// [`WireError::TrailingBytes`].
pub fn decode_payload(tag: u32, payload: &[u8]) -> Result<CommandKind, WireError> {
    let mut d = Decoder::new(payload);
    let kind = match tag {
        TAG_CONTROLS => {
            let thrust = d.i16()?;
            let strafe = d.i16()?;
            let turn = d.i16()?;
            let aim = if d.flag()? { Some(d.u32()?) } else { None };
            let buttons = d.u32()?;
            CommandKind::Controls(ControlState {
                thrust,
                strafe,
                turn,
                aim,
                buttons,
            })
        }
        other if RESERVED_TAGS.iter().any(|&(t, _)| t == other) => {
            return Err(WireError::ReservedTag(other));
        }
        other => return Err(WireError::UnknownTag(other)),
    };
    d.finish()?;
    Ok(kind)
}

/// Decodes one command frame: `(pilot, seq_field, kind)`.
///
/// # Errors
///
/// Any [`WireError`]; a payload length above [`MAX_PAYLOAD_BYTES`] is
/// [`WireError::TooLong`] and one past the end of the input is
/// [`WireError::Truncated`].
pub fn decode_frame(d: &mut Decoder<'_>) -> Result<(u64, u64, CommandKind), WireError> {
    let pilot = d.varint()?;
    let seq_field = d.varint()?;
    let tag = u32::try_from(d.varint()?).map_err(|_| WireError::Overflow)?;
    let len = usize::try_from(d.varint()?).map_err(|_| WireError::TooLong)?;
    if len > MAX_PAYLOAD_BYTES {
        return Err(WireError::TooLong);
    }
    let payload = d.take(len)?;
    Ok((pilot, seq_field, decode_payload(tag, payload)?))
}

// ---- The simulation setup -------------------------------------------------

/// The fewest bytes one encoded ship takes: ids, pose, motion, four dynamics numbers,
/// the continuous-detection flag, the team, the hit points, the weapon flag and an
/// empty hull's count.
const SHIP_BYTES: usize = 8 + 8 + 3 * 8 + 3 * 8 + 4 * 8 + 1 + 4 + 8 + 1 + 1;
/// The fewest bytes one encoded hull part takes: the shape tag, a ball's radius, the
/// offset, the rotation and the density.
const PART_BYTES: usize = 1 + 8 + 3 * 8 + 4 * 8 + 8;
/// The fewest bytes one encoded pilot takes: its id and the option tag.
const PILOT_BYTES: usize = 8 + 1;
/// The bytes one encoded AI pilot takes: its id and its ship.
const AI_PILOT_BYTES: usize = 8 + 8;

fn put_vec3(out: &mut Vec<u8>, v: DVec3) -> Result<(), WireError> {
    put_f64(out, v.x)?;
    put_f64(out, v.y)?;
    put_f64(out, v.z)
}

fn get_vec3(d: &mut Decoder<'_>) -> Result<DVec3, WireError> {
    Ok(DVec3::new(d.f64()?, d.f64()?, d.f64()?))
}

fn put_part(out: &mut Vec<u8>, p: &HullPart) -> Result<(), WireError> {
    match p.shape {
        PartShape::Cuboid { half_extents } => {
            out.push(0);
            put_vec3(out, half_extents)?;
        }
        PartShape::Ball { radius } => {
            out.push(1);
            put_f64(out, radius)?;
        }
        PartShape::Cylinder {
            half_height,
            radius,
        } => {
            out.push(2);
            put_f64(out, half_height)?;
            put_f64(out, radius)?;
        }
    }
    put_vec3(out, p.offset)?;
    for q in p.rotation.to_array() {
        put_f64(out, q)?;
    }
    put_f64(out, p.density)
}

fn get_part(d: &mut Decoder<'_>) -> Result<HullPart, WireError> {
    let shape = match d.u8()? {
        0 => PartShape::Cuboid {
            half_extents: get_vec3(d)?,
        },
        1 => PartShape::Ball { radius: d.f64()? },
        2 => PartShape::Cylinder {
            half_height: d.f64()?,
            radius: d.f64()?,
        },
        _ => return Err(WireError::Invalid("unknown hull part shape")),
    };
    let offset = get_vec3(d)?;
    let rotation = DQuat::from_xyzw(d.f64()?, d.f64()?, d.f64()?, d.f64()?);
    Ok(HullPart {
        shape,
        offset,
        rotation,
        density: d.f64()?,
    })
}

/// Appends the encoding of a setup: the ships in order (each with its hull and
/// weapon), the pilots in order, the AI pilots in order, the attractor strength, the
/// maximum speed and the five AI parameters.
/// The layout belongs to `st-sim` and follows [`SIM_VERSION`](crate::SIM_VERSION).
///
/// # Errors
///
/// [`WireError::NonFinite`] if a number is NaN or infinite.
pub fn encode_setup(out: &mut Vec<u8>, setup: &Setup) -> Result<(), WireError> {
    put_varint(out, setup.ships.len() as u64);
    for s in &setup.ships {
        out.extend_from_slice(&s.id.to_le_bytes());
        out.extend_from_slice(&s.system.to_le_bytes());
        put_vec3(out, s.position)?;
        put_vec3(out, s.velocity)?;
        put_f64(out, s.heading)?;
        put_f64(out, s.max_accel)?;
        put_f64(out, s.max_turn_rate)?;
        put_f64(out, s.drag)?;
        out.push(u8::from(s.ccd));
        out.extend_from_slice(&s.team.to_le_bytes());
        put_f64(out, s.hit_points)?;
        match &s.weapon {
            Some(w) => {
                out.push(1);
                put_f64(out, w.range)?;
                put_f64(out, w.damage)?;
                put_f64(out, w.radius)?;
                out.extend_from_slice(&w.cooldown_ticks.to_le_bytes());
            }
            None => out.push(0),
        }
        put_varint(out, s.hull.len() as u64);
        for part in &s.hull {
            put_part(out, part)?;
        }
    }
    put_varint(out, setup.pilots.len() as u64);
    for &(id, controlled) in &setup.pilots {
        out.extend_from_slice(&id.0.to_le_bytes());
        match controlled {
            Some(ship) => {
                out.push(1);
                out.extend_from_slice(&ship.to_le_bytes());
            }
            None => out.push(0),
        }
    }
    put_varint(out, setup.ai_pilots.len() as u64);
    for &(id, ship) in &setup.ai_pilots {
        out.extend_from_slice(&id.0.to_le_bytes());
        out.extend_from_slice(&ship.to_le_bytes());
    }
    put_f64(out, setup.gravity_mu)?;
    put_f64(out, setup.max_speed_mps)?;
    put_f64(out, setup.ai.turn_gain)?;
    put_f64(out, setup.ai.fire_cone)?;
    put_f64(out, setup.ai.standoff)?;
    put_f64(out, setup.ai.unarmed_standoff)?;
    out.extend_from_slice(&setup.ai.drift_ticks.to_le_bytes());
    Ok(())
}

/// Decodes a setup written by [`encode_setup`]. The result is not validated
/// (`Sim::new` does that).
///
/// # Errors
///
/// Any [`WireError`].
pub fn decode_setup(d: &mut Decoder<'_>) -> Result<Setup, WireError> {
    let ship_count = d.count(SHIP_BYTES)?;
    let mut ships = Vec::with_capacity(ship_count);
    for _ in 0..ship_count {
        let id = d.u64()?;
        let system = d.u64()?;
        let position = get_vec3(d)?;
        let velocity = get_vec3(d)?;
        let heading = d.f64()?;
        let max_accel = d.f64()?;
        let max_turn_rate = d.f64()?;
        let drag = d.f64()?;
        let ccd = d.flag()?;
        let team = d.u32()?;
        let hit_points = d.f64()?;
        let weapon = if d.flag()? {
            Some(Weapon {
                range: d.f64()?,
                damage: d.f64()?,
                radius: d.f64()?,
                cooldown_ticks: d.u32()?,
            })
        } else {
            None
        };
        let part_count = d.count(PART_BYTES)?;
        let mut hull = Vec::with_capacity(part_count);
        for _ in 0..part_count {
            hull.push(get_part(d)?);
        }
        ships.push(ShipInit {
            id,
            system,
            position,
            velocity,
            heading,
            max_accel,
            max_turn_rate,
            drag,
            hull,
            ccd,
            team,
            hit_points,
            weapon,
        });
    }
    let pilot_count = d.count(PILOT_BYTES)?;
    let mut pilots = Vec::with_capacity(pilot_count);
    for _ in 0..pilot_count {
        let id = PilotId(d.u64()?);
        let controlled = if d.flag()? { Some(d.u64()?) } else { None };
        pilots.push((id, controlled));
    }
    let ai_count = d.count(AI_PILOT_BYTES)?;
    let mut ai_pilots = Vec::with_capacity(ai_count);
    for _ in 0..ai_count {
        ai_pilots.push((PilotId(d.u64()?), d.u64()?));
    }
    let gravity_mu = d.f64()?;
    let max_speed_mps = d.f64()?;
    let ai = AiParams {
        turn_gain: d.f64()?,
        fire_cone: d.f64()?,
        standoff: d.f64()?,
        unarmed_standoff: d.f64()?,
        drift_ticks: d.u64()?,
    };
    Ok(Setup {
        ships,
        pilots,
        ai_pilots,
        gravity_mu,
        max_speed_mps,
        ai,
    })
}

// ---- Golden bytes ---------------------------------------------------------

/// A fixed sample of every command kind in frames, followed by a fixed
/// setup, in this module's encoding.
///
/// `tools/determinism/sim-version.txt` pins the FNV-1a 64 hash of these
/// bytes (ADR 0009), so a change of the encoding cannot merge without a new
/// `SIM_VERSION`. The sample is: pilot 1 sets full thrust and full left
/// strafe (`seq` field 1); the same pilot then turns with an aim and buttons
/// (field 1, a difference of one); pilot 300 sends default controls with a
/// two-byte pilot and `seq` field 128; then the setup of one ship with a
/// three-part hull and a weapon, two pilots and one AI pilot.
///
/// ```
/// use st_sim::command::wire;
/// assert_eq!(wire::golden_bytes()[..4], [1, 1, 1, 11]);
/// ```
pub fn golden_bytes() -> Vec<u8> {
    let controls = |c| CommandKind::Controls(c);
    let mut out = Vec::new();
    encode_frame(
        &mut out,
        1,
        1,
        &controls(ControlState {
            thrust: 32767,
            strafe: -32767,
            turn: 0,
            aim: None,
            buttons: 0,
        }),
    );
    encode_frame(
        &mut out,
        1,
        1,
        &controls(ControlState {
            thrust: 0,
            strafe: 0,
            turn: 1,
            aim: Some(0x8000_0000),
            buttons: 0b101,
        }),
    );
    encode_frame(&mut out, 300, 128, &controls(ControlState::default()));
    let setup = Setup {
        ships: vec![ShipInit {
            id: 7,
            system: 2,
            position: DVec3::new(1.5, -0.0, 3.0),
            velocity: DVec3::new(0.25, 0.0, 0.0),
            heading: 0.5,
            max_accel: 25.0,
            max_turn_rate: 1.5,
            drag: 0.01,
            hull: vec![
                HullPart {
                    shape: PartShape::Cuboid {
                        half_extents: DVec3::new(1.0, 0.5, 2.0),
                    },
                    offset: DVec3::new(0.0, 0.0, 1.0),
                    rotation: DQuat::IDENTITY,
                    density: 100.0,
                },
                HullPart {
                    shape: PartShape::Ball { radius: 0.75 },
                    offset: DVec3::new(0.5, 0.0, -1.0),
                    rotation: DQuat::IDENTITY,
                    density: 50.0,
                },
                HullPart {
                    shape: PartShape::Cylinder {
                        half_height: 1.5,
                        radius: 0.25,
                    },
                    offset: DVec3::new(-0.5, 0.0, 0.0),
                    rotation: DQuat::from_xyzw(0.0, 0.0, 0.0, 1.0),
                    density: 25.0,
                },
            ],
            ccd: true,
            team: 3,
            hit_points: 200.0,
            weapon: Some(Weapon {
                range: 800.0,
                damage: 5.0,
                radius: 0.5,
                cooldown_ticks: 12,
            }),
        }],
        pilots: vec![(PilotId(1), Some(7)), (PilotId(2), None)],
        ai_pilots: vec![(PilotId(3), 7)],
        gravity_mu: 1.0e9,
        max_speed_mps: 20_000.0,
        ai: AiParams {
            turn_gain: 3.0,
            fire_cone: 0.05,
            standoff: 0.5,
            unarmed_standoff: 100.0,
            drift_ticks: 240,
        },
    };
    // The sample is finite, so encoding cannot fail; the unit test would
    // notice an empty tail.
    let _ = encode_setup(&mut out, &setup);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips_at_the_edges() {
        for value in [
            0,
            1,
            127,
            128,
            16_383,
            16_384,
            u64::from(u32::MAX),
            u64::MAX - 1,
            u64::MAX,
        ] {
            let mut out = Vec::new();
            put_varint(&mut out, value);
            assert_eq!(out.len(), varint_len(value), "{value}");
            let mut d = Decoder::new(&out);
            assert_eq!(d.varint(), Ok(value));
            assert_eq!(d.finish(), Ok(()));
        }
        let mut out = Vec::new();
        put_varint(&mut out, u64::MAX);
        assert_eq!(out.len(), 10);
        assert_eq!(*out.last().unwrap(), 1);
    }

    #[test]
    fn varint_rejects_overlong_and_overflow() {
        // Zero in two bytes, 1 in two bytes, 128 with a spare zero group.
        for bytes in [&[0x80, 0x00][..], &[0x81, 0x00], &[0x80, 0x81, 0x00]] {
            assert_eq!(
                Decoder::new(bytes).varint(),
                Err(WireError::OverlongVarint),
                "{bytes:?}"
            );
        }
        // Bit 64 set in the tenth byte, and an eleventh byte.
        let mut too_big = vec![0xff; 9];
        too_big.push(0x02);
        assert_eq!(Decoder::new(&too_big).varint(), Err(WireError::Overflow));
        let mut eleven = vec![0xff; 10];
        eleven.push(0x00);
        assert_eq!(Decoder::new(&eleven).varint(), Err(WireError::Overflow));
        // Truncated in the middle.
        assert_eq!(Decoder::new(&[0x80]).varint(), Err(WireError::Truncated));
        assert_eq!(Decoder::new(&[]).varint(), Err(WireError::Truncated));
    }

    #[test]
    fn controls_payload_is_11_or_15_bytes_and_round_trips() {
        let held = ControlState {
            thrust: -32767,
            strafe: 32767,
            turn: -1,
            aim: None,
            buttons: u32::MAX,
        };
        let aimed = ControlState {
            aim: Some(u32::MAX),
            ..held
        };
        for (c, want) in [(held, 11), (aimed, 15)] {
            let kind = CommandKind::Controls(c);
            let mut out = Vec::new();
            encode_payload(&mut out, &kind);
            assert_eq!(out.len(), want);
            assert_eq!(payload_len(&kind), want);
            assert_eq!(decode_payload(TAG_CONTROLS, &out), Ok(kind));
        }
    }

    #[test]
    fn an_out_of_range_axis_decodes_and_step_rejects_it() {
        let kind = CommandKind::Controls(ControlState {
            thrust: i16::MIN,
            ..ControlState::default()
        });
        let mut out = Vec::new();
        encode_payload(&mut out, &kind);
        assert_eq!(decode_payload(TAG_CONTROLS, &out), Ok(kind));
    }

    #[test]
    fn payload_errors() {
        let mut ok = Vec::new();
        encode_payload(
            &mut ok,
            &CommandKind::Controls(ControlState {
                aim: Some(5),
                ..ControlState::default()
            }),
        );
        // Truncated at every length.
        for n in 0..ok.len() {
            assert_eq!(
                decode_payload(TAG_CONTROLS, &ok[..n]),
                Err(WireError::Truncated)
            );
        }
        // Trailing byte.
        let mut long = ok.clone();
        long.push(0);
        assert_eq!(
            decode_payload(TAG_CONTROLS, &long),
            Err(WireError::TrailingBytes)
        );
        // The option tag byte is 6 bytes in.
        let mut bad_flag = ok.clone();
        bad_flag[6] = 2;
        assert_eq!(
            decode_payload(TAG_CONTROLS, &bad_flag),
            Err(WireError::BadFlag)
        );
        // Tags.
        assert_eq!(decode_payload(0, &ok), Err(WireError::UnknownTag(0)));
        assert_eq!(decode_payload(2, &ok), Err(WireError::UnknownTag(2)));
        for (tag, _) in RESERVED_TAGS {
            assert_eq!(decode_payload(tag, &ok), Err(WireError::ReservedTag(tag)));
        }
    }

    #[test]
    fn frame_errors() {
        // A payload length above the limit is refused before anything is read.
        let mut huge = Vec::new();
        put_varint(&mut huge, 1);
        put_varint(&mut huge, 1);
        put_varint(&mut huge, 1);
        put_varint(&mut huge, MAX_PAYLOAD_BYTES as u64 + 1);
        assert_eq!(
            decode_frame(&mut Decoder::new(&huge)),
            Err(WireError::TooLong)
        );
        // A length past the end of the input.
        let mut short = Vec::new();
        put_varint(&mut short, 1);
        put_varint(&mut short, 1);
        put_varint(&mut short, 1);
        put_varint(&mut short, 11);
        short.extend_from_slice(&[0; 5]);
        assert_eq!(
            decode_frame(&mut Decoder::new(&short)),
            Err(WireError::Truncated)
        );
        // A tag that does not fit a u32.
        let mut wide = Vec::new();
        put_varint(&mut wide, 1);
        put_varint(&mut wide, 1);
        put_varint(&mut wide, u64::from(u32::MAX) + 1);
        put_varint(&mut wide, 0);
        assert_eq!(
            decode_frame(&mut Decoder::new(&wide)),
            Err(WireError::Overflow)
        );
    }

    #[test]
    fn counts_are_bounded_by_the_bytes_left() {
        let mut d = Decoder::new(&[200, 1, 0, 0]);
        // 200 elements of at least 10 bytes cannot fit in 2 bytes.
        assert_eq!(d.count(10), Err(WireError::TooLong));
        let mut out = Vec::new();
        put_varint(&mut out, u64::MAX);
        assert_eq!(Decoder::new(&out).count(1), Err(WireError::TooLong));
    }

    #[test]
    fn setup_round_trips_bit_exactly_and_keeps_negative_zero() {
        let mut out = Vec::new();
        let setup = Setup {
            ships: vec![ShipInit {
                id: u64::MAX,
                system: 0,
                position: DVec3::new(-0.0, f64::MIN_POSITIVE / 2.0, f64::MAX),
                velocity: DVec3::ZERO,
                heading: -0.0,
                max_accel: 1.0,
                max_turn_rate: 1.0,
                drag: 0.0,
                ccd: false,
                team: u32::MAX,
                hit_points: f64::MAX,
                weapon: None,
                ..ShipInit::default()
            }],
            pilots: vec![(PilotId(3), None), (PilotId(1), Some(u64::MAX))],
            ai_pilots: vec![(PilotId(9), 5)],
            gravity_mu: 1.0,
            max_speed_mps: 2.5e4,
            ai: AiParams {
                turn_gain: 2.0,
                fire_cone: 0.1,
                standoff: 0.25,
                unarmed_standoff: 50.0,
                drift_ticks: u64::MAX,
            },
        };
        encode_setup(&mut out, &setup).unwrap();
        let mut d = Decoder::new(&out);
        let back = decode_setup(&mut d).unwrap();
        d.finish().unwrap();
        assert!(back.ships[0].position.x.is_sign_negative());
        assert!(back.ships[0].heading.is_sign_negative());
        assert_eq!(back, setup);
        let mut again = Vec::new();
        encode_setup(&mut again, &back).unwrap();
        assert_eq!(again, out);
    }

    #[test]
    fn setup_refuses_non_finite_numbers_both_ways() {
        let mut setup = Setup {
            ships: Vec::new(),
            pilots: Vec::new(),
            ai_pilots: Vec::new(),
            gravity_mu: f64::NAN,
            max_speed_mps: 1.0,
            ai: AiParams {
                turn_gain: 1.0,
                fire_cone: 1.0,
                standoff: 1.0,
                unarmed_standoff: 1.0,
                drift_ticks: 1,
            },
        };
        assert_eq!(
            encode_setup(&mut Vec::new(), &setup),
            Err(WireError::NonFinite)
        );
        setup.gravity_mu = 1.0;
        let mut out = Vec::new();
        encode_setup(&mut out, &setup).unwrap();
        // The tail is the six numbers (gravity, speed limit, four AI numbers) and the
        // drift ticks, eight bytes each.
        let n = out.len();
        for field in 0..6 {
            let mut bad = out.clone();
            let at = n - 56 + 8 * field;
            bad[at..at + 8].copy_from_slice(&f64::INFINITY.to_bits().to_le_bytes());
            assert_eq!(
                decode_setup(&mut Decoder::new(&bad)),
                Err(WireError::NonFinite),
                "field {field}"
            );
        }
        let mut bad = Setup {
            max_speed_mps: f64::NAN,
            ..setup.clone()
        };
        assert_eq!(
            encode_setup(&mut Vec::new(), &bad),
            Err(WireError::NonFinite)
        );
        bad = Setup {
            ai: AiParams {
                fire_cone: f64::NAN,
                ..setup.ai
            },
            ..setup
        };
        assert_eq!(
            encode_setup(&mut Vec::new(), &bad),
            Err(WireError::NonFinite)
        );
    }

    /// The format itself, byte for byte. A change here is a change of the
    /// wire format: it needs a new `SIM_VERSION` and a line in
    /// `tools/determinism/sim-version.txt` (ADR 0009).
    #[test]
    fn golden_frames() {
        let bytes = golden_bytes();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let frames: String = [
            // pilot 1, field 1, tag 1, len 11: thrust, strafe, turn, no aim, buttons
            "0101010b ff7f 0180 0000 00 00000000",
            // pilot 1, field 1, tag 1, len 15: turn 1, aim 0x80000000, buttons 5
            "0101010f 0000 0000 0100 01 00000080 05000000",
            // pilot 300 (ac 02), field 128 (80 01), tag 1, len 11, default controls
            "ac02 8001 01 0b 0000 0000 0000 00 00000000",
        ]
        .concat()
        .replace(' ', "");
        assert!(hex.starts_with(&frames), "{hex}");
        // The tail is the setup: ship count, one ship of 382 bytes (96, the flag, team, hit points,
        // a 29-byte weapon, a hull count and parts of 89, 73 and 81 bytes), pilot count,
        // two pilots of 17 and 9 bytes, the AI pilot count, one AI pilot of 16 bytes and
        // the attractor strength, the speed limit, four AI numbers and the AI drift ticks (8
        // bytes each).
        let tail = &bytes[frames.len() / 2..];
        assert_eq!(
            tail.len(),
            1 + 382 + 1 + 17 + 9 + 1 + 16 + 8 + 8 + 4 * 8 + 8
        );
        assert_eq!(tail[0], 1);
        assert!(tail.ends_with(&240_u64.to_le_bytes()));
        let mu_at = tail.len() - 6 * 8 - 8;
        assert_eq!(tail[mu_at..mu_at + 8], 1.0e9_f64.to_bits().to_le_bytes());
    }
}
