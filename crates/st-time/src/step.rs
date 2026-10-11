//! The fixed-step accumulator.

use core::fmt;

use crate::{Nanos, TICK_RATE_HZ};

/// Nanoseconds per second. The accumulator counts in units of
/// `1 / (NANOS_PER_SEC * rate)` seconds, so one tick is `NANOS_PER_SEC` units
/// for any rate and no tick length is ever rounded.
const NANOS_PER_SEC: u64 = 1_000_000_000;

/// The default clamp on ticks per frame: 8 ticks is about 67 ms at 120 Hz.
const DEFAULT_MAX_TICKS: u32 = 8;

/// Why a [`StepConfig`] could not be built.
///
/// ```
/// use st_time::{ConfigError, StepConfig};
///
/// assert_eq!(StepConfig::new(0, 8), Err(ConfigError::ZeroRate));
/// assert_eq!(StepConfig::new(120, 0), Err(ConfigError::ZeroMaxTicks));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// The tick rate was zero.
    ZeroRate,
    /// The rate exceeds one tick per nanosecond (1 GHz).
    RateTooHigh,
    /// The per-frame tick clamp was zero, which would stop the simulation.
    ZeroMaxTicks,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ConfigError::ZeroRate => "tick rate must be at least 1 Hz",
            ConfigError::RateTooHigh => "tick rate must be at most 1 GHz",
            ConfigError::ZeroMaxTicks => "max ticks per frame must be at least 1",
        })
    }
}

impl std::error::Error for ConfigError {}

/// Settings for a [`FixedStep`]: the tick rate and the spiral-of-death clamp.
///
/// ```
/// use st_time::StepConfig;
///
/// let cfg = StepConfig::new(60, 4)?;
/// assert_eq!(cfg.rate_hz(), 60);
/// assert_eq!(cfg.max_ticks_per_frame(), 4);
/// assert_eq!(StepConfig::default().rate_hz(), 120);
/// # Ok::<(), st_time::ConfigError>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepConfig {
    rate_hz: u32,
    max_ticks: u32,
}

impl StepConfig {
    /// Builds a configuration.
    ///
    /// * `rate_hz` is the simulation rate, from 1 to 1 000 000 000.
    /// * `max_ticks_per_frame` is the *spiral-of-death clamp*: the most ticks
    ///   one call to [`FixedStep::advance`] will ask for, at least 1. If a
    ///   frame takes longer than the simulation can catch up with (each tick
    ///   costing more than its real-time length), unclamped catch-up makes every
    ///   frame longer than the last. Once the clamp is hit, the remaining
    ///   backlog is dropped and reported in [`Advance::dropped_ticks`], so the
    ///   game slows down instead of freezing. Pick a value so that
    ///   `max_ticks_per_frame / rate_hz` is the longest real-time stall you
    ///   still want to simulate in full.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] for a zero or too-high rate or a zero clamp.
    pub const fn new(rate_hz: u32, max_ticks_per_frame: u32) -> Result<StepConfig, ConfigError> {
        if rate_hz == 0 {
            return Err(ConfigError::ZeroRate);
        }
        if rate_hz as u64 > NANOS_PER_SEC {
            return Err(ConfigError::RateTooHigh);
        }
        if max_ticks_per_frame == 0 {
            return Err(ConfigError::ZeroMaxTicks);
        }
        Ok(StepConfig {
            rate_hz,
            max_ticks: max_ticks_per_frame,
        })
    }

    /// The simulation rate in hertz.
    pub const fn rate_hz(&self) -> u32 {
        self.rate_hz
    }

    /// The most ticks one frame may run.
    pub const fn max_ticks_per_frame(&self) -> u32 {
        self.max_ticks
    }
}

impl Default for StepConfig {
    /// 120 Hz ([`TICK_RATE_HZ`](crate::TICK_RATE_HZ)), at most 8 ticks per frame.
    fn default() -> StepConfig {
        StepConfig {
            rate_hz: TICK_RATE_HZ,
            max_ticks: DEFAULT_MAX_TICKS,
        }
    }
}

/// The result of one [`FixedStep::advance`] call.
///
/// ```
/// use st_time::{FixedStep, Nanos};
///
/// let mut clock = FixedStep::default();
/// let a = clock.advance(Nanos::from_millis(10));
/// assert_eq!(a.ticks(), 1);
/// assert_eq!(a.dropped_ticks(), 0);
/// assert!(!a.was_clamped());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Advance {
    ticks: u32,
    dropped_ticks: u64,
    // Fraction of the next tick already elapsed, as `alpha_num / alpha_den`.
    alpha_num: u64,
    alpha_den: u64,
}

