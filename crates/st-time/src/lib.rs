//! Deterministic fixed-step time, frame budgets and profiling.
//!
//! `st-time` turns *elapsed real time that the caller supplies* into a whole
//! number of fixed simulation ticks, and keeps simple statistics about how long
//! the caller's frames and passes took. It never reads a clock itself: there is
//! no `Instant::now` and no `SystemTime::now` in this crate, so everything it
//! does is a pure function of the numbers passed in. Measuring time is the job
//! of the platform layer or the application; this crate only does arithmetic
//! on what they measured.
//!
//! # Pieces
//!
//! * [`Nanos`] is an integer duration in nanoseconds. [`TICK_RATE_HZ`] and
//!   [`DT`] define the default 120 Hz simulation rate.
//! * [`FixedStep`] is the accumulator. Each frame the caller passes the real
//!   time elapsed since the previous frame, and [`FixedStep::advance`] returns
//!   an [`Advance`]: how many ticks to run, and how far the next tick is
//!   through (the render interpolation fraction). It clamps the number of
//!   ticks per frame so that a long stall cannot cause a "spiral of death".
//! * [`SampleWindow`] keeps the last `N` duration samples and reports mean,
//!   minimum, maximum and percentiles ([`SampleWindow::p95`],
//!   [`SampleWindow::p99`]) without allocating.
//! * [`FrameBudget`] groups a [`SampleWindow`] per phase (for example "sim",
//!   "cull", "draw") plus one for the whole frame, and counts frames over a
//!   budget. Callers feed it durations they measured.
//!
//! # Determinism
//!
//! The accumulator is integer-only. It stores time as `nanoseconds * rate`, so
//! one tick is exactly `1_000_000_000` units and a tick period that is not a
//! whole number of nanoseconds (1/120 s is 8 333 333.33... ns) does not
//! accumulate rounding error. Feeding the same sequence of elapsed times always
//! produces the same sequence of tick counts, on every target.
//!
//! The only floating-point values this crate produces are conveniences for
//! presentation: [`Advance::alpha`], [`Nanos::as_secs_f64`] and the [`DT`]
//! constant (which the ADR defines as a `const`). They are **render and
//! display only**. The simulation state must never depend on the value of
//! [`Advance::alpha`]; use [`Advance::ticks`] to drive the simulation.
//!
//! # Example
//!
//! ```
//! use st_time::{FixedStep, Nanos};
//!
//! let mut clock = FixedStep::default(); // 120 Hz, at most 8 ticks per frame
//!
//! // The caller measured 20 ms since the last frame (a 50 Hz display).
//! let advance = clock.advance(Nanos::from_millis(20));
//! assert_eq!(advance.ticks(), 2); // 20 ms is 2.4 ticks at 120 Hz
//! assert!(advance.alpha() > 0.3 && advance.alpha() < 0.5);
//! // ...the caller now calls its simulation step `advance.ticks()` times and
//! // renders, blending the last two states by `advance.alpha()`.
//! ```

mod budget;
mod nanos;
mod samples;
mod step;

pub use budget::{FrameBudget, FrameReport};
pub use nanos::{DT, Nanos, TICK_RATE_HZ, Tick, ticks_to_nanos};
pub use samples::{SampleWindow, Summary};
pub use step::{Advance, ConfigError, FixedStep, StepConfig};
