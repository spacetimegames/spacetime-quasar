//! Behaviour tests for `st_math::interp`. Bit-pattern goldens are in `interp_golden.rs`.

use st_math::det;
use st_math::glam::{DQuat, DVec3};
use st_math::interp::{hermite, hermite_velocity, lerp, lerp_vec3, nlerp, slerp};

/// SplitMix64: a tiny deterministic generator for test inputs.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + (hi - lo) * unit
    }

    fn vec3(&mut self, scale: f64) -> DVec3 {
        DVec3::new(
            self.range(-scale, scale),
            self.range(-scale, scale),
            self.range(-scale, scale),
        )
    }

    fn unit_quat(&mut self) -> DQuat {
        loop {
            let q = DQuat::from_xyzw(
                self.range(-1.0, 1.0),
                self.range(-1.0, 1.0),
                self.range(-1.0, 1.0),
                self.range(-1.0, 1.0),
            );
            let len = q.length();
            if len > 0.1 && len <= 1.0 {
                return q / len;
            }
        }
    }
}

/// Angle of the rotation that takes `a` to `b`, in radians in `[0, pi]`.
fn angle_between(a: DQuat, b: DQuat) -> f64 {
    let d = det::min(a.dot(b).abs(), 1.0);
    2.0 * det::acos(d)
}

#[test]
fn lerp_hits_both_ends_exactly() {
    let mut rng = Rng(10);
    for _ in 0..1000 {
        let (a, b) = (rng.range(-1e9, 1e9), rng.range(-1e9, 1e9));
        assert_eq!(lerp(a, b, 0.0), a);
        assert_eq!(lerp(a, b, 1.0), b);
        let v = (rng.vec3(1e9), rng.vec3(1e9));
        assert_eq!(lerp_vec3(v.0, v.1, 0.0), v.0);
        assert_eq!(lerp_vec3(v.0, v.1, 1.0), v.1);
    }
    assert!(lerp(0.0, 1.0, f64::NAN).is_nan());
    assert_eq!(lerp(0.0, 1.0, -1.0), -1.0);
}

#[test]
fn nlerp_and_slerp_are_exact_at_the_ends_and_unit_length() {
    let mut rng = Rng(11);
    for _ in 0..1000 {
        let (a, b) = (rng.unit_quat(), rng.unit_quat());
        let same_rotation = |q: DQuat, want: DQuat| (q.dot(want).abs() - 1.0).abs() < 1e-14;
        for f in [nlerp, slerp] {
            assert!(same_rotation(f(a, b, 0.0), a));
            assert!(same_rotation(f(a, b, 1.0), b));
            let mid = f(a, b, rng.range(0.0, 1.0));
            assert!((mid.length() - 1.0).abs() < 1e-14);
        }
    }
}

#[test]
fn slerp_has_constant_angular_speed() {
    let mut rng = Rng(12);
    for _ in 0..500 {
        let (a, b) = (rng.unit_quat(), rng.unit_quat());
        let total = angle_between(a, b);
        for k in 1..8 {
            let t = k as f64 / 8.0;
            let from_a = angle_between(a, slerp(a, b, t));
            assert!((from_a - t * total).abs() < 1e-7, "t={t} total={total}");
        }
    }
}

#[test]
fn quaternion_blends_take_the_shorter_arc() {
    let a = DQuat::from_rotation_y(0.2);
    let b = DQuat::from_rotation_y(0.6);
    // -b is the same rotation as b, so the path must not swing the long way round.
    let neg_b = -b;
    for f in [nlerp, slerp] {
        let direct = f(a, b, 0.5);
        let via_neg = f(a, neg_b, 0.5);
        assert!((direct.dot(via_neg).abs() - 1.0).abs() < 1e-14);
        assert!((angle_between(direct, DQuat::from_rotation_y(0.4))).abs() < 1e-7);
    }
}

#[test]
fn slerp_of_nearly_equal_and_opposite_rotations_is_finite() {
    let a = DQuat::from_rotation_x(0.3);
    let near = DQuat::from_rotation_x(0.3 + 1e-9);
    let q = slerp(a, near, 0.5);
    assert!(q.is_finite() && (q.length() - 1.0).abs() < 1e-14);
    // The same rotation, written with the other sign.
    let q = slerp(a, -a, 0.5);
    assert!((q.dot(a).abs() - 1.0).abs() < 1e-14);
    // A half-turn apart: any arc is valid but the result must be a unit quaternion.
    let half_turn = DQuat::from_rotation_z(core::f64::consts::PI);
    let q = slerp(DQuat::IDENTITY, half_turn, 0.5);
    assert!((q.length() - 1.0).abs() < 1e-14);
    assert!((angle_between(DQuat::IDENTITY, q) - core::f64::consts::FRAC_PI_2).abs() < 1e-7);
}

