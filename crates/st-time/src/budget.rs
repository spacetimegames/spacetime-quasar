//! Frame-budget accounting.

use crate::{Nanos, SampleWindow, Summary};

/// What [`FrameBudget::end_frame`] learned about the frame that just finished.
///
/// ```
/// use st_time::{FrameBudget, Nanos};
///
/// let mut fb = FrameBudget::<1, 16>::new(Nanos::from_millis(8), ["sim"]);
/// let r = fb.end_frame(Nanos::from_millis(10));
/// assert!(r.over_budget);
/// assert_eq!(r.overrun, Nanos::from_millis(2));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameReport {
    /// Whether the frame total exceeded the budget.
    pub over_budget: bool,
    /// By how much it exceeded it; zero when within budget.
    pub overrun: Nanos,
}

/// Per-phase and whole-frame timing against a budget.
///
/// `PHASES` is the number of named phases a frame is divided into (for example
/// simulate, cull, draw, present), and `WINDOW` is how many recent frames the
/// statistics cover. Phase names are plain `&'static str` labels supplied by
/// the caller; the crate attaches no meaning to them.
///
/// The caller measures durations with its own clock and feeds them in:
///
/// 1. [`FrameBudget::record_phase`] zero or more times per frame. Several
///    records for the same phase in one frame add up (a phase may consist of
///    many passes).
/// 2. [`FrameBudget::end_frame`] with the measured total. This pushes each
///    phase's sum and the total into their windows and resets the phase sums.
///
/// There is no allocation and no hash map; storage is inline and sized by the
/// const parameters.
///
/// ```
/// use st_time::{FrameBudget, Nanos};
///
/// // 120 Hz display budget, three phases, statistics over the last 240 frames.
/// let mut fb = FrameBudget::<3, 240>::new(
///     Nanos::new(8_333_333),
///     ["sim", "cull", "draw"],
/// );
///
/// fb.record_phase(0, Nanos::from_millis(2));
/// fb.record_phase(1, Nanos::from_millis(1));
/// fb.record_phase(2, Nanos::from_millis(4));
/// let report = fb.end_frame(Nanos::from_millis(7));
/// assert!(!report.over_budget);
///
/// assert_eq!(fb.phase_name(2), Some("draw"));
/// assert_eq!(fb.phase_summary(2).unwrap().mean, Nanos::from_millis(4));
/// assert_eq!(fb.frame_summary().unwrap().count, 1);
/// assert_eq!(fb.over_budget_frames(), 0);
/// ```
#[derive(Clone, Debug)]
pub struct FrameBudget<const PHASES: usize, const WINDOW: usize> {
    budget: Nanos,
    names: [&'static str; PHASES],
    current: [Nanos; PHASES],
    phases: [SampleWindow<WINDOW>; PHASES],
    frame: SampleWindow<WINDOW>,
    frames: u64,
    over: u64,
}

impl<const PHASES: usize, const WINDOW: usize> FrameBudget<PHASES, WINDOW> {
    /// Creates an accountant with a per-frame `budget` and phase `names`.
    pub fn new(budget: Nanos, names: [&'static str; PHASES]) -> Self {
        FrameBudget {
            budget,
            names,
            current: [Nanos::ZERO; PHASES],
            phases: [SampleWindow::new(); PHASES],
            frame: SampleWindow::new(),
            frames: 0,
            over: 0,
        }
    }

    /// The per-frame budget.
    pub fn budget(&self) -> Nanos {
        self.budget
    }

    /// Changes the budget. Past frames are not re-judged.
    pub fn set_budget(&mut self, budget: Nanos) {
        self.budget = budget;
    }

    /// Adds `duration` to phase `phase` for the frame in progress.
    ///
    /// Returns `false` and records nothing if `phase >= PHASES`; this is not a
    /// panic, so a stale index is harmless.
    pub fn record_phase(&mut self, phase: usize, duration: Nanos) -> bool {
        match self.current.get_mut(phase) {
            Some(slot) => {
                *slot = slot.saturating_add(duration);
                true
            }
            None => false,
        }
    }

    /// Finishes the frame whose total measured duration is `total`.
    ///
    /// `total` is supplied rather than summed from the phases because a frame
    /// usually includes time outside any named phase (waiting for vsync,
    /// event handling). Each phase's sum for this frame, including zero for a
    /// phase not recorded, is pushed to its window, then the sums reset.
    pub fn end_frame(&mut self, total: Nanos) -> FrameReport {
        for (window, slot) in self.phases.iter_mut().zip(self.current.iter_mut()) {
            window.record(*slot);
            *slot = Nanos::ZERO;
        }
        self.frame.record(total);
        self.frames = self.frames.saturating_add(1);
        let over_budget = total > self.budget;
        if over_budget {
            self.over = self.over.saturating_add(1);
        }
        FrameReport {
            over_budget,
            overrun: total.saturating_sub(self.budget),
        }
    }

    /// The name of phase `phase`, or `None` if out of range.
    pub fn phase_name(&self, phase: usize) -> Option<&'static str> {
        self.names.get(phase).copied()
    }

    /// The statistics for phase `phase` over the window, or `None` if the index
    /// is out of range or no frame has finished.
    pub fn phase_summary(&self, phase: usize) -> Option<Summary> {
        self.phases.get(phase).and_then(SampleWindow::summary)
    }

    /// The statistics for whole-frame totals over the window, or `None` if no
    /// frame has finished.
    pub fn frame_summary(&self) -> Option<Summary> {
        self.frame.summary()
    }

    /// Frames finished since creation (or the last [`FrameBudget::reset`]).
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Of those, how many exceeded the budget.
    pub fn over_budget_frames(&self) -> u64 {
        self.over
    }

    /// Discards all samples and counters, keeping the budget and names.
    pub fn reset(&mut self) {
        self.current = [Nanos::ZERO; PHASES];
        for w in &mut self.phases {
            w.clear();
        }
        self.frame.clear();
        self.frames = 0;
        self.over = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_accumulate_within_a_frame() {
        let mut fb = FrameBudget::<2, 8>::new(Nanos::from_millis(10), ["a", "b"]);
        fb.record_phase(0, Nanos::from_millis(1));
        fb.record_phase(0, Nanos::from_millis(2));
        fb.end_frame(Nanos::from_millis(5));
        assert_eq!(fb.phase_summary(0).unwrap().max, Nanos::from_millis(3));
        // The unrecorded phase counts as a zero sample, not a missing one.
        assert_eq!(fb.phase_summary(1).unwrap().max, Nanos::ZERO);
        // Sums were reset for the next frame.
        fb.end_frame(Nanos::from_millis(5));
        assert_eq!(fb.phase_summary(0).unwrap().min, Nanos::ZERO);
    }

    #[test]
    fn bad_index_is_harmless() {
        let mut fb = FrameBudget::<1, 4>::new(Nanos::from_millis(1), ["x"]);
        assert!(!fb.record_phase(5, Nanos::from_millis(1)));
        assert_eq!(fb.phase_name(5), None);
        assert_eq!(fb.phase_summary(5), None);
    }

    #[test]
    fn over_budget_counting_and_reset() {
        let mut fb = FrameBudget::<0, 4>::new(Nanos::from_millis(8), []);
        assert!(!fb.end_frame(Nanos::from_millis(8)).over_budget);
        assert!(fb.end_frame(Nanos::from_millis(9)).over_budget);
        assert_eq!((fb.frames(), fb.over_budget_frames()), (2, 1));
        fb.reset();
        assert_eq!((fb.frames(), fb.frame_summary()), (0, None));
    }
}
