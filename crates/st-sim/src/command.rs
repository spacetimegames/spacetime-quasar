//! Commands: the only way anything outside `st-sim` changes simulation state.
//!
//! The types follow ADR 0009 ("Commands"). The wire encoding and replay files
//! of that ADR are not built yet, so a command log here is a `Vec<Command>`.

use crate::hash::{HashError, StateHash, StateHasher};

/// Largest number of commands one tick may carry (ADR 0009).
pub const MAX_COMMANDS_PER_TICK: usize = 1024;

/// A pilot. 0 is reserved for the session; real pilots start at 1.
///
/// A pilot is the same kind of thing whoever feeds it: nothing in the
/// simulation singles one out as "the player".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PilotId(pub u64);

impl PilotId {
    /// The session pilot, which is not a controller of ships.
    pub const SESSION: PilotId = PilotId(0);
}

/// Held control state of one pilot, quantised at the source (ADR 0009,
/// "Values"). Axes are in `-32767..=32767`; `-32768` is invalid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ControlState {
    /// Positive forward, negative brake.
    pub thrust: i16,
    /// Positive to the right of the heading.
    pub strafe: i16,
    /// Positive turns counter-clockwise (toward +y from +x).
    pub turn: i16,
    /// Combat-mode heading as a binary angle (2^32 units per turn), if any.
    pub aim: Option<u32>,
    /// Held buttons as a bit set.
    pub buttons: u32,
}

impl ControlState {
    /// Whether every axis is in range.
    pub(crate) fn is_valid(&self) -> bool {
        self.thrust != i16::MIN && self.strafe != i16::MIN && self.turn != i16::MIN
    }
}

/// Wire tag of `ControlState` as a pilot component, and of
/// [`CommandKind::Controls`] (ADR 0009: tag 1).
pub(crate) const CONTROLS_COMPONENT_TAG: u32 = 1;

impl StateHash for ControlState {
    fn state_hash(&self, h: &mut StateHasher) -> Result<(), HashError> {
        h.i16(self.thrust);
        h.i16(self.strafe);
        h.i16(self.turn);
        h.option_tag(self.aim.is_some());
        if let Some(aim) = self.aim {
            h.u32(aim);
        }
        h.u32(self.buttons);
        Ok(())
    }
}

/// What a command does.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum CommandKind {
    /// Tag 1. Held control state, sent when it changes (sample and hold).
    Controls(ControlState),
}

/// One input to the simulation for one pilot at one tick.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    /// The tick this command applies in: `state(tick + 1) = step(state(tick), ..)`.
    pub tick: u64,
    /// The pilot that sends it.
    pub pilot: PilotId,
    /// Strictly increasing per pilot over the whole session, first value 1;
    /// gaps are allowed.
    pub seq: u64,
    /// What the command does.
    pub kind: CommandKind,
}

/// Sorts commands into the order `Sim::step` requires: ascending by
/// (pilot, sequence number). The sort is stable.
///
/// ```
/// use st_sim::{Command, CommandKind, ControlState, PilotId, command};
///
/// let c = |pilot, seq| Command {
///     tick: 0,
///     pilot: PilotId(pilot),
///     seq,
///     kind: CommandKind::Controls(ControlState::default()),
/// };
/// let mut log = vec![c(2, 1), c(1, 2), c(1, 1)];
/// command::sort(&mut log);
/// let order: Vec<_> = log.iter().map(|c| (c.pilot.0, c.seq)).collect();
/// assert_eq!(order, [(1, 1), (1, 2), (2, 1)]);
/// ```
pub fn sort(commands: &mut [Command]) {
    commands.sort_by_key(|c| (c.pilot, c.seq));
}

/// Why `Sim::step` rejected a command slice. State is unchanged. Variants
/// that name one command carry its index in the slice; the caller drops that
/// command and steps again.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepError {
    /// The command's tick is not the simulation's current tick.
    WrongTick {
        /// Index of the offending command.
        index: usize,
    },
    /// The slice is not strictly ascending by (pilot, sequence number).
    NotSorted {
        /// Index of the first command that breaks the order.
        index: usize,
    },
    /// The pilot does not exist.
    UnknownPilot {
        /// Index of the offending command.
        index: usize,
    },
    /// The sequence number is not above the pilot's last applied one.
    StaleSequence {
        /// Index of the offending command.
        index: usize,
    },
    /// A session kind from a pilot, or a pilot kind from the session.
    WrongSource {
        /// Index of the offending command.
        index: usize,
    },
    /// The payload is out of range (for example an axis of `-32768`).
    InvalidPayload {
        /// Index of the offending command.
        index: usize,
    },
    /// More than [`MAX_COMMANDS_PER_TICK`] commands in one tick.
    TooManyCommands {
        /// How many were passed.
        count: usize,
    },
    /// The floating-point environment is not round-to-nearest-even without
    /// flush-to-zero (`st_math::det::fp_env_is_default()` is false), so the
    /// step would not be reproducible. Nothing was changed (ADR 0008).
    FloatEnvironment,
}

impl core::fmt::Display for StepError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StepError::WrongTick { index } => write!(f, "command {index} has the wrong tick"),
            StepError::NotSorted { index } => write!(f, "command {index} breaks the order"),
            StepError::UnknownPilot { index } => write!(f, "command {index}: unknown pilot"),
            StepError::StaleSequence { index } => {
                write!(
                    f,
                    "command {index}: sequence number not above the last applied"
                )
            }
            StepError::WrongSource { index } => {
                write!(f, "command {index}: kind not valid from this pilot")
            }
            StepError::InvalidPayload { index } => write!(f, "command {index}: invalid payload"),
            StepError::FloatEnvironment => {
                f.write_str("the floating-point environment is not the default")
            }
            StepError::TooManyCommands { count } => {
                write!(
                    f,
                    "{count} commands in one tick (limit {MAX_COMMANDS_PER_TICK})"
                )
            }
        }
    }
}

impl core::error::Error for StepError {}
