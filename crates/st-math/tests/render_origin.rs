//! Tests for `RenderOrigin` (ADR 0007), including precision at the edge of the envelope.
//!
//! The envelope: simulation coordinates within +-2^23 m of the system origin, the camera up
//! to 3,072 m from the render origin on each axis between rebases, and objects within
//! 8,192 m of the render origin.

use st_math::glam::{DVec3, Vec3};
use st_math::{RenderOrigin, det};

const GRID: f64 = 4096.0;
/// The validity bound of ADR 0007: every coordinate within this distance of the system origin.
const BOUND: f64 = 8_388_608.0;
/// Half an f32 ULP for magnitudes in [4096, 8192): 2^-12 m, 0.24 mm.
const HALF_ULP_8K: f64 = 1.0 / 4096.0;

/// Deterministic pseudo-random numbers in `[-1, 1)` (xorshift64*), so the tests need no
/// dependency and give the same samples on every target.
struct Samples(u64);

impl Samples {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let bits = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11; // 53 random bits
        (bits as f64) / ((1u64 << 52) as f64) - 1.0
    }

    fn vec(&mut self, scale: f64) -> DVec3 {
        DVec3::new(self.next(), self.next(), self.next()) * scale
    }
}

fn on_grid(v: DVec3) -> bool {
    v.to_array()
        .iter()
        .all(|c| c.is_finite() && c % GRID == 0.0)
}

fn max_abs(v: DVec3) -> f64 {
    det::max(det::max(v.x.abs(), v.y.abs()), v.z.abs())
}

#[test]
fn new_rounds_to_nearest_grid_point() {
    let cases = [
        (0.0, 0.0),
        (2047.9, 0.0),
        (2048.0, 4096.0), // ties away from zero
        (-2048.0, -4096.0),
        (-2047.9, 0.0),
        (4096.0, 4096.0),
        (10_000.0, 8192.0),
        (-10_000.0, -8192.0),
        (BOUND, BOUND),
        (-BOUND, -BOUND),
    ];
    for (c, want) in cases {
        let o = RenderOrigin::new(DVec3::new(c, c, c)).origin();
        assert_eq!(o, DVec3::splat(want), "camera {c}");
    }
}

#[test]
fn new_never_yields_negative_zero() {
    let o = RenderOrigin::new(DVec3::new(-0.0, -1.0, -2047.0)).origin();
    for c in o.to_array() {
        assert_eq!(c.to_bits(), 0.0_f64.to_bits());
    }
}

#[test]
fn origin_is_always_on_the_grid() {
    let mut rng = Samples(0x1234_5678_9ABC_DEF1);
    let mut origin = RenderOrigin::new(DVec3::ZERO);
    for _ in 0..10_000 {
        let camera = rng.vec(BOUND);
        let _ = origin.rebase(camera);
        assert!(on_grid(origin.origin()));
        assert!(on_grid(RenderOrigin::new(camera).origin()));
    }
}

#[test]
fn rebase_threshold_is_exactly_3072_metres_on_any_axis() {
    let edge: f64 = 3072.0;
    let beyond = f64::from_bits(edge.to_bits() + 1);
    for axis in 0..3 {
        let mut at_edge = [0.0; 3];
        at_edge[axis] = edge;
        let mut origin = RenderOrigin::new(DVec3::ZERO);
        assert_eq!(origin.rebase(DVec3::from_array(at_edge)), DVec3::ZERO);
        assert_eq!(origin.origin(), DVec3::ZERO);

        let mut past_edge = [0.0; 3];
        past_edge[axis] = beyond;
        let shift = origin.rebase(DVec3::from_array(past_edge));
        let mut want = [0.0; 3];
        want[axis] = GRID;
        assert_eq!(shift, DVec3::from_array(want), "axis {axis}");
        assert_eq!(origin.origin(), shift);
    }
    // Negative side.
    let mut origin = RenderOrigin::new(DVec3::ZERO);
    assert_eq!(origin.rebase(DVec3::new(-edge, 0.0, 0.0)), DVec3::ZERO);
    assert_eq!(
        origin.rebase(DVec3::new(-beyond, 0.0, 0.0)),
        DVec3::new(-GRID, 0.0, 0.0)
    );
}

