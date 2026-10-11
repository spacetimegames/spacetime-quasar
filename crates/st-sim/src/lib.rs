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
//! Boundary: no GPU, no input devices, no wall clock, no files. A sim crate
//! under ADR 0008, so it carries no clippy opt-out.
//!
//! Non-goals (not yet built). From ADR 0009: replay files (`st_sim::replay`),
//! the command wire encoding and its tag table with the reserved tags,
//! `SIM_VERSION`, session commands, the `StepError` variants `NotExternal` and
//! `TickTooLarge`, and the payload (1 MiB) and per-tick (2 MiB) byte limits.
//! Also: Rapier physics, weapons, AI, economy.
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
pub mod scenarios;
mod sim;

pub use command::{Command, CommandKind, ControlState, PilotId, StepError};
pub use hash::{DumpLine, HASH_FORMAT_VERSION, HashError, StateHash, StateHasher};
pub use sim::{CHECKPOINT_INTERVAL, Setup, SetupError, ShipInit, Sim};
