//! Ship hulls, weapons and the mass model.
//!
//! A hull is a list of parts. `Sim` turns it into one compound collider with explicit mass
//! properties (ADR 0008, "Physics configuration"), so Rapier and Parry never add or
//! diagonalise mass properties themselves. The mass model here is closed-form: each part's
//! mass and inertia tensor, rotated into the ship frame, summed about the common centre
//! of mass by the parallel-axis theorem, then diagonalised by
//! [`st_math::eigen::symmetric_eigen3`].
//!
//! Nothing here names a Rapier or Parry type.

use st_math::eigen::symmetric_eigen3;
use st_math::glam::{DMat3, DQuat, DVec3};

/// The most parts one hull may have.
pub const MAX_HULL_PARTS: usize = 16;

/// The convex shape of one hull part, in the part's own frame.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PartShape {
    /// A box with these half extents along the part's x, y and z axes, in metres.
    Cuboid {
        /// Half extents.
        half_extents: DVec3,
    },
    /// A sphere.
    Ball {
        /// Radius in metres.
        radius: f64,
    },
    /// A cylinder around the part's y axis.
    Cylinder {
        /// Half the length along y, in metres.
        half_height: f64,
        /// Radius in metres.
        radius: f64,
    },
}

/// One part of a hull.
///
/// ```
/// use st_math::glam::{DQuat, DVec3};
/// use st_sim::{HullPart, PartShape, ShipInit, Sim, Setup};
///
/// // A ship made of a box and a ball: the simulation computes mass, centre of mass and
/// // principal inertia from the parts and builds one compound collider.
/// let hull = vec![
///     HullPart {
///         shape: PartShape::Cuboid { half_extents: DVec3::new(1.0, 0.5, 2.0) },
///         offset: DVec3::ZERO,
///         rotation: DQuat::IDENTITY,
///         density: 100.0,
///     },
///     HullPart {
///         shape: PartShape::Ball { radius: 0.8 },
///         offset: DVec3::new(0.0, 0.0, 2.0),
///         rotation: DQuat::IDENTITY,
///         density: 50.0,
///     },
/// ];
/// let setup = Setup {
///     ships: vec![ShipInit { id: 1, hull, ..ShipInit::default() }],
///     pilots: Vec::new(),
///     ai_pilots: Vec::new(),
///     gravity_mu: 0.0,
/// };
/// assert!(Sim::new(&setup, 0).is_ok());
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HullPart {
    /// The part's shape.
    pub shape: PartShape,
    /// The part's centre in the ship frame, metres.
    pub offset: DVec3,
    /// The part's orientation in the ship frame; must be a unit quaternion.
    pub rotation: DQuat,
    /// Mass density in kg per cubic metre.
    pub density: f64,
}

/// A weapon: an instant sweep of a ball along the ship's heading.
///
/// While the pilot holds the primary button (bit 0 of [`ControlState::buttons`](crate::ControlState)),
/// the weapon fires whenever it is ready: it sweeps a ball of `radius` metres from the
/// ship's centre along its heading for `range` metres, and the first other ship it touches loses
/// `damage` hit points. Then it is not ready for `cooldown_ticks` ticks.
///
/// ```
/// use st_math::glam::DVec3;
/// use st_sim::{Command, CommandKind, ControlState, FIRE, PilotId, Setup, ShipInit, Sim, Weapon};
///
/// // Ship 1 fires down its nose (+Z) at ship 2, 100 m ahead.
/// let gun = Weapon { range: 200.0, damage: 10.0, radius: 1.0, cooldown_ticks: 10 };
/// let shooter = ShipInit { id: 1, weapon: Some(gun), ..ShipInit::default() };
/// let target = ShipInit { id: 2, position: DVec3::new(0.0, 0.0, 100.0), ..ShipInit::default() };
/// let setup = Setup {
///     ships: vec![shooter, target],
///     pilots: vec![(PilotId(1), Some(1))],
///     ai_pilots: Vec::new(),
///     gravity_mu: 0.0,
/// };
/// let mut sim = Sim::new(&setup, 1)?;
/// let hold = Command {
///     tick: 0,
///     pilot: PilotId(1),
///     seq: 1,
///     kind: CommandKind::Controls(ControlState { buttons: FIRE, ..ControlState::default() }),
/// };
/// sim.step(&[hold])?;
/// assert_eq!(sim.ship_hit_points(2), Some(90.0));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weapon {
    /// How far the sweep reaches, metres.
    pub range: f64,
    /// Hit points taken from the ship that is hit.
    pub damage: f64,
    /// Radius of the swept ball, metres.
    pub radius: f64,
    /// Ticks between shots; at least 1.
    pub cooldown_ticks: u32,
}

