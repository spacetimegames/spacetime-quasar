//! Headless game simulation.
//!
//! Purpose: the deterministic simulation. It takes a world seed and a per-tick
//! command log as its only inputs (ADR 0008, ADR 0009) and advances exactly one
//! fixed tick of [`DT`](st_time::DT) per [`Sim::step`]. Pilots send
//! [`Command`]s that steer ships; a pilot is a pilot whether a person, a
//! replay or a test script feeds it, and AI pilots fly from inside the
//! simulation.
//!
//! # Physics
//!
//! Ships are Rapier rigid bodies (`rapier3d-f64`, ADR 0007), one physics world per
//! star system, stepped in ascending system id. No Rapier or Parry type appears in a public
//! signature of this crate. The rules of ADR 0008 hold:
//!
//! * a ship is one body with one compound collider built from its [`HullPart`]s, at the
//!   body origin, with explicit mass properties that `st-sim` computes itself (closed-form
//!   part inertia, parallel-axis sums, the Jacobi solver of [`st_math::eigen`]), so Rapier and
//!   Parry never diagonalise an inertia tensor;
//! * the planar constraint is the `LockedAxes` of ADR 0007 (free motion in the XZ plane and
//!   yaw about +Y), with gyroscopic forces off so the hull stays in the plane, ships that never
//!   sleep, and continuous collision detection per ship;
//! * the integration step is `DT`, the body velocity limit is `Setup::max_speed_mps`, which the
//!   setup builder derives from data (Rapier's default of 400 m/s would cap normal flight), and
//!   nothing runs in parallel;
//! * thrust, strafe, gravity and drag act as impulses and velocities, turning is a rate
//!   command, and weapons are Parry shape casts through the broad phase.
//!
//! The state hash reads each body through Rapier's API (ADR 0008, "State hash") and leaves
//! out Rapier's caches. [`Sim`] is `Clone`: a clone copies every world and continues bit for
//! bit.
//!
//! # Rules of the game, as far as they exist
//!
//! **Provisional determinism fixture (SPAC-128), not the PD-28 combat rules.** Phase 2
//! replaces it with the PD-28 rules (beam and bolt prototypes, shields, armour facings,
//! sections); that change bumps [`SIM_VERSION`]. The hit points, weapons, teams and AI pilots
//! below exist so that the determinism harness exercises damage, removal and AI inside the
//! step. They live in one private module (`sim/combat.rs`) that Phase 2 replaces, and their
//! tuning numbers come from the [`Setup`] ([`AiParams`], [`Weapon`]), not from the step.
//!
//! One command kind ([`CommandKind::Controls`]); thrust, strafe, turn and a fire button;
//! central gravity; ships with hit points and an optional [`Weapon`] that sweeps a ball along
//! the heading; AI pilots that chase the nearest enemy of another team; destroyed ships leave
//! their world. The canonical state hash ([`StateHasher`], format version 1) and the
//! [`scenarios`] of the determinism harness complete the crate. It uses only
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
//! are reserved and decode as an error), persisted snapshots, and the replay
//! viewer and seek cache (`st-game`, SPAC-121). Also: joints, multibodies, soft bodies and the
//! other Rapier features ADR 0008 keeps out, section-level damage, the economy, factions and
//! missions, and loading ships from data (the pinned scenarios define theirs in code). Also:
//! no `assets/` data, `st-game` or `apps/` code uses [`Weapon`], hit points, teams or the AI
//! as game content before Phase 2.
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
mod hull;
mod physics;
pub mod replay;
pub mod scenarios;
mod sim;

pub use command::{Command, CommandKind, ControlState, PilotId, StepError};
pub use hash::{DumpLine, HASH_FORMAT_VERSION, HashError, StateHash, StateHasher};
pub use hull::{FIRE, HullPart, MAX_HULL_PARTS, PartShape, Weapon};
pub use sim::{
    AiParams, CHECKPOINT_INTERVAL, MAX_SHIPS_PER_SYSTEM, Setup, SetupError, ShipInit, Sim,
};

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
pub const SIM_VERSION: u32 = 2;
