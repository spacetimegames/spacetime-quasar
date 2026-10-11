//! One Rapier world per star system (ADR 0007), configured by ADR 0008.
//!
//! Private to the crate: Rapier and Parry types never appear in a public signature of
//! `st-sim`. The rules this module keeps:
//!
//! * one collider per body, at the body origin, a compound of the hull parts with explicit
//!   mass properties (so Rapier never adds or diagonalises mass properties);
//! * the planar [`LockedAxes`] of ADR 0007;
//! * no joints, multibodies, soft bodies, hooks or events, and no second thread;
//! * the integration step is exactly `st_time::DT`.

use rapier3d_f64::math::{Pose, Vector};
use rapier3d_f64::parry::mass_properties::MassProperties;
use rapier3d_f64::parry::query::ShapeCastOptions;
use rapier3d_f64::parry::shape::{Ball, SharedShape};
use rapier3d_f64::prelude::{
    BroadPhaseBvh, CCDSolver, ColliderBuilder, ColliderSet, ImpulseJointSet, IntegrationParameters,
    IslandManager, LockedAxes, MultibodyJointSet, NarrowPhase, PhysicsPipeline, QueryFilter,
    RigidBodyBuilder, RigidBodyHandle, RigidBodySet, SoftBodySet,
};
use st_math::glam::{DQuat, DVec3};
use st_time::DT;

use crate::hull::{HullPart, MassModel, PartShape};

/// The planar constraint of ADR 0007: free motion in the XZ plane and yaw only.
pub(crate) fn planar() -> LockedAxes {
    LockedAxes::TRANSLATION_LOCKED_Y | LockedAxes::ROTATION_LOCKED_X | LockedAxes::ROTATION_LOCKED_Z
}

/// What a world needs to create one ship body.
pub(crate) struct BodyInit<'a> {
    /// Entity id, stored as the collider's user data to map a hit back to a ship.
    pub id: u64,
    pub position: DVec3,
    pub rotation: DQuat,
    pub velocity: DVec3,
    pub ccd: bool,
    pub parts: &'a [HullPart],
    pub mass: &'a MassModel,
}

/// A physics world: everything `PhysicsPipeline::step` takes.
pub(crate) struct World {
    pub parameters: IntegrationParameters,
    pub islands: IslandManager,
    pub broad_phase: BroadPhaseBvh,
    pub narrow_phase: NarrowPhase,
    pub bodies: RigidBodySet,
    pub colliders: ColliderSet,
    pub impulse_joints: ImpulseJointSet,
    pub multibody_joints: MultibodyJointSet,
    pub soft_bodies: SoftBodySet,
    pub ccd_solver: CCDSolver,
    /// Holds only workspace buffers and is not `Clone`; a clone gets a new one
    /// (verified to continue bit for bit, ADR 0009).
    pipeline: PhysicsPipeline,
}

impl Clone for World {
    fn clone(&self) -> Self {
        Self {
            parameters: self.parameters,
            islands: self.islands.clone(),
            broad_phase: self.broad_phase.clone(),
            narrow_phase: self.narrow_phase.clone(),
            bodies: self.bodies.clone(),
            colliders: self.colliders.clone(),
            impulse_joints: self.impulse_joints.clone(),
            multibody_joints: self.multibody_joints.clone(),
            soft_bodies: self.soft_bodies.clone(),
            ccd_solver: self.ccd_solver.clone(),
            pipeline: PhysicsPipeline::new(),
        }
    }
}

impl core::fmt::Debug for World {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("World")
            .field("bodies", &self.bodies.len())
            .field("colliders", &self.colliders.len())
            .finish_non_exhaustive()
    }
}

fn shape_of(part: &HullPart) -> SharedShape {
    match part.shape {
        PartShape::Cuboid { half_extents: h } => SharedShape::cuboid(h.x, h.y, h.z),
        PartShape::Ball { radius } => SharedShape::ball(radius),
        PartShape::Cylinder {
            half_height,
            radius,
        } => SharedShape::cylinder(half_height, radius),
    }
}

