//! Game glue: input mapping, camera rigs, session loop and content wiring.
//!
//! Planned scope: Game glue: input mapping, camera rigs, the session loop, save and load, menus and content wiring.
//!
//! Boundary: this crate is a stub; it has no public API yet.
//!
//! Non-goals: Does not reimplement what the lower crates provide; it only connects them.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "exempt from PD-06: ADR 0008"
)]