/// The button bit that fires a ship's weapon (the primary button of ADR 0009).
///
/// ```
/// assert_eq!(st_sim::FIRE & 1, 1);
/// ```
pub const FIRE: u32 = 1;

/// Mass, centre of mass and principal inertia of a hull, in the ship frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MassModel {
    pub mass: f64,
    pub local_com: DVec3,
    pub principal_inertia: DVec3,
    pub principal_frame: DQuat,
}

impl PartShape {
    pub(crate) fn is_valid(&self) -> bool {
        let ok = |x: f64| x.is_finite() && x > 0.0;
        match *self {
            PartShape::Cuboid { half_extents } => {
                ok(half_extents.x) && ok(half_extents.y) && ok(half_extents.z)
            }
            PartShape::Ball { radius } => ok(radius),
            PartShape::Cylinder {
                half_height,
                radius,
            } => ok(half_height) && ok(radius),
        }
    }

    /// Volume in cubic metres. `pi` is a literal, so this uses no libm.
    fn volume(&self) -> f64 {
        match *self {
            PartShape::Cuboid { half_extents: h } => 8.0 * h.x * h.y * h.z,
            PartShape::Ball { radius } => {
                4.0 / 3.0 * core::f64::consts::PI * radius * radius * radius
            }
            PartShape::Cylinder {
                half_height,
                radius,
            } => core::f64::consts::PI * radius * radius * 2.0 * half_height,
        }
    }

    /// The diagonal inertia tensor about the part's own centre, for unit mass.
    fn unit_inertia(&self) -> DVec3 {
        match *self {
            PartShape::Cuboid { half_extents: h } => DVec3::new(
                (h.y * h.y + h.z * h.z) / 3.0,
                (h.x * h.x + h.z * h.z) / 3.0,
                (h.x * h.x + h.y * h.y) / 3.0,
            ),
            PartShape::Ball { radius } => DVec3::splat(0.4 * radius * radius),
            PartShape::Cylinder {
                half_height,
                radius,
            } => {
                let lateral = (3.0 * radius * radius + 4.0 * half_height * half_height) / 12.0;
                DVec3::new(lateral, 0.5 * radius * radius, lateral)
            }
        }
    }
}

impl HullPart {
    pub(crate) fn is_valid(&self) -> bool {
        let q = self.rotation;
        let len2 = q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w;
        self.shape.is_valid()
            && self.offset.is_finite()
            && self.density.is_finite()
            && self.density > 0.0
            && q.is_finite()
            && (len2 - 1.0).abs() < 1.0e-9
    }

    fn mass(&self) -> f64 {
        self.density * self.shape.volume()
    }
}

/// Computes the mass model of a hull, or `None` if it is empty, has too many parts, has
/// an invalid part or overflows.
pub(crate) fn mass_model(parts: &[HullPart]) -> Option<MassModel> {
    if parts.is_empty() || parts.len() > MAX_HULL_PARTS || !parts.iter().all(HullPart::is_valid) {
        return None;
    }
    // Parts in order: the sums are fixed-order, so the rounding is too.
    let mut mass = 0.0;
    let mut weighted = DVec3::ZERO;
    for p in parts {
        let m = p.mass();
        mass += m;
        weighted += p.offset * m;
    }
    if !(mass.is_finite() && mass > 0.0) {
        return None;
    }
    let com = weighted / mass;

    let mut tensor = DMat3::ZERO;
    for p in parts {
        let m = p.mass();
        // Inertia about the part's centre, rotated into the ship frame.
        let rot = DMat3::from_quat(p.rotation);
        let own = rot * DMat3::from_diagonal(p.shape.unit_inertia() * m) * rot.transpose();
        // Parallel axis: I += m (|d|^2 E - d d^T), d from the common centre of mass.
        let d = p.offset - com;
        let shift = (DMat3::IDENTITY * d.length_squared()) - outer(d, d);
        tensor += own + shift * m;
    }
    let eigen = symmetric_eigen3(&tensor)?;
    if eigen.values.x <= 0.0 {
        return None;
    }
    // `from_mat3` of a proper rotation is a unit quaternion; renormalise to remove the
    // last bits of drift so Rapier gets a unit rotation.
    let principal_frame = DQuat::from_mat3(&eigen.vectors).normalize();
    principal_frame.is_finite().then_some(MassModel {
        mass,
        local_com: com,
        principal_inertia: eigen.values,
        principal_frame,
    })
}

