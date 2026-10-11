//! Monotonic real-time clock returning [`Nanos`].

use st_time::Nanos;
use std::time::Instant;

/// A monotonic clock that reports elapsed time since its creation.
///
/// Monotonic means the value never goes backwards, even when the user changes
/// the system time, which makes it the right source for frame timing. It is
/// *not* a calendar clock.
///
/// `st-time` never reads a clock itself (ADR 0008); the session loop measures
/// with this type and passes the result in.
///
/// On `wasm32-unknown-unknown`, `std::time::Instant::now` panics, so this
/// type never calls it there. [`Clock::is_supported`] returns `false`,
/// [`Clock::now`] always returns [`Nanos::ZERO`], and a fixed-step accumulator
/// fed from it will not tick. A browser clock needs a new dependency and comes
/// with the browser target.
///
/// ```
/// use st_platform::Clock;
///
/// let clock = Clock::new();
/// let a = clock.now();
/// let b = clock.now();
/// assert!(b >= a);
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    // `None` where the platform has no usable `Instant`. Holding an `Option`
    // keeps one code path for callers instead of two `cfg`-gated types.
    origin: Option<Instant>,
}

impl Clock {
    /// Returns whether this target has a working monotonic clock.
    ///
    /// `false` only on `wasm32-unknown-unknown`.
    pub const fn is_supported() -> bool {
        cfg!(not(all(target_arch = "wasm32", target_os = "unknown")))
    }

    /// Starts a clock; [`Clock::now`] counts from this moment.
    pub fn new() -> Clock {
        Clock {
            origin: Self::is_supported().then(Self::instant_now),
        }
    }

    /// Returns the time elapsed since [`Clock::new`], or [`Nanos::ZERO`] when
    /// the target has no clock ([`Clock::is_supported`]).
    ///
    /// Saturates at [`Nanos::MAX`] (about 584 years).
    pub fn now(&self) -> Nanos {
        match self.origin {
            Some(origin) => Nanos::from_duration(origin.elapsed()),
            None => Nanos::ZERO,
        }
    }

    // The only place `Instant::now` is called. It is reached only when
    // `is_supported()` holds, so it cannot run on wasm32-unknown-unknown.
    fn instant_now() -> Instant {
        Instant::now()
    }
}

impl Default for Clock {
    fn default() -> Clock {
        Clock::new()
    }
}

/// Measures the time between frames.
///
/// Call [`FrameTimer::lap`] once per frame and pass the result to the
/// fixed-step accumulator. The first lap measures from [`FrameTimer::new`].
///
/// ```
/// use st_platform::FrameTimer;
/// use st_time::Nanos;
///
/// let mut timer = FrameTimer::new();
/// let frame_time: Nanos = timer.lap();
/// assert!(frame_time < Nanos::from_secs(60));
/// ```
#[derive(Clone, Copy, Debug)]
pub struct FrameTimer {
    clock: Clock,
    last: Nanos,
}

impl FrameTimer {
    /// Starts a timer on a fresh [`Clock`].
    pub fn new() -> FrameTimer {
        FrameTimer::with_clock(Clock::new())
    }

    /// Starts a timer on an existing clock, so that frame times and other
    /// measurements share one time base.
    pub fn with_clock(clock: Clock) -> FrameTimer {
        let last = clock.now();
        FrameTimer { clock, last }
    }

    /// Returns the clock this timer reads.
    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// Returns the time since the previous lap (or since creation) and starts
    /// the next one. Returns [`Nanos::ZERO`] when the clock is unsupported.
    pub fn lap(&mut self) -> Nanos {
        let now = self.clock.now();
        // `Clock::now` is monotonic; the saturating subtraction only matters
        // if a caller built the timer from a clock and rewound by other means.
        let elapsed = now.saturating_sub(self.last);
        self.last = now;
        elapsed
    }
}

impl Default for FrameTimer {
    fn default() -> FrameTimer {
        FrameTimer::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn now_never_decreases() {
        let clock = Clock::new();
        let mut previous = clock.now();
        for _ in 0..1000 {
            let next = clock.now();
            assert!(next >= previous);
            previous = next;
        }
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[test]
    fn clock_advances_with_real_time() {
        use std::time::Duration;
        let clock = Clock::new();
        assert!(Clock::is_supported());
        std::thread::sleep(Duration::from_millis(5));
        assert!(clock.now() >= Nanos::from_millis(5));
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    #[test]
    fn laps_partition_time() {
        use std::time::Duration;
        let clock = Clock::new();
        let mut timer = FrameTimer::with_clock(clock);
        std::thread::sleep(Duration::from_millis(2));
        let first = timer.lap();
        let second = timer.lap();
        assert!(first >= Nanos::from_millis(2));
        // The two laps cover exactly the span the clock measured.
        assert!(first.saturating_add(second) <= clock.now());
    }
}