#[test]
fn quaternion_blends_fall_back_to_identity_on_bad_input() {
    let q = DQuat::from_rotation_y(0.5);
    for f in [nlerp, slerp] {
        assert_eq!(f(q, q, f64::NAN), DQuat::IDENTITY);
        assert_eq!(
            f(DQuat::from_xyzw(f64::NAN, 0.0, 0.0, 1.0), q, 0.5),
            DQuat::IDENTITY
        );
        assert_eq!(
            f(
                DQuat::from_xyzw(0.0, 0.0, 0.0, 0.0),
                DQuat::from_xyzw(0.0, 0.0, 0.0, 0.0),
                0.5
            ),
            DQuat::IDENTITY
        );
        assert_eq!(f(q, q, f64::INFINITY), DQuat::IDENTITY);
    }
}

#[test]
fn hermite_hits_both_ends_exactly() {
    let mut rng = Rng(13);
    for _ in 0..1000 {
        let (p0, v0, p1, v1) = (
            rng.vec3(1e6),
            rng.vec3(100.0),
            rng.vec3(1e6),
            rng.vec3(100.0),
        );
        let dt = rng.range(0.001, 10.0);
        assert_eq!(hermite(p0, v0, p1, v1, dt, 0.0), p0);
        assert_eq!(hermite(p0, v0, p1, v1, dt, 1.0), p1);
        let v_start = hermite_velocity(p0, v0, p1, v1, dt, 0.0).unwrap();
        let v_end = hermite_velocity(p0, v0, p1, v1, dt, 1.0).unwrap();
        // The end velocities come out of a division by the duration: allow rounding scaled by it.
        let scale = 1.0 + v0.length() + v1.length() + (p0.length() + p1.length()) / dt;
        assert!((v_start - v0).length() < 1e-9 * scale);
        assert!((v_end - v1).length() < 1e-9 * scale);
    }
}

#[test]
fn hermite_reproduces_a_cubic_polynomial() {
    // x(s) = c0 + c1 s + c2 s^2 + c3 s^3 in seconds is its own Hermite interpolant.
    let c = [
        DVec3::new(1.0, -2.0, 3.0),
        DVec3::new(0.5, 4.0, -1.0),
        DVec3::new(-3.0, 0.25, 2.0),
        DVec3::new(1.5, -0.75, 0.5),
    ];
    let pos = |s: f64| c[0] + c[1] * s + c[2] * (s * s) + c[3] * (s * s * s);
    let vel = |s: f64| c[1] + c[2] * (2.0 * s) + c[3] * (3.0 * s * s);
    let dt = 2.5;
    for k in 0..=20 {
        let t = k as f64 / 20.0;
        let want = pos(t * dt);
        let got = hermite(pos(0.0), vel(0.0), pos(dt), vel(dt), dt, t);
        assert!((got - want).length() < 1e-12, "t={t}");
        let v = hermite_velocity(pos(0.0), vel(0.0), pos(dt), vel(dt), dt, t).unwrap();
        assert!((v - vel(t * dt)).length() < 1e-12, "t={t}");
    }
}

#[test]
fn hermite_velocity_matches_a_finite_difference() {
    let mut rng = Rng(14);
    for _ in 0..200 {
        let (p0, v0, p1, v1) = (rng.vec3(1e3), rng.vec3(10.0), rng.vec3(1e3), rng.vec3(10.0));
        let dt = rng.range(0.5, 4.0);
        let t = rng.range(0.1, 0.9);
        let h = 1e-6;
        let ahead = hermite(p0, v0, p1, v1, dt, t + h);
        let behind = hermite(p0, v0, p1, v1, dt, t - h);
        let numeric = (ahead - behind) / (2.0 * h * dt);
        let exact = hermite_velocity(p0, v0, p1, v1, dt, t).unwrap();
        assert!((numeric - exact).length() < 1e-5 * (1.0 + exact.length()));
    }
}

#[test]
fn hermite_velocity_rejects_a_useless_duration() {
    let z = DVec3::ZERO;
    for dt in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(hermite_velocity(z, z, z, z, dt, 0.5), None);
    }
    // The position curve has no division by the duration and stays defined.
    assert_eq!(hermite(DVec3::X, z, DVec3::Y, z, 0.0, 0.0), DVec3::X);
    assert!(hermite(z, z, z, z, f64::NAN, 0.5).is_nan());
}
