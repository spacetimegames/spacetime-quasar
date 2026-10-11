//! Deterministic scalar math and the large-world render origin.
//!
//! This crate provides deterministic math for simulation code and the one place where
//! simulation positions become render positions.
//!
//! # Contents
//!
//! * [`det`]: scalar math over a software libm. It is the only door to transcendental
//!   functions (`sin`, `exp`, `powf`, ...) in simulation crates, plus fully specified
//!   `min`, `max`, `clamp`, `powi` and degree conversions, and a floating-point
//!   environment check.
//! * [`RenderOrigin`]: the camera-relative render origin of the large-world scheme. The
//!   simulation works in `f64` metres in a system frame; the renderer works in `f32`
//!   metres relative to a grid-aligned origin near the camera.
//! * [`glam`], re-exported unchanged, so that a consumer can name the exact glam version
//!   this crate was built with. There are no wrapper types around vectors, quaternions or
//!   matrices.
//!
//! Non-goals: orbit (Kepler) and interpolation math are planned in a separate Subtask and
//! are not provided yet.
//!
//! Operations that can fail on bad input return `Option` or `Result` or a documented
//! fallback; nothing here panics on misuse.
//!
//! # Example
//!
//! ```
//! use st_math::{det, glam::DVec3, RenderOrigin};
//!
//! let camera = DVec3::new(1_000_000.0, 0.0, -250_000.0);
//! let origin = RenderOrigin::new(camera);
//! let ship = camera + DVec3::new(10.0, 2.0, -3.0);
//! let on_screen = origin.to_render(ship);
//! assert!((on_screen - origin.to_render(camera)).length() < 11.0);
//! assert!(det::fp_env_is_default());
//! ```
//!
//! # Non-goals
//!
//! Not a physics engine, not GPU-bound math, and no game-specific types. Orbit and Kepler
//! helpers and interpolation are planned for this crate but are not provided yet.

pub use glam;

pub mod det;
mod render_origin;

pub use render_origin::RenderOrigin;