#[test]
fn rebase_rounds_every_axis_when_any_axis_triggers() {
    // x triggers the rebase; y is 2,500 m from the origin, inside the threshold but nearer
    // to the next grid point, so the rule "round c per axis" moves it too.
    let mut origin = RenderOrigin::new(DVec3::ZERO);
    let shift = origin.rebase(DVec3::new(9000.0, 2500.0, 100.0));
    assert_eq!(shift, DVec3::new(8192.0, 4096.0, 0.0));
    assert_eq!(origin.origin(), shift);
}

#[test]
fn rebase_returns_zero_and_keeps_origin_when_camera_stays_close() {
    let mut origin = RenderOrigin::new(DVec3::new(100_000.0, 0.0, -50_000.0));
    let before = origin;
    let mut rng = Samples(42);
    for _ in 0..1000 {
        let camera = before.origin() + rng.vec(3072.0);
        assert_eq!(origin.rebase(camera), DVec3::ZERO);
        assert_eq!(origin, before);
    }
}

#[test]
fn camera_stays_within_3072_metres_between_rebases_and_2048_after_one() {
    let mut rng = Samples(7);
    let mut origin = RenderOrigin::new(DVec3::ZERO);
    for _ in 0..10_000 {
        let camera = rng.vec(BOUND);
        let shift = origin.rebase(camera);
        let offset = max_abs(camera - origin.origin());
        assert!(offset <= 3072.0, "offset {offset}");
        if shift != DVec3::ZERO {
            assert!(offset <= GRID / 2.0, "offset after rebase {offset}");
        }
    }
}

#[test]
fn rebase_shift_is_a_whole_number_of_cells_and_exact_in_f32() {
    let mut rng = Samples(99);
    let mut origin = RenderOrigin::new(DVec3::ZERO);
    for _ in 0..10_000 {
        let before = origin.origin();
        let shift = origin.rebase(rng.vec(BOUND));
        assert_eq!(origin.origin(), before + shift);
        for c in shift.to_array() {
            assert_eq!(c % GRID, 0.0);
            // Exactly representable in f32: the renderer can apply it to persistent data.
            assert_eq!(f64::from(c as f32), c);
        }
    }
    // The largest possible shift inside the validity bound: -2^23 to +2^23.
    let mut origin = RenderOrigin::new(DVec3::splat(-BOUND));
    let shift = origin.rebase(DVec3::splat(BOUND));
    assert_eq!(shift, DVec3::splat(2.0 * BOUND));
    assert_eq!(f64::from((2.0 * BOUND) as f32), 2.0 * BOUND);
}

#[test]
fn to_render_is_one_rounding_of_the_f64_difference() {
    let origin = RenderOrigin::new(DVec3::new(123_456.0, -654_321.0, 7_000_000.0));
    let mut rng = Samples(5);
    for _ in 0..1000 {
        let p = origin.origin() + rng.vec(20_000.0);
        assert_eq!(origin.to_render(p), (p - origin.origin()).as_vec3());
    }
}

