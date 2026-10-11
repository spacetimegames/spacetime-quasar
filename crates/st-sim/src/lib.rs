//! Headless game simulation.
//!
//! Purpose: the deterministic simulation. It takes a world seed and a per-tick
//! command log as its only inputs (ADR 0008, ADR 0009) and advances exactly one
//! fixed tick of [`DT`](st_time::DT) per [`Sim::step`]. Pilots send
//! [`Command`]s that steer ships; a pilot is a pilot whether a person, a
//! replay or a test script feeds it.
//!
//! This is a minimal version: a few bodies integrated in a plane, one command
//! kind ([`CommandKind::Controls`]), the canonical state hash
//! ([`StateHasher`], format version 1) and a handful of
//! [`scenarios`] for the determinism harness. It uses only
//! [`st_math::det`] for transcendental math and [`st_rng`] for randomness, and
//! it reads no clock.
//!
//! A command log is a file too (ADR 0009). [`command::wire`] is the command
//! codec (varints, the tag table with the reserved tags, golden bytes),
//! [`replay`] writes and reads `.stqreplay` files and plays them back against
//! their checkpoint hashes, and [`SIM_VERSION`] names the simulation a replay
//! was recorded by: a replay plays only in a build with the same version
//! (PD-33). [`Sim::step`] enforces the ADR 0009 limits (1,024 commands per
//! tick, 1 MiB per payload, 2 MiB per tick).
//!
//! Boundary: no GPU, no input devices, no wall clock, no files. The replay
//! code works on byte slices and `Vec<u8>`; the caller stores them (through
//! `st-platform`, or `std::fs` in a tool). A sim crate under ADR 0008, so it
//! carries no clippy opt-out.
//!
//! Non-goals (not yet built). From ADR 0009: session commands (`AddPilot`,
//! `RemovePilot`, `SetController`, `DataSetChange`, `Narrative`; their tags
//! are reserved and decode as an error), AI pilots (so no pilot is controlled
//! inside the simulation yet, and `StepError::NotExternal` cannot occur in a
//! real run), persisted snapshots, and the replay viewer and seek cache
//! (`st-game`, SPAC-121). Also: Rapier physics, weapons, AI, economy.
//!
//! # Example
//!
//! ```
//! use st_sim::{Command, CommandKind, ControlState, PilotId, Sim};
//! use st_sim::scenarios;
//!
//! let scenario = scenarios::solo_burn();
//! let mut sim = Sim::new(&scenario.setup, scenario.seed)?;
//! let before = sim.state_hash()?;
//!
//! let burn = Command {
//!     tick: sim.tick(),
//!     pilot: PilotId(1),
//!     seq: 1,
//!     kind: CommandKind::Controls(ControlState { thrust: 32767, ..ControlState::default() }),
//! };
//! sim.step(&[burn])?;
//! assert_eq!(sim.tick(), 1);
//! assert_ne!(sim.state_hash()?, before);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod command;
mod hash;
pub mod replay;
pub mod scenarios;
mod sim;

pub use command::{Command, CommandKind, ControlState, PilotId, StepError};
pub use hash::{DumpLine, HASH_FORMAT_VERSION, HashError, StateHash, StateHasher};
pub use sim::{CHECKPOINT_INTERVAL, Setup, SetupError, ShipInit, Sim};

/// The simulation version (ADR 0009, "Version policy"). A replay recorded by
/// one version is refused by every other, because the simulation is
/// version-locked to its recordings (PD-33).
///
/// A change bumps it, with a new line in `tools/determinism/sim-version.txt`,
/// when it changes a pinned reference line, a command kind, its payload or
/// encoding, the setup encoding, the `replay` container version, the `st-rng`
/// algorithm, or the version of a simulation dependency in `Cargo.lock`.
/// It is not part of the state hash.
///
/// ```
/// assert!(st_sim::SIM_VERSION >= 1);
/// ```
pub const SIM_VERSION: u32 = 1;
