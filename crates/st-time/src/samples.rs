//! Fixed-capacity duration sample windows with percentile statistics.

use crate::Nanos;

/// Summary statistics of a [`SampleWindow`], in nanoseconds.
///
/// ```
/// use st_time::{Nanos, SampleWindow};
///
/// let mut w = SampleWindow::<8>::new();
/// w.record(Nanos::from_millis(4));
/// w.record(Nanos::from_millis(6));
/// let s = w.summary().unwrap();
/// assert_eq!(s.mean, Nanos::from_millis(5));
/// assert_eq!(s.count, 2);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Number of samples in the window.
    pub count: usize,
    /// Smallest sample.
    pub min: Nanos,
    /// Largest sample.
    pub max: Nanos,
    /// Arithmetic mean, rounded down.
    pub mean: Nanos,
    /// 95th percentile (nearest rank).
    pub p95: Nanos,
    /// 99th percentile (nearest rank).
    pub p99: Nanos,
}

/// A ring buffer of the most recent `N` duration samples.
///
/// The storage is inline (`N` `u64`s), so it never allocates and the caller
/// chooses the window size at compile time. When full, the oldest sample is
/// overwritten. Percentiles use the *nearest-rank* method on a sorted copy of
/// the window, so they are always one of the recorded samples. Computing a
/// percentile sorts up to `N` values; call it when reporting, not every frame.
///
/// `N == 0` is allowed: such a window ignores samples and reports nothing.
///
/// ```
/// use st_time::{Nanos, SampleWindow};
///
/// let mut w = SampleWindow::<100>::new();
/// for ms in 1..=100 {
///     w.record(Nanos::from_millis(ms));
/// }
/// assert_eq!(w.p95(), Some(Nanos::from_millis(95)));
/// assert_eq!(w.p99(), Some(Nanos::from_millis(99)));
/// assert_eq!(w.max(), Some(Nanos::from_millis(100)));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleWindow<const N: usize> {
    buf: [u64; N],
    // Number of valid samples, at most `N`.
    len: usize,
    // Index the next sample is written to.
    next: usize,
}

impl<const N: usize> SampleWindow<N> {
    /// Creates an empty window.
    pub const fn new() -> SampleWindow<N> {
        SampleWindow {
            buf: [0; N],
            len: 0,
            next: 0,
        }
    }

    /// The window capacity, `N`.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// The number of samples currently held (at most `N`).
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the window holds no samples.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Records a sample, overwriting the oldest one when the window is full.
    pub fn record(&mut self, sample: Nanos) {
        if N == 0 {
            return;
        }
        self.buf[self.next] = sample.as_nanos();
        self.next = (self.next + 1) % N;
        if self.len < N {
            self.len += 1;
        }
    }

    /// Forgets all samples.
    pub fn clear(&mut self) {
        self.len = 0;
        self.next = 0;
    }

    // The valid samples, in storage order (not chronological once wrapped, which
    // no statistic here depends on).
    fn samples(&self) -> &[u64] {
        &self.buf[..self.len]
    }

    /// The smallest sample, or `None` if empty.
    pub fn min(&self) -> Option<Nanos> {
        self.samples().iter().copied().min().map(Nanos::new)
    }

    /// The largest sample, or `None` if empty.
    pub fn max(&self) -> Option<Nanos> {
        self.samples().iter().copied().max().map(Nanos::new)
    }

    /// The mean, rounded down, or `None` if empty.
    pub fn mean(&self) -> Option<Nanos> {
        if self.len == 0 {
            return None;
        }
        // u128 sum: `N` samples of up to u64::MAX cannot overflow it.
        let sum: u128 = self.samples().iter().map(|&s| u128::from(s)).sum();
        // The mean of u64 values is at most u64::MAX.
        Some(Nanos::new((sum / self.len as u128) as u64))
    }

    /// The `permille / 1000` quantile by the nearest-rank method, or `None` if
    /// empty. `permille` is clamped to `0..=1000`; 950 is the 95th percentile.
    ///
    /// ```
    /// use st_time::{Nanos, SampleWindow};
    ///
    /// let mut w = SampleWindow::<4>::new();
    /// for ms in [1, 2, 3, 40] {
    ///     w.record(Nanos::from_millis(ms));
    /// }
    /// assert_eq!(w.percentile_permille(500), Some(Nanos::from_millis(2)));
    /// assert_eq!(w.percentile_permille(5000), Some(Nanos::from_millis(40)));
    /// ```
    pub fn percentile_permille(&self, permille: u32) -> Option<Nanos> {
        if self.len == 0 {
            return None;
        }
        let mut sorted = self.buf;
        let sorted = &mut sorted[..self.len];
        sorted.sort_unstable();
        Some(Nanos::new(nearest_rank(sorted, permille)))
    }

    /// The 95th percentile, or `None` if empty.
    pub fn p95(&self) -> Option<Nanos> {
        self.percentile_permille(950)
    }

    /// The 99th percentile, or `None` if empty.
    pub fn p99(&self) -> Option<Nanos> {
        self.percentile_permille(990)
    }

    /// All statistics at once (one sort), or `None` if empty.
    pub fn summary(&self) -> Option<Summary> {
        if self.len == 0 {
            return None;
        }
        let mut sorted = self.buf;
        let sorted = &mut sorted[..self.len];
        sorted.sort_unstable();
        Some(Summary {
            count: self.len,
            min: Nanos::new(sorted[0]),
            max: Nanos::new(sorted[self.len - 1]),
            mean: self.mean().unwrap_or(Nanos::ZERO),
            p95: Nanos::new(nearest_rank(sorted, 950)),
            p99: Nanos::new(nearest_rank(sorted, 990)),
        })
    }
}

impl<const N: usize> Default for SampleWindow<N> {
    fn default() -> SampleWindow<N> {
        SampleWindow::new()
    }
}

/// Nearest-rank quantile of a non-empty ascending slice.
fn nearest_rank(sorted: &[u64], permille: u32) -> u64 {
    let n = sorted.len() as u128;
    let pm = u128::from(permille.min(1000));
    // rank = ceil(n * q), 1-based, at least 1 so the 0th percentile is the min.
    let rank = (n * pm).div_ceil(1000).max(1);
    sorted[(rank - 1) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_reports_none() {
        let w = SampleWindow::<4>::new();
        assert!(w.is_empty());
        assert_eq!(
            (w.min(), w.mean(), w.p95(), w.summary()),
            (None, None, None, None)
        );
    }

    #[test]
    fn wraps_and_drops_oldest() {
        let mut w = SampleWindow::<3>::new();
        for ms in 1..=5 {
            w.record(Nanos::from_millis(ms));
        }
        assert_eq!(w.len(), 3);
        assert_eq!(w.min(), Some(Nanos::from_millis(3)));
        assert_eq!(w.max(), Some(Nanos::from_millis(5)));
    }

    #[test]
    fn zero_capacity_is_inert() {
        let mut w = SampleWindow::<0>::new();
        w.record(Nanos::from_secs(1));
        assert!(w.is_empty());
        assert_eq!(w.summary(), None);
    }

    #[test]
    fn mean_does_not_overflow() {
        let mut w = SampleWindow::<2>::new();
        w.record(Nanos::MAX);
        w.record(Nanos::MAX);
        assert_eq!(w.mean(), Some(Nanos::MAX));
    }

    #[test]
    fn clear_resets() {
        let mut w = SampleWindow::<2>::new();
        w.record(Nanos::new(1));
        w.clear();
        assert!(w.is_empty());
    }
}