#[test]
fn precision_at_the_envelope_edge() {
    // Origins spread over the whole validity bound, the camera at the very edge of the
    // no-rebase zone (3,072 m on every axis, both signs), objects up to 8,192 m from the
    // origin: the worst case of the ADR's precision budget.
    let mut rng = Samples(2026);
    let origins = [
        DVec3::ZERO,
        DVec3::new(GRID, -GRID, 0.0),
        DVec3::splat(1.0e6 - 1.0e6 % GRID),
        DVec3::splat(-BOUND),
        DVec3::splat(BOUND),
        DVec3::new(BOUND, -BOUND, BOUND - GRID),
    ];
    let signs = [-1.0, 1.0];
    for o in origins {
        let mut origin = RenderOrigin::new(o);
        assert_eq!(origin.origin(), o);
        for sx in signs {
            for sy in signs {
                for sz in signs {
                    let camera = o + DVec3::new(sx, sy, sz) * 3072.0;
                    assert_eq!(
                        origin.rebase(camera),
                        DVec3::ZERO,
                        "camera at the edge must not rebase"
                    );
                    let camera_r = origin.to_render(camera);
                    for _ in 0..200 {
                        // An object up to 5,000 m from the camera, hence within 8,072 m of O.
                        let offset = rng.vec(5000.0 / 3.0_f64.sqrt());
                        let p = camera + offset;
                        assert!(max_abs(p - o) < 8192.0);
                        let r = origin.to_render(p);
                        // Absolute: at most half an f32 ULP (0.24 mm) for |p - O| < 8,192 m.
                        let exact = p - o;
                        let err = (DVec3::from(r) - exact).abs();
                        assert!(max_abs(err) <= HALF_ULP_8K, "abs error {err:?} at {o:?}");
                        // Relative to the camera: both ends rounded, so at most one full ULP
                        // (0.49 mm), the figure of the ADR budget.
                        let rel = DVec3::from(r - camera_r);
                        let rel_err = (rel - offset).abs();
                        assert!(
                            max_abs(rel_err) <= 2.0 * HALF_ULP_8K,
                            "rel error {rel_err:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn naive_f32_conversion_is_off_by_metres_where_render_origin_is_not() {
    // The reason RenderOrigin exists: 8 million metres from the system origin an f32 has a
    // spacing of 1 m.
    let camera = DVec3::new(8_000_000.0, 0.0, 0.0);
    let origin = RenderOrigin::new(camera);
    let p = camera + DVec3::new(1.234_567, 0.0, 0.0);
    let naive = p.as_vec3() - camera.as_vec3();
    let ours = origin.to_render(p) - origin.to_render(camera);
    let want = 1.234_567;
    assert!((f64::from(naive.x) - want).abs() >= 0.2);
    assert!((f64::from(ours.x) - want).abs() <= 2.0 * HALF_ULP_8K);
}

#[test]
fn shifting_render_space_data_by_the_rebase_shift_stays_within_half_an_ulp() {
    // ADR 0007: after a rebase the renderer shifts persistent data by -shift; for results
    // within 8 km of the new origin the shift itself rounds by at most 0.25 mm.
    let mut rng = Samples(31337);
    let mut origin = RenderOrigin::new(DVec3::splat(5_000_000.0));
    for _ in 0..2000 {
        let old_origin = origin.origin();
        let camera = old_origin + rng.vec(40_000.0);
        let shift = origin.rebase(camera);
        if shift == DVec3::ZERO {
            continue;
        }
        let p = origin.origin() + rng.vec(4000.0);
        let stored = {
            // What the renderer stored under the old origin.
            RenderOrigin::new(old_origin).to_render(p)
        };
        let shifted = stored - shift.as_vec3();
        let want_exact = DVec3::from(stored) - shift;
        assert!(max_abs(DVec3::from(shifted) - want_exact) <= HALF_ULP_8K);
    }
}

#[test]
fn non_finite_input_never_panics_and_keeps_the_origin_valid() {
    let bad = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
    for b in bad {
        for camera in [
            DVec3::new(b, 0.0, 0.0),
            DVec3::new(0.0, b, 0.0),
            DVec3::new(0.0, 0.0, b),
            DVec3::splat(b),
        ] {
            assert!(on_grid(RenderOrigin::new(camera).origin()));

            let mut origin = RenderOrigin::new(DVec3::new(8192.0, 0.0, 0.0));
            let before = origin;
            assert_eq!(origin.rebase(camera), DVec3::ZERO);
            assert_eq!(origin, before);

            // Conversion of a bad position gives non-finite components, not a panic.
            let r = origin.to_render(camera);
            assert!(!r.is_finite());
        }
    }
    assert_eq!(
        RenderOrigin::new(DVec3::new(f64::NAN, 5000.0, f64::INFINITY)).origin(),
        DVec3::new(0.0, 4096.0, 0.0)
    );
}

#[test]
fn huge_finite_input_does_not_panic() {
    let mut origin = RenderOrigin::new(DVec3::splat(f64::MAX));
    assert!(on_grid(origin.origin()));
    let shift = origin.rebase(DVec3::splat(f64::MIN));
    // The shift may overflow to infinity far outside the validity bound; it must not panic.
    let _ = shift;
    let _ = origin.to_render(DVec3::splat(f64::MAX));
    let _: Vec3 = origin.to_render(DVec3::ZERO);
}
