//! Game glue: input mapping, camera rigs, session loop and content wiring.
//!
//! Planned scope: Game glue: input mapping, camera rigs, the session loop, save and load, menus and content wiring.
//!
//! Boundary: this crate only connects lower crates. Today it holds one piece
//! of content wiring, [`HotData`], which feeds `st-platform`'s file watcher
//! into `st-data`'s loader (cargo feature `hot-reload`, on by default,
//! native targets only). The rest is a stub.
//!
//! Non-goals: Does not reimplement what the lower crates provide; it only connects them.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "exempt from PD-06: ADR 0008"
)]

#[cfg(all(feature = "hot-reload", not(target_arch = "wasm32")))]
mod hot_data;

#[cfg(all(feature = "hot-reload", not(target_arch = "wasm32")))]
pub use hot_data::{DataSnapshot, HotData, ReloadError, ReloadEvent, Reloaded};