impl Advance {
    /// How many simulation ticks to run now. Never exceeds the configured
    /// [`StepConfig::max_ticks_per_frame`].
    pub const fn ticks(&self) -> u32 {
        self.ticks
    }

    /// Whole ticks of backlog discarded by the spiral-of-death clamp this call.
    /// Zero in normal operation.
    pub const fn dropped_ticks(&self) -> u64 {
        self.dropped_ticks
    }

    /// Whether the clamp discarded any backlog this call.
    pub const fn was_clamped(&self) -> bool {
        self.dropped_ticks != 0
    }

    /// The render interpolation fraction as an exact ratio `(numerator,
    /// denominator)`, with `numerator < denominator`.
    ///
    /// It says how far real time has progressed into the *next* tick, after the
    /// ticks in [`Advance::ticks`] have run.
    pub const fn alpha_ratio(&self) -> (u64, u64) {
        (self.alpha_num, self.alpha_den)
    }

    /// The render interpolation fraction in `[0, 1)`, as a `f64`.
    ///
    /// Blend the previous and current simulation states by this value when
    /// drawing, so motion stays smooth when the display rate is not a multiple
    /// of the tick rate.
    ///
    /// **Render-only.** This is a float derived at the edge for the renderer.
    /// It is outside the deterministic state: never use it in simulation logic,
    /// hashes, replays or commands.
    ///
    /// ```
    /// use st_time::{FixedStep, Nanos};
    ///
    /// let mut clock = FixedStep::default();
    /// // Half a tick (1/240 s) has elapsed and no tick is due yet.
    /// let a = clock.advance(Nanos::new(4_166_666));
    /// assert_eq!(a.ticks(), 0);
    /// assert!((a.alpha() - 0.5).abs() < 1e-6);
    /// ```
    pub fn alpha(&self) -> f64 {
        self.alpha_num as f64 / self.alpha_den as f64
    }
}

/// A fixed-step time accumulator.
///
/// The caller measures how much real time passed since the previous frame (with
/// whatever clock it owns) and passes it to [`FixedStep::advance`]. The clock
/// answers how many fixed ticks the simulation owes, and keeps the leftover
/// fraction of a tick for the next frame. Rendering is decoupled from the
/// simulation rate: a 144 Hz display sometimes runs zero ticks in a frame and
/// sometimes one, and the interpolation fraction hides the difference.
///
/// This type holds no tick counter. The simulation owns its own tick index
/// (ADR 0008); call `Sim::step` once per tick returned.
///
/// ```
/// use st_time::{FixedStep, Nanos};
///
/// let mut clock = FixedStep::default();
/// // One second of real time delivered in 1 ms slices is exactly 120 ticks,
/// // however the slices fall: no drift.
/// let total: u32 = (0..1000).map(|_| clock.advance(Nanos::from_millis(1)).ticks()).sum();
/// assert_eq!(total, 120);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedStep {
    cfg: StepConfig,
    // Accumulated time in units of 1/(1e9 * rate) s. Invariant between calls:
    // `acc < NANOS_PER_SEC`, i.e. less than one tick. Counting in these units
    // makes `elapsed_ns * rate` exact, so a tick length that is not a whole
    // number of nanoseconds never introduces rounding drift.
    acc: u64,
}

impl FixedStep {
    /// Creates a clock with an empty accumulator.
    pub const fn new(cfg: StepConfig) -> FixedStep {
        FixedStep { cfg, acc: 0 }
    }

    /// The configuration this clock was built with.
    pub const fn config(&self) -> StepConfig {
        self.cfg
    }

