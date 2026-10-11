//! The camera-relative render origin (ADR 0007).

use glam::{DVec3, Vec3};

/// Edge of the grid on which the render origin lies, in metres.
///
/// A power of two, so every grid point is exactly representable in `f32` and `f64` and a
/// shift between two grid points is exact in both.
const GRID: f64 = 4096.0;

/// Distance from the origin, per axis, beyond which the camera triggers a rebase: 0.75 of
/// [`GRID`], in metres.
///
/// Why 0.75 grid cells: after a rebase the camera is within half a cell (2,048 m) of the new
/// origin, so the camera has to travel another 1,024 m before the next rebase. That
/// hysteresis stops a camera hovering on a cell boundary from rebasing every frame, while
/// keeping the camera within 3,072 m per axis of the origin between rebases.
const REBASE_DISTANCE: f64 = 0.75 * GRID;

/// Rounds one coordinate to the nearest grid point, mapping everything that is not a
/// usable finite coordinate to the grid point `0.0`.
///
/// The `+ 0.0` turns `-0.0` into `+0.0`, so equal origins are also equal bit for bit.
#[inline]
fn snap(c: f64) -> f64 {
    if c.is_finite() {
        // Dividing and multiplying by a power of two is exact, so only `round` rounds.
        (c / GRID).round() * GRID + 0.0
    } else {
        0.0
    }
}

/// The render origin of the large-world scheme: a point in the current system frame,
/// always on a 4,096 m grid, that render-space positions are measured from.
///
/// The simulation holds positions as `f64` metres in the frame of a star system. An `f32`
/// cannot represent those with sub-millimetre accuracy far from the system origin, so the
/// renderer subtracts a nearby origin *in `f64`* and rounds the small difference once to
/// `f32`. Keeping the origin on a coarse grid, and moving it only when the camera has left
/// a neighbourhood of it, keeps render-space values stable between frames and makes every
/// shift of the origin exactly representable in `f32`.
///
/// The rule:
///
/// * the origin `O` has every coordinate an integer multiple of 4,096 m;
/// * once per rendered frame, before any render-space value is produced, call
///   [`rebase`](Self::rebase) with the camera position `c`. If `c` is more than 3,072 m
///   from `O` on any axis, `O` becomes `c` rounded per axis to the nearest grid point;
/// * [`to_render`](Self::to_render) converts a simulation position `p` to `(p - O)` as
///   `f32`, subtracting in `f64` and rounding once. This is the only conversion from
///   simulation positions to render positions.
///
/// Between rebases the camera is within 3,072 m of `O` on every axis, so everything within
/// 8,192 m of `O` is placed with an `f32` spacing of 2^-11 m (0.49 mm).
///
/// All views of a frame share one `RenderOrigin`. A jump to another star system starts a
/// new one with [`new`](Self::new).
///
/// # Non-finite input
///
/// The type never panics. A coordinate of a camera position that is `NaN` or infinite is
/// treated as unusable: [`new`](Self::new) places that axis of the origin at `0.0`, and
/// [`rebase`](Self::rebase) ignores such a camera position entirely. The origin therefore
/// always stays finite and on the grid.
///
/// # Example
///
/// ```
/// use st_math::{glam::{DVec3, Vec3}, RenderOrigin};
///
/// let mut origin = RenderOrigin::new(DVec3::new(10_000.0, 0.0, 0.0));
/// assert_eq!(origin.origin(), DVec3::new(8_192.0, 0.0, 0.0));
///
/// // The camera stays near the origin: nothing moves.
/// assert_eq!(origin.rebase(DVec3::new(11_000.0, 0.0, 0.0)), DVec3::ZERO);
///
/// // The camera flies on; the origin follows in whole 4,096 m steps.
/// let shift = origin.rebase(DVec3::new(16_000.0, 0.0, 0.0));
/// assert_eq!(shift, DVec3::new(8_192.0, 0.0, 0.0));
/// assert_eq!(origin.origin(), DVec3::new(16_384.0, 0.0, 0.0));
///
/// // Render-space positions are small and exact to a fraction of a millimetre.
/// let p = DVec3::new(16_001.5, 2.0, 3.0);
/// assert_eq!(origin.to_render(p), Vec3::new(-382.5, 2.0, 3.0));
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderOrigin {
    /// Invariant: every coordinate is finite and an integer multiple of [`GRID`], and no
    /// coordinate is `-0.0`.
    origin: DVec3,
}

