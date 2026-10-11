//! The Phase 1 CLI demo: edit a `.ron` value, see the new figures and a new
//! final state hash.
//!
//! Purpose: PD-31 asks the owner to feel the speed and scale numbers before a
//! ship exists. This crate loads the envelope from `assets/space_envelope`,
//! validates it with `st-data`, prints it together with figures a person can
//! picture (km/h, minutes, "times a jet airliner"), runs a deterministic
//! `st-sim` replay built from the same data and prints its final state hash.
//! `st-game`'s `HotData` reloads the file when it is saved; a bad edit is
//! reported with the file, line and column and the old values stay in use.
//!
//! Boundary: the library is deterministic, I/O-free and compiles for every
//! target ([`envelope`], [`figures`], [`scenario`]); [`Session`] (native only)
//! wires in the file watcher and returns text. The binary reads the clock,
//! sleeps and prints, and is the only exempt code (ADR 0008).
//!
//! Non-goals: no window, no game loop, no replay files (ADR 0009 is not built
//! yet), no `q` key: reading the keyboard without blocking needs a thread, and
//! threads go through `st-platform`, which has no such facility yet.
//!
//! # Example
//!
//! ```
//! use phase1_demo::{SpaceEnvelope, figures, scenario};
//! use st_data::Loader;
//!
//! let text = br#"(kind: "space_envelope", version: 1, data: (
//!     normal_speed_mps: 1000.0, cruise_multiplier: 10.0, system_radius_km: 2000.0,
//!     body_spacing_km: (min: 60.0, max: 120.0), acceleration_mps2: 100.0,
//!     replay_seed: 31, reference_speeds: [], reference_lengths: []))"#;
//! let env = Loader::new().load::<SpaceEnvelope>("pd-31.ron", text).unwrap().value;
//! assert!(figures::feel(&env).contains("at normal speed: to the next body 1 min 00 s to 2 min 00 s"));
//! let replay = scenario::replay(&env)?;
//! assert_eq!(replay.hash, scenario::replay(&env)?.hash);
//! # Ok::<(), phase1_demo::ReplayError>(())
//! ```

pub mod envelope;
pub mod figures;
pub mod scenario;
#[cfg(not(target_arch = "wasm32"))]
mod session;

pub use envelope::{ReferenceLength, ReferenceSpeed, SpaceEnvelope, SpacingKm, VALIDITY_BOUND_M};
pub use scenario::{Replay, ReplayError};
#[cfg(not(target_arch = "wasm32"))]
pub use session::{REMINDER_EVERY, Session};