impl World {
    /// An empty world. `max_speed_mps` is the fastest any body may go (Rapier's default
    /// of 400 m/s would cap normal flight); the caller has validated it as finite and
    /// positive.
    pub(crate) fn new(max_speed_mps: f64) -> Self {
        Self {
            parameters: IntegrationParameters {
                dt: DT,
                normalized_max_linear_velocity: max_speed_mps,
                ..IntegrationParameters::default()
            },
            islands: IslandManager::new(),
            broad_phase: BroadPhaseBvh::new(),
            narrow_phase: NarrowPhase::new(),
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            soft_bodies: SoftBodySet::new(),
            ccd_solver: CCDSolver::new(),
            pipeline: PhysicsPipeline::new(),
        }
    }

    /// Inserts a dynamic ship body with its single compound collider.
    ///
    /// Ships never sleep: gravity acts on them every tick, and a body that is asleep
    /// has to be woken by an impulse anyway.
    pub(crate) fn add_ship(&mut self, init: &BodyInit<'_>) -> RigidBodyHandle {
        let body = RigidBodyBuilder::dynamic()
            .pose(Pose::from_parts(init.position, init.rotation))
            .linvel(init.velocity)
            .locked_axes(planar())
            .ccd_enabled(init.ccd)
            .can_sleep(false)
            // Planar flight is yaw about +Y only. Gyroscopic torque from a hull whose
            // principal axes are not the world axes would tilt it out of the plane
            // and push its origin off y = 0.
            .gyroscopic_forces_enabled(false);
        let handle = self.bodies.insert(body);
        let parts = init
            .parts
            .iter()
            .map(|p| (Pose::from_parts(p.offset, p.rotation), shape_of(p)))
            .collect();
        let m = init.mass;
        let mass_properties = MassProperties::with_principal_inertia_frame(
            m.local_com,
            m.mass,
            m.principal_inertia,
            m.principal_frame,
        );
        let collider = ColliderBuilder::compound(parts)
            .mass_properties(mass_properties)
            .user_data(u128::from(init.id));
        self.colliders
            .insert_with_parent(collider, handle, &mut self.bodies);
        handle
    }

    /// Brings the broad phase up to date with a step of zero length, which Rapier defines as
    /// applying user changes and updating collision detection only. Without it the first
    /// tick's weapon sweeps would see an empty world.
    pub(crate) fn prime(&mut self) {
        let dt = self.parameters.dt;
        self.parameters.dt = 0.0;
        self.step();
        self.parameters.dt = dt;
    }

    /// Removes a body and its collider.
    pub(crate) fn remove_ship(&mut self, handle: RigidBodyHandle) {
        self.bodies.remove(
            handle,
            &mut self.islands,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.soft_bodies,
            true,
        );
    }

    /// Advances the world by one step of `DT`, with no gravity of its own (the
    /// simulation applies gravity as impulses) and no hooks or events.
    pub(crate) fn step(&mut self) {
        self.pipeline.step(
            Vector::ZERO,
            &self.parameters,
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.soft_bodies,
            &mut self.ccd_solver,
            &(),
            &(),
        );
    }

    /// Sweeps a ball from `origin` along the unit vector `direction` for `range` metres
    /// and returns the entity id of the first ship it touches, skipping `shooter`.
    ///
    /// The broad phase holds the colliders as of the last step, so a sweep sees the world
    /// one tick stale at worst; that is deterministic like everything else.
    pub(crate) fn sweep(
        &self,
        shooter: RigidBodyHandle,
        origin: DVec3,
        direction: DVec3,
        radius: f64,
        range: f64,
    ) -> Option<u64> {
        let query = self.broad_phase.as_query_pipeline(
            self.narrow_phase.query_dispatcher(),
            &self.bodies,
            &self.colliders,
            QueryFilter::new().exclude_rigid_body(shooter),
        );
        let ball = Ball::new(radius);
        let (collider, _hit) = query.cast_shape(
            &Pose::from_parts(origin, DQuat::IDENTITY),
            direction,
            &ball,
            ShapeCastOptions::with_max_time_of_impact(range),
        )?;
        let target = self.colliders.get(collider)?;
        // The id was stored from a `u64`.
        u64::try_from(target.user_data).ok()
    }
}