fn outer(a: DVec3, b: DVec3) -> DMat3 {
    DMat3::from_cols(a * b.x, a * b.y, a * b.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cuboid(h: DVec3, offset: DVec3, density: f64) -> HullPart {
        HullPart {
            shape: PartShape::Cuboid { half_extents: h },
            offset,
            rotation: DQuat::IDENTITY,
            density,
        }
    }

    #[test]
    fn a_cuboid_has_the_textbook_inertia() {
        let m = mass_model(&[cuboid(DVec3::new(1.0, 2.0, 3.0), DVec3::ZERO, 10.0)]).unwrap();
        // Volume 48 m^3, mass 480 kg, I_x = m (w^2 + h^2) / 12 with full extents 2, 4, 6.
        assert_eq!(m.mass, 480.0);
        let mut inertia = [
            480.0 * (16.0 + 36.0) / 12.0,
            480.0 * (4.0 + 36.0) / 12.0,
            480.0 * (4.0 + 16.0) / 12.0,
        ];
        inertia.sort_by(f64::total_cmp);
        for (got, want) in m.principal_inertia.to_array().iter().zip(inertia) {
            assert!((got - want).abs() < 1e-9 * want, "{got} {want}");
        }
    }

    #[test]
    fn two_offset_parts_move_the_centre_and_add_the_parallel_axis_term() {
        let a = cuboid(DVec3::splat(1.0), DVec3::new(-5.0, 0.0, 0.0), 1.0);
        let b = cuboid(DVec3::splat(1.0), DVec3::new(5.0, 0.0, 0.0), 1.0);
        let m = mass_model(&[a, b]).unwrap();
        assert_eq!(m.local_com, DVec3::ZERO);
        assert_eq!(m.mass, 16.0);
        // Each cube: m = 8, own I = m (2^2 + 2^2) / 12 = 16/3 about every axis; two of them put
        // 8 * 25 = 200 each about the axes perpendicular to x.
        let own = 16.0 / 3.0;
        let mut want = [2.0 * own, 2.0 * own + 400.0, 2.0 * own + 400.0];
        want.sort_by(f64::total_cmp);
        for (got, want) in m.principal_inertia.to_array().iter().zip(want) {
            assert!((got - want).abs() < 1e-9 * want, "{got} {want}");
        }
    }

    #[test]
    fn rotated_parts_still_give_a_proper_principal_frame() {
        let (s, c) = st_math::det::sin_cos(0.3);
        let tilt = DQuat::from_xyzw(0.0, 0.0, s, c).normalize();
        let part = HullPart {
            rotation: tilt,
            ..cuboid(DVec3::new(1.0, 3.0, 2.0), DVec3::new(0.0, 1.0, 0.0), 2.0)
        };
        let m = mass_model(&[
            part,
            cuboid(DVec3::splat(0.5), DVec3::new(2.0, 0.0, 1.0), 4.0),
        ])
        .unwrap();
        assert!((m.principal_frame.length() - 1.0).abs() < 1e-15);
        assert!(m.principal_inertia.x <= m.principal_inertia.y);
        assert!(m.principal_inertia.y <= m.principal_inertia.z);
    }

    #[test]
    fn invalid_hulls_are_none() {
        let ok = cuboid(DVec3::ONE, DVec3::ZERO, 1.0);
        assert!(mass_model(&[]).is_none());
        assert!(mass_model(&[HullPart { density: 0.0, ..ok }]).is_none());
        assert!(
            mass_model(&[HullPart {
                density: f64::NAN,
                ..ok
            }])
            .is_none()
        );
        assert!(
            mass_model(&[HullPart {
                offset: DVec3::new(f64::INFINITY, 0.0, 0.0),
                ..ok
            }])
            .is_none()
        );
        assert!(
            mass_model(&[HullPart {
                rotation: DQuat::from_xyzw(0.0, 0.0, 0.0, 2.0),
                ..ok
            }])
            .is_none()
        );
        let flat = HullPart {
            shape: PartShape::Ball { radius: -1.0 },
            ..ok
        };
        assert!(mass_model(&[flat]).is_none());
        assert!(mass_model(&[ok; MAX_HULL_PARTS + 1]).is_none());
        assert!(mass_model(&[ok; MAX_HULL_PARTS]).is_some());
    }
}
