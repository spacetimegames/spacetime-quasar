//! Deterministic math for the simulation: scalar functions, Kepler orbits, interpolation and
//! the large-world render origin.
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
//! * [`eigen`]: eigen-decomposition of symmetric 3 x 3 matrices (Jacobi, fixed schedule), used for
//!   the principal axes of ship inertia tensors (ADR 0008).
//! * [`orbit`]: deterministic Kepler orbits. Elements and state vectors convert both ways, and
//!   an orbit is advanced in time with a Kepler-equation solver that runs a fixed number of
//!   iterations. Elliptic orbits only; bad input is an error value, never a panic.
//! * [`interp`]: `f64` interpolation for the values the simulation hands to the renderer:
//!   scalar and vector lerp, quaternion nlerp and slerp, and cubic Hermite curves from
//!   position and velocity.
//! * [`glam`], re-exported unchanged, so that a consumer can name the exact glam version
//!   this crate was built with. There are no wrapper types around vectors, quaternions or
//!   matrices.
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
//! Not a physics engine, not GPU-bound math, and no game-specific types. Hyperbolic and
//! parabolic orbits, orbital perturbations and moving orbit centres are not provided.

pub use glam;

pub mod det;
pub mod eigen;
pub mod interp;
pub mod orbit;
mod render_origin;

pub use render_origin::RenderOrigin;
