//! Integer time types and the simulation rate constants.

use core::fmt;
use core::time::Duration;

/// The simulation tick rate in hertz (ADR 0008: the simulation steps at a fixed
/// 120 Hz).
///
/// ```
/// assert_eq!(st_time::TICK_RATE_HZ, 120);
/// ```
pub const TICK_RATE_HZ: u32 = 120;

/// The length of one simulation tick in seconds, `1.0 / 120.0` (ADR 0008).
///
/// This is a `f64` constant because simulation code integrates with it. It is
/// the only float the simulation takes from this crate; tick *counting* is done
/// in integers (see [`FixedStep`](crate::FixedStep)).
///
/// ```
/// assert_eq!(st_time::DT, 1.0 / 120.0);
/// ```
pub const DT: f64 = 1.0 / 120.0;

/// A simulation tick index.
///
/// ADR 0008 and ADR 0009 define the tick counter as a `u64` owned by the
/// simulation; this alias only names that type so signatures can say what the
/// number means. It is not a distinct type.
///
/// ```
/// let t: st_time::Tick = 7_200; // 60 s at 120 Hz
/// assert_eq!(t, 60 * u64::from(st_time::TICK_RATE_HZ));
/// ```
pub type Tick = u64;

/// An integer duration in nanoseconds.
///
/// A `u64` of nanoseconds spans about 584 years, far more than any session.
/// Arithmetic here saturates instead of panicking or wrapping, so misuse
/// (for example subtracting a larger time from a smaller one) yields zero.
///
/// ```
/// use st_time::Nanos;
///
/// let frame = Nanos::from_millis(16) + Nanos::from_micros(667);
/// assert_eq!(frame.as_nanos(), 16_667_000);
/// assert_eq!(Nanos::from_secs(1).saturating_sub(Nanos::from_secs(2)), Nanos::ZERO);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nanos(u64);

impl Nanos {
    /// Zero duration.
    pub const ZERO: Nanos = Nanos(0);
    /// The largest representable duration.
    pub const MAX: Nanos = Nanos(u64::MAX);

    /// Creates a duration from a nanosecond count.
    pub const fn new(nanos: u64) -> Nanos {
        Nanos(nanos)
    }

    /// Creates a duration from microseconds, saturating at [`Nanos::MAX`].
    pub const fn from_micros(micros: u64) -> Nanos {
        Nanos(micros.saturating_mul(1_000))
    }

    /// Creates a duration from milliseconds, saturating at [`Nanos::MAX`].
    pub const fn from_millis(millis: u64) -> Nanos {
        Nanos(millis.saturating_mul(1_000_000))
    }

    /// Creates a duration from seconds, saturating at [`Nanos::MAX`].
    pub const fn from_secs(secs: u64) -> Nanos {
        Nanos(secs.saturating_mul(1_000_000_000))
    }

    /// Converts a [`Duration`], saturating at [`Nanos::MAX`].
    ///
    /// `Duration` is only a value here; this does not read any clock. A caller
    /// that measured with `Instant` can pass `later.duration_since(earlier)`.
    ///
    /// ```
    /// use std::time::Duration;
    /// use st_time::Nanos;
    ///
    /// assert_eq!(Nanos::from_duration(Duration::from_millis(2)), Nanos::from_millis(2));
    /// assert_eq!(Nanos::from_duration(Duration::MAX), Nanos::MAX);
    /// ```
    pub fn from_duration(d: Duration) -> Nanos {
        Nanos(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
    }

    /// Returns the nanosecond count.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Returns the duration in seconds as a `f64`.
    ///
    /// For display and profiling output only. Do not feed the result back into
    /// simulation state.
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1e9
    }

    /// Adds two durations, saturating at [`Nanos::MAX`].
    pub const fn saturating_add(self, other: Nanos) -> Nanos {
        Nanos(self.0.saturating_add(other.0))
    }

    /// Subtracts `other`, saturating at zero.
    pub const fn saturating_sub(self, other: Nanos) -> Nanos {
        Nanos(self.0.saturating_sub(other.0))
    }
}

impl core::ops::Add for Nanos {
    type Output = Nanos;
    /// Saturating addition; see [`Nanos::saturating_add`].
    fn add(self, rhs: Nanos) -> Nanos {
        self.saturating_add(rhs)
    }
}

impl core::ops::Sub for Nanos {
    type Output = Nanos;
    /// Saturating subtraction; see [`Nanos::saturating_sub`].
    fn sub(self, rhs: Nanos) -> Nanos {
        self.saturating_sub(rhs)
    }
}

impl From<Duration> for Nanos {
    fn from(d: Duration) -> Nanos {
        Nanos::from_duration(d)
    }
}

impl fmt::Display for Nanos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ns", self.0)
    }
}

/// The real-time length of `ticks` ticks at [`TICK_RATE_HZ`], rounded down to a
/// whole nanosecond and saturating at [`Nanos::MAX`].
///
/// One tick is 8 333 333.33... ns, so the result for a single tick is rounded;
/// use [`FixedStep`](crate::FixedStep) (which does not round) to count ticks.
///
/// ```
/// use st_time::{ticks_to_nanos, Nanos};
///
/// assert_eq!(ticks_to_nanos(120), Nanos::from_secs(1));
/// assert_eq!(ticks_to_nanos(1).as_nanos(), 8_333_333);
/// ```
pub fn ticks_to_nanos(ticks: Tick) -> Nanos {
    let n = u128::from(ticks) * 1_000_000_000 / u128::from(TICK_RATE_HZ);
    Nanos(u64::try_from(n).unwrap_or(u64::MAX))
}