    /// Adds `elapsed` real time and returns how many ticks are due.
    ///
    /// `elapsed` is the time since the previous call (or since the clock was
    /// created or [reset](FixedStep::reset)), measured by the caller. A zero
    /// `elapsed` is fine and returns zero ticks. Calling with a huge value
    /// (a debugger pause, a laptop waking from sleep) is also fine: the number
    /// of ticks is clamped to [`StepConfig::max_ticks_per_frame`] and the
    /// excess is dropped, reported through [`Advance::dropped_ticks`]. The
    /// sub-tick remainder is kept, so the interpolation fraction stays valid.
    ///
    /// ```
    /// use st_time::{FixedStep, Nanos};
    ///
    /// let mut clock = FixedStep::default(); // clamp: 8 ticks
    /// let a = clock.advance(Nanos::from_secs(60)); // a long stall
    /// assert_eq!(a.ticks(), 8);
    /// assert!(a.was_clamped());
    /// assert!(clock.advance(Nanos::ZERO).ticks() == 0); // backlog was dropped
    /// ```
    pub fn advance(&mut self, elapsed: Nanos) -> Advance {
        let rate = u128::from(self.cfg.rate_hz);
        // Wide arithmetic: u64 nanoseconds times a u32 rate always fits in u128,
        // so no input can overflow. The `acc` carried over is below one tick.
        let total = u128::from(self.acc) + u128::from(elapsed.as_nanos()) * rate;
        let unit = u128::from(NANOS_PER_SEC);
        let due = total / unit;
        // The remainder is always below `unit`, so it fits in a u64.
        self.acc = (total % unit) as u64;

        let max = u128::from(self.cfg.max_ticks);
        let run = due.min(max);
        let dropped = due - run;
        Advance {
            // `run <= max <= u32::MAX`.
            ticks: run as u32,
            // `due <= u64::MAX * 1e9 / 1e9`-ish; saturate rather than assume.
            dropped_ticks: u64::try_from(dropped).unwrap_or(u64::MAX),
            alpha_num: self.acc,
            alpha_den: NANOS_PER_SEC,
        }
    }

    /// Discards any accumulated partial tick.
    ///
    /// Call after an unusual discontinuity, such as loading a level, so the
    /// first frame afterwards does not inherit a stale fraction.
    ///
    /// ```
    /// use st_time::{FixedStep, Nanos};
    ///
    /// let mut clock = FixedStep::default();
    /// clock.advance(Nanos::from_millis(5));
    /// clock.reset();
    /// assert_eq!(clock.advance(Nanos::ZERO).alpha(), 0.0);
    /// ```
    pub fn reset(&mut self) {
        self.acc = 0;
    }
}

impl Default for FixedStep {
    /// A clock built from [`StepConfig::default`].
    fn default() -> FixedStep {
        FixedStep::new(StepConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_over_long_runs() {
        // 10 minutes in awkward 16.6 ms slices must yield exactly the right
        // count; float accumulation would drift here.
        let mut c = FixedStep::new(StepConfig::new(120, 1000).unwrap());
        let slice = 16_666_667u64;
        let n = 600_000_000_000u64 / slice;
        let mut ticks = 0u64;
        for _ in 0..n {
            ticks += u64::from(c.advance(Nanos::new(slice)).ticks());
        }
        let elapsed = n * slice;
        assert_eq!(ticks, elapsed * 120 / 1_000_000_000);
    }

    #[test]
    fn deterministic() {
        let run = || {
            let mut c = FixedStep::default();
            (0..500u64)
                .map(|i| {
                    c.advance(Nanos::new(1_000_000 + (i * 7919) % 30_000_000))
                        .ticks()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn clamp_and_drop() {
        let mut c = FixedStep::new(StepConfig::new(120, 3).unwrap());
        let a = c.advance(Nanos::from_secs(1)); // 120 ticks due
        assert_eq!((a.ticks(), a.dropped_ticks()), (3, 117));
        assert_eq!(c.advance(Nanos::ZERO).ticks(), 0);
    }

    #[test]
    fn extreme_input_does_not_overflow() {
        let mut c = FixedStep::new(StepConfig::new(1_000_000_000, u32::MAX).unwrap());
        let a = c.advance(Nanos::MAX);
        assert_eq!(a.ticks(), u32::MAX);
        assert!(a.was_clamped());
        let (n, d) = a.alpha_ratio();
        assert!(n < d);
    }

    #[test]
    fn alpha_in_range() {
        let mut c = FixedStep::default();
        for i in 0..1000u64 {
            let a = c.advance(Nanos::new(i * 123_457));
            assert!((0.0..1.0).contains(&a.alpha()));
        }
    }

    #[test]
    fn bad_config() {
        assert_eq!(
            StepConfig::new(2_000_000_000, 1),
            Err(ConfigError::RateTooHigh)
        );
        assert!(ConfigError::ZeroRate.to_string().contains("rate"));
    }
}