impl RenderOrigin {
    /// Creates a render origin at the grid point nearest `camera`.
    ///
    /// Use this at the start of a session and as the reset after a change of star system.
    /// Each coordinate of `camera` is rounded to the nearest multiple of 4,096 m (ties away
    /// from zero); a coordinate that is `NaN` or infinite gives `0.0` on that axis.
    ///
    /// ```
    /// use st_math::{glam::DVec3, RenderOrigin};
    ///
    /// let origin = RenderOrigin::new(DVec3::new(2_048.0, -2_047.0, 5_000.0));
    /// assert_eq!(origin.origin(), DVec3::new(4_096.0, 0.0, 4_096.0));
    /// ```
    #[must_use]
    pub fn new(camera: DVec3) -> Self {
        Self {
            origin: DVec3::new(snap(camera.x), snap(camera.y), snap(camera.z)),
        }
    }

    /// Moves the origin when the camera has left its neighbourhood, and returns the shift.
    ///
    /// Call this once per rendered frame, before any render-space value is produced. If the
    /// camera is more than 3,072 m from the origin on any axis, the origin is set to
    /// `camera` rounded per axis to the nearest grid point, and the returned shift is
    /// `new origin - old origin`: a whole multiple of 4,096 m per axis, exactly
    /// representable in `f32` while the origin is within 2^23 m of the system origin (the
    /// validity bound of ADR 0007). The renderer shifts persistent render-space data by the
    /// negative of that shift. When the origin did not move, the result is
    /// [`DVec3::ZERO`].
    ///
    /// A `camera` with a `NaN` or infinite coordinate is ignored: the origin stays and the
    /// result is [`DVec3::ZERO`].
    ///
    /// ```
    /// use st_math::{glam::DVec3, RenderOrigin};
    ///
    /// let mut origin = RenderOrigin::new(DVec3::ZERO);
    /// // Exactly 3,072 m away is not "more than" 3,072 m.
    /// assert_eq!(origin.rebase(DVec3::new(3_072.0, 0.0, 0.0)), DVec3::ZERO);
    /// assert_eq!(
    ///     origin.rebase(DVec3::new(3_073.0, 0.0, 0.0)),
    ///     DVec3::new(4_096.0, 0.0, 0.0),
    /// );
    /// assert_eq!(origin.rebase(DVec3::new(f64::NAN, 0.0, 0.0)), DVec3::ZERO);
    /// ```
    #[must_use = "the renderer must shift persistent render-space data by the returned amount"]
    pub fn rebase(&mut self, camera: DVec3) -> DVec3 {
        if !camera.is_finite() {
            return DVec3::ZERO;
        }
        let offset = (camera - self.origin).abs();
        // `>` on each axis: a camera exactly on the threshold does not rebase. A NaN
        // offset cannot occur because both operands are finite; `max_element` is avoided
        // to stay clear of the platform-dependent `f64::max`.
        if offset.x > REBASE_DISTANCE || offset.y > REBASE_DISTANCE || offset.z > REBASE_DISTANCE {
            let previous = self.origin;
            self.origin = DVec3::new(snap(camera.x), snap(camera.y), snap(camera.z));
            self.origin - previous
        } else {
            DVec3::ZERO
        }
    }

    /// Converts a simulation position to a render-space position.
    ///
    /// Computes `(p - origin)` in `f64` and rounds the result once to `f32`. This is the
    /// only conversion from simulation positions to render positions; render-space values
    /// are metres relative to [`origin`](Self::origin). Within 8,192 m of the origin the
    /// result is within 0.25 mm of the exact difference.
    ///
    /// Beyond the `f32` range (about 3.4e38 m) the result is an infinity, and a `NaN`
    /// input gives `NaN` components; neither panics.
    ///
    /// ```
    /// use st_math::{glam::{DVec3, Vec3}, RenderOrigin};
    ///
    /// let origin = RenderOrigin::new(DVec3::new(8_000_000.0, 0.0, 0.0));
    /// let p = origin.origin() + DVec3::new(100.25, -3.0, 0.5);
    /// assert_eq!(origin.to_render(p), Vec3::new(100.25, -3.0, 0.5));
    /// ```
    #[inline]
    #[must_use]
    pub fn to_render(&self, p: DVec3) -> Vec3 {
        (p - self.origin).as_vec3()
    }

    /// Returns the origin, a point in the current system frame on the 4,096 m grid.
    ///
    /// ```
    /// use st_math::{glam::DVec3, RenderOrigin};
    ///
    /// assert_eq!(RenderOrigin::new(DVec3::ZERO).origin(), DVec3::ZERO);
    /// ```
    #[inline]
    #[must_use]
    pub fn origin(&self) -> DVec3 {
        self.origin
    }
}
