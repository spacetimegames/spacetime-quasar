//! The simulation: ships on Rapier bodies, pilots, the fixed step and the state hash.

use std::collections::BTreeMap;

use rapier3d_f64::prelude::{RigidBody, RigidBodyHandle};
use st_math::det;
use st_math::glam::{DQuat, DVec3};
use st_rng::Stream;
use st_time::{DT, TICK_RATE_HZ, Tick};

use crate::command::{
    CONTROLS_COMPONENT_TAG, Command, CommandKind, ControlState, MAX_COMMANDS_PER_TICK, PilotId,
    StepError, wire,
};
use crate::hash::{DumpLine, HASH_FORMAT_VERSION, HashError, StateHash, StateHasher};
use crate::hull::{FIRE, HullPart, MAX_HULL_PARTS, MassModel, PartShape, Weapon, mass_model};
use crate::physics::{BodyInit, World};

/// Hashes are taken every this many ticks (1 s at 120 Hz), at tick 0 and at
/// the final tick (ADR 0008, "Checkpoints").
pub const CHECKPOINT_INTERVAL: Tick = TICK_RATE_HZ as Tick;

/// The most colliders (one per ship) a system world may hold (ADR 0008).
pub const MAX_SHIPS_PER_SYSTEM: usize = 1_048_576;

/// Axis values are divided by this to get a fraction in `-1.0..=1.0`.
const AXIS_MAX: f64 = 32767.0;

/// Entity component tag of the ship component (ADR 0008, "State hash").
const SHIP_COMPONENT_TAG: u32 = 1;

/// How strongly an AI pilot turns toward its target, in axis fractions per radian
/// of heading error.
const AI_TURN_GAIN: f64 = 3.0;
/// An AI pilot holds fire while the target is within this many radians of dead ahead.
const AI_FIRE_CONE: f64 = 0.05;
/// An AI pilot tries to hold this fraction of its weapon range from the target.
const AI_STANDOFF: f64 = 0.5;
/// Range an AI pilot without a weapon keeps from its target, metres.
const AI_UNARMED_STANDOFF: f64 = 100.0;
/// An AI pilot re-rolls its sideways drift every this many ticks.
const AI_DRIFT_TICKS: Tick = 240;

/// Initial state of one ship.
///
/// [`ShipInit::default`] is a small test craft with a single-box hull and no weapon; a
/// scenario or a data loader fills in the rest. The hull, weapon and hit points are
/// what Rapier and the combat rules turn into one compound collider with explicit mass
/// properties (ADR 0008).
#[derive(Clone, Debug, PartialEq)]
pub struct ShipInit {
    /// Entity id, unique in the setup.
    pub id: u64,
    /// Star system the ship is in. Each system has its own physics world.
    pub system: u64,
    /// Position in the system frame, metres. The gameplay plane is XZ (ADR 0007).
    pub position: DVec3,
    /// Velocity, metres per second.
    pub velocity: DVec3,
    /// Heading in radians: the yaw about +Y, counter-clockwise seen from above,
    /// from the +Z axis toward +X. The ship's nose is its local +Z axis.
    pub heading: f64,
    /// Acceleration at full thrust or strafe, metres per second squared.
    pub max_accel: f64,
    /// Turn rate at full turn, radians per second.
    pub max_turn_rate: f64,
    /// Velocity damping per second (`v *= exp(-drag * dt)` each tick).
    pub drag: f64,
    /// The hull parts, in the ship frame: one to [`MAX_HULL_PARTS`] of them.
    pub hull: Vec<HullPart>,
    /// Whether the body uses continuous collision detection.
    pub ccd: bool,
    /// The side the ship fights for. AI pilots shoot at ships of another team; team 0
    /// has no enemies and is nobody's enemy.
    pub team: u32,
    /// Hit points; the ship is destroyed when they reach zero. Positive and finite.
    pub hit_points: f64,
    /// The ship's weapon, if it has one.
    pub weapon: Option<Weapon>,
}

impl Default for ShipInit {
    fn default() -> Self {
        Self {
            id: 0,
            system: 0,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            heading: 0.0,
            max_accel: 25.0,
            max_turn_rate: 1.5,
            drag: 0.01,
            hull: vec![HullPart {
                shape: PartShape::Cuboid {
                    half_extents: DVec3::new(2.0, 1.0, 4.0),
                },
                offset: DVec3::ZERO,
                rotation: DQuat::IDENTITY,
                density: 100.0,
            }],
            ccd: false,
            team: 0,
            hit_points: 100.0,
            weapon: None,
        }
    }
}

/// The initial state of a simulation: everything except the seed.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    /// The ships, in any order.
    pub ships: Vec<ShipInit>,
    /// Pilots other than the session pilot, which always exists. Each names
    /// the ship it controls, if any. Commands from outside steer these pilots.
    pub pilots: Vec<(PilotId, Option<u64>)>,
    /// AI pilots: each flies the ship it names from inside the simulation, so it takes
    /// no commands (ADR 0009). Ids share one space with `pilots`.
    pub ai_pilots: Vec<(PilotId, u64)>,
    /// Strength of the central attractor at the origin, in m^3/s^2.
    pub gravity_mu: f64,
}

/// A [`Setup`] that cannot start a simulation.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupError {
    /// Two ships have this id.
    DuplicateShip(u64),
    /// Two pilots have this id.
    DuplicatePilot(PilotId),
    /// Pilot 0 is the session and cannot be listed.
    ReservedPilot,
    /// A pilot controls a ship that is not in the setup.
    UnknownShip(u64),
    /// A number in the setup is NaN or infinite.
    NotFinite,
    /// The hull of this ship is empty, has too many parts, or has an invalid part.
    BadHull(u64),
    /// The weapon of this ship has a non-positive or non-finite number.
    BadWeapon(u64),
    /// The hit points of this ship are not positive and finite.
    BadHitPoints(u64),
    /// A system has more than [`MAX_SHIPS_PER_SYSTEM`] ships.
    TooManyShips,
}

impl core::fmt::Display for SetupError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SetupError::DuplicateShip(id) => write!(f, "duplicate ship id {id}"),
            SetupError::DuplicatePilot(p) => write!(f, "duplicate pilot id {}", p.0),
            SetupError::ReservedPilot => f.write_str("pilot 0 is reserved for the session"),
            SetupError::UnknownShip(id) => write!(f, "pilot controls unknown ship {id}"),
            SetupError::NotFinite => f.write_str("a number in the setup is not finite"),
            SetupError::BadHull(id) => write!(f, "the hull of ship {id} is invalid"),
            SetupError::BadWeapon(id) => write!(f, "the weapon of ship {id} is invalid"),
            SetupError::BadHitPoints(id) => write!(f, "the hit points of ship {id} are invalid"),
            SetupError::TooManyShips => {
                write!(f, "a system has more than {MAX_SHIPS_PER_SYSTEM} ships")
            }
        }
    }
}

impl core::error::Error for SetupError {}

#[derive(Clone, Copy, Debug)]
struct WeaponState {
    spec: Weapon,
    /// The first tick at which the weapon may fire again.
    ready_tick: Tick,
}

#[derive(Clone, Debug)]
struct Ship {
    system: u64,
    /// The Rapier body in the system's world; `None` once the ship is destroyed.
    body: Option<RigidBodyHandle>,
    max_accel: f64,
    max_turn_rate: f64,
    drag: f64,
    team: u32,
    hit_points: f64,
    weapon: Option<WeaponState>,
    /// Shots of this ship that hit another ship.
    hits: u64,
}

impl Ship {
    /// The ship's id, system, the ADR 0008 body block and its components.
    fn state_hash_with_id(
        &self,
        id: u64,
        worlds: &BTreeMap<u64, World>,
        h: &mut StateHasher,
    ) -> Result<(), HashError> {
        h.u64(id);
        h.u64(self.system);
        let body = self
            .body
            .and_then(|handle| worlds.get(&self.system)?.bodies.get(handle));
        h.option_tag(body.is_some());
        if let Some(body) = body {
            hash_body(body, h)?;
        }
        // One component: the ship.
        h.count(1);
        h.u32(SHIP_COMPONENT_TAG);
        h.f64(self.max_accel)?;
        h.f64(self.max_turn_rate)?;
        h.f64(self.drag)?;
        h.u32(self.team);
        h.f64(self.hit_points)?;
        h.u64(self.hits);
        h.option_tag(self.weapon.is_some());
        if let Some(w) = &self.weapon {
            h.f64(w.spec.range)?;
            h.f64(w.spec.damage)?;
            h.f64(w.spec.radius)?;
            h.u32(w.spec.cooldown_ticks);
            h.u64(w.ready_tick);
        }
        Ok(())
    }
}

/// The body block of ADR 0008, read through Rapier's API. Rapier's caches are not part of
/// it: a divergence there shows up in the body state at a later checkpoint.
fn hash_body(body: &RigidBody, h: &mut StateHasher) -> Result<(), HashError> {
    h.vec3(body.translation())?;
    h.quat(*body.rotation())?;
    h.vec3(body.linvel())?;
    h.vec3(body.angvel())?;
    h.u8(body.locked_axes().bits());
    h.bool(body.is_sleeping());
    h.bool(body.is_ccd_enabled());
    let m = &body.mass_properties().local_mprops;
    h.f64(m.inv_mass)?;
    h.vec3(m.local_com)?;
    h.vec3(m.inv_principal_inertia)?;
    h.quat(m.principal_inertia_local_frame)
}

#[derive(Clone, Debug)]
struct Pilot {
    /// Whether commands come from outside the simulation. AI pilots are
    /// internal and take no commands (ADR 0009); they are told apart by the
    /// setup, so this is not hashed.
    external: bool,
    controlled: Option<u64>,
    last_seq: u64,
    controls: ControlState,
}

/// The simulation. `Clone` is a snapshot: it copies all simulation state and the
/// full Rapier world of every system, and creates new pipeline workspaces (ADR 0009).
///
/// State advances only through [`Sim::step`], one tick of `st_time::DT` per
/// call. Entities and pilots live in ordered maps, so iteration order is
/// part of the contract. Each star system has its own physics world, stepped in
/// ascending system id order (ADR 0007).
#[derive(Clone, Debug)]
pub struct Sim {
    tick: Tick,
    seed: u64,
    gravity_mu: f64,
    ships: BTreeMap<u64, Ship>,
    pilots: BTreeMap<PilotId, Pilot>,
    worlds: BTreeMap<u64, World>,
}

/// Which size limit a command breaks (ADR 0009, "Validation and stepping").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SizeLimit {
    /// One payload is above 1 MiB.
    Payload,
    /// The tick's commands are above 2 MiB together.
    Tick,
}

/// Adds one command to the byte count of its tick. A command counts its
/// payload plus the largest framing of one command.
fn account_size(tick_bytes: usize, payload_len: usize) -> Result<usize, SizeLimit> {
    if payload_len > wire::MAX_PAYLOAD_BYTES {
        return Err(SizeLimit::Payload);
    }
    match tick_bytes.checked_add(payload_len + wire::FRAME_OVERHEAD) {
        Some(total) if total <= wire::MAX_TICK_BYTES => Ok(total),
        _ => Err(SizeLimit::Tick),
    }
}

fn finite3(v: DVec3) -> bool {
    v.x.is_finite() && v.y.is_finite() && v.z.is_finite()
}

fn positive(x: f64) -> bool {
    x.is_finite() && x > 0.0
}

/// An axis fraction in `-1.0..=1.0` as the quantised command value.
fn quantise(fraction: f64) -> i16 {
    (det::clamp(fraction, -1.0, 1.0) * AXIS_MAX).round() as i16
}

/// What an AI pilot sees of one ship.
#[derive(Clone, Copy)]
struct Contact {
    id: u64,
    system: u64,
    team: u32,
    position: DVec3,
}

impl Sim {
    /// Builds a simulation at tick 0 from a setup and a world seed.
    ///
    /// Pilot 0, the session, exists in every simulation.
    ///
    /// # Errors
    ///
    /// A [`SetupError`] if ids repeat, a pilot names a missing ship, a number is NaN
    /// or infinite, or a hull, weapon or hit-point value is invalid.
    pub fn new(setup: &Setup, seed: u64) -> Result<Sim, SetupError> {
        if !setup.gravity_mu.is_finite() {
            return Err(SetupError::NotFinite);
        }
        let mut ships = BTreeMap::new();
        let mut worlds: BTreeMap<u64, World> = BTreeMap::new();
        // Ships go into their world in ascending id order, so the arena order does not
        // depend on the order of the setup list.
        let mut sorted: Vec<&ShipInit> = setup.ships.iter().collect();
        sorted.sort_by_key(|s| s.id);
        for pair in sorted.windows(2) {
            if pair[0].id == pair[1].id {
                return Err(SetupError::DuplicateShip(pair[0].id));
            }
        }
        for s in sorted {
            let finite = finite3(s.position)
                && finite3(s.velocity)
                && s.heading.is_finite()
                && s.max_accel.is_finite()
                && s.max_turn_rate.is_finite()
                && s.drag.is_finite();
            if !finite {
                return Err(SetupError::NotFinite);
            }
            if !positive(s.hit_points) {
                return Err(SetupError::BadHitPoints(s.id));
            }
            if let Some(w) = &s.weapon
                && !(positive(w.range)
                    && positive(w.damage)
                    && positive(w.radius)
                    && w.cooldown_ticks >= 1)
            {
                return Err(SetupError::BadWeapon(s.id));
            }
            if s.hull.len() > MAX_HULL_PARTS {
                return Err(SetupError::BadHull(s.id));
            }
            let mass: MassModel = mass_model(&s.hull).ok_or(SetupError::BadHull(s.id))?;
            let world = worlds.entry(s.system).or_insert_with(World::new);
            if world.bodies.len() >= MAX_SHIPS_PER_SYSTEM {
                return Err(SetupError::TooManyShips);
            }
            // Heading is a yaw about +Y: the quaternion (0, sin h/2, 0, cos h/2).
            let (sin, cos) = det::sin_cos(0.5 * s.heading);
            let handle = world.add_ship(&BodyInit {
                id: s.id,
                position: s.position,
                rotation: DQuat::from_xyzw(0.0, sin, 0.0, cos),
                velocity: s.velocity,
                ccd: s.ccd,
                parts: &s.hull,
                mass: &mass,
            });
            ships.insert(
                s.id,
                Ship {
                    system: s.system,
                    body: Some(handle),
                    max_accel: s.max_accel,
                    max_turn_rate: s.max_turn_rate,
                    drag: s.drag,
                    team: s.team,
                    hit_points: s.hit_points,
                    weapon: s.weapon.map(|spec| WeaponState {
                        spec,
                        ready_tick: 0,
                    }),
                    hits: 0,
                },
            );
        }
        let mut pilots = BTreeMap::new();
        pilots.insert(
            PilotId::SESSION,
            Pilot {
                external: true,
                controlled: None,
                last_seq: 0,
                controls: ControlState::default(),
            },
        );
        let listed = setup
            .pilots
            .iter()
            .map(|&(id, ship)| (id, ship, true))
            .chain(
                setup
                    .ai_pilots
                    .iter()
                    .map(|&(id, ship)| (id, Some(ship), false)),
            );
        for (id, controlled, external) in listed {
            if id == PilotId::SESSION {
                return Err(SetupError::ReservedPilot);
            }
            if let Some(ship) = controlled
                && !ships.contains_key(&ship)
            {
                return Err(SetupError::UnknownShip(ship));
            }
            let pilot = Pilot {
                external,
                controlled,
                last_seq: 0,
                controls: ControlState::default(),
            };
            if pilots.insert(id, pilot).is_some() {
                return Err(SetupError::DuplicatePilot(id));
            }
        }
        for world in worlds.values_mut() {
            world.prime();
        }
        Ok(Sim {
            tick: 0,
            seed,
            gravity_mu: setup.gravity_mu,
            ships,
            pilots,
            worlds,
        })
    }

    /// The current tick: the number of steps taken so far.
    pub fn tick(&self) -> Tick {
        self.tick
    }

    /// The world seed.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    fn body_of(&self, ship: &Ship) -> Option<&RigidBody> {
        self.worlds.get(&ship.system)?.bodies.get(ship.body?)
    }

    /// A ship's position and velocity, if it exists and is not destroyed. For
    /// tests and tools; the simulation never reads this back.
    pub fn ship_state(&self, id: u64) -> Option<(DVec3, DVec3)> {
        let body = self.body_of(self.ships.get(&id)?)?;
        Some((body.translation(), body.linvel()))
    }

    /// A ship's heading (yaw about +Y, radians in `-pi..=pi`), if it exists and is
    /// not destroyed. For tests and tools.
    pub fn ship_heading(&self, id: u64) -> Option<f64> {
        let body = self.body_of(self.ships.get(&id)?)?;
        let nose = *body.rotation() * DVec3::Z;
        Some(det::atan2(nose.x, nose.z))
    }

    /// A ship's remaining hit points (zero once destroyed), if it exists. For tests
    /// and tools.
    pub fn ship_hit_points(&self, id: u64) -> Option<f64> {
        self.ships.get(&id).map(|s| s.hit_points)
    }

    /// Advances exactly one tick.
    ///
    /// `commands` must all carry [`Sim::tick`] as their tick and be sorted
    /// strictly ascending by (pilot, sequence number); see
    /// [`command::sort`](crate::command::sort). Within the tick, commands
    /// apply first, then AI pilots choose their controls, then weapons fire,
    /// then controls and gravity act on the bodies and every physics world
    /// takes one step.
    ///
    /// Before anything else it checks `st_math::det::fp_env_is_default()` (ADR 0008).
    ///
    /// # Errors
    ///
    /// [`StepError::FloatEnvironment`] if the floating-point environment was
    /// changed. Otherwise a [`StepError`] naming the first offending command. The
    /// whole slice is validated before anything changes, so on error the state is
    /// unchanged.
    pub fn step(&mut self, commands: &[Command]) -> Result<(), StepError> {
        if !det::fp_env_is_default() {
            return Err(StepError::FloatEnvironment);
        }
        self.validate(commands)?;
        for cmd in commands {
            // Validated above: the pilot exists.
            if let Some(pilot) = self.pilots.get_mut(&cmd.pilot) {
                pilot.last_seq = cmd.seq;
                match &cmd.kind {
                    CommandKind::Controls(c) => pilot.controls = *c,
                }
            }
        }
        self.run_ai();
        self.fire_weapons();
        self.apply_controls();
        // Worlds in ascending system id (ADR 0007).
        for world in self.worlds.values_mut() {
            world.step();
        }
        self.tick += 1;
        Ok(())
    }

    fn validate(&self, commands: &[Command]) -> Result<(), StepError> {
        if commands.len() > MAX_COMMANDS_PER_TICK {
            return Err(StepError::TooManyCommands {
                count: commands.len(),
            });
        }
        let mut previous: Option<(PilotId, u64)> = None;
        let mut tick_bytes = 0_usize;
        for (index, cmd) in commands.iter().enumerate() {
            if cmd.tick != self.tick {
                return Err(StepError::WrongTick { index });
            }
            if previous.is_some_and(|p| p >= (cmd.pilot, cmd.seq)) {
                return Err(StepError::NotSorted { index });
            }
            previous = Some((cmd.pilot, cmd.seq));
            let Some(pilot) = self.pilots.get(&cmd.pilot) else {
                return Err(StepError::UnknownPilot { index });
            };
            // Order: source, then who controls the pilot, then the payload.
            match &cmd.kind {
                CommandKind::Controls(_) if cmd.pilot == PilotId::SESSION => {
                    return Err(StepError::WrongSource { index });
                }
                CommandKind::Controls(_) => {}
            }
            if !pilot.external {
                return Err(StepError::NotExternal { index });
            }
            match account_size(tick_bytes, wire::payload_len(&cmd.kind)) {
                Ok(total) => tick_bytes = total,
                Err(SizeLimit::Payload) => return Err(StepError::InvalidPayload { index }),
                Err(SizeLimit::Tick) => return Err(StepError::TickTooLarge { index }),
            }
            match &cmd.kind {
                CommandKind::Controls(c) if !c.is_valid() => {
                    return Err(StepError::InvalidPayload { index });
                }
                CommandKind::Controls(_) => {}
            }
            // Sorted strictly ascending, so checking against the state is
            // enough: later commands of the same pilot have higher numbers.
            if cmd.seq <= pilot.last_seq {
                return Err(StepError::StaleSequence { index });
            }
        }
        Ok(())
    }

    /// The controls that steer each ship: those of the pilot that controls it.
    fn steering(&self) -> BTreeMap<u64, ControlState> {
        let mut steering = BTreeMap::new();
        for pilot in self.pilots.values() {
            if let Some(ship) = pilot.controlled {
                steering.insert(ship, pilot.controls);
            }
        }
        steering
    }

    /// AI pilots, in ascending pilot id, choose controls from the state at the start
    /// of the tick: face and approach the nearest enemy to a standoff distance, drift
    /// sideways (a seeded choice that changes every [`AI_DRIFT_TICKS`] ticks) and fire
    /// while it is ahead and in range.
    fn run_ai(&mut self) {
        if self.pilots.values().all(|p| p.external) {
            return;
        }
        let mut contacts = Vec::new();
        for (&id, ship) in &self.ships {
            if let Some(body) = self.body_of(ship) {
                contacts.push(Contact {
                    id,
                    system: ship.system,
                    team: ship.team,
                    position: body.translation(),
                });
            }
        }
        let drift = Stream::from_seed(self.seed).derive_label("ai-drift");
        let epoch = self.tick / AI_DRIFT_TICKS;
        let mut chosen = Vec::new();
        for (&pilot_id, pilot) in &self.pilots {
            let Some(ship_id) = pilot.controlled.filter(|_| !pilot.external) else {
                continue;
            };
            let Some(ship) = self.ships.get(&ship_id) else {
                continue;
            };
            let Some(body) = self.body_of(ship) else {
                // Destroyed: the controls stay as they were and act on nothing.
                continue;
            };
            let me = body.translation();
            let enemy = contacts
                .iter()
                .filter(|c| {
                    c.system == ship.system && c.team != ship.team && c.team != 0 && ship.team != 0
                })
                .min_by(|a, b| {
                    let da = (a.position - me).length_squared();
                    let db = (b.position - me).length_squared();
                    da.total_cmp(&db).then(a.id.cmp(&b.id))
                });
            let Some(enemy) = enemy else {
                chosen.push((pilot_id, ControlState::default()));
                continue;
            };
            let to = enemy.position - me;
            let distance = to.length();
            let nose = *body.rotation() * DVec3::Z;
            let error = {
                let raw = det::atan2(to.x, to.z) - det::atan2(nose.x, nose.z);
                // Wrap into (-pi, pi].
                raw - core::f64::consts::TAU * (raw / core::f64::consts::TAU).round()
            };
            let range = ship.weapon.map_or(0.0, |w| w.spec.range);
            let standoff = if range > 0.0 {
                AI_STANDOFF * range
            } else {
                AI_UNARMED_STANDOFF
            };
            let thrust = if error.abs() < core::f64::consts::FRAC_PI_2 {
                (distance - standoff) / standoff
            } else {
                0.0
            };
            let sideways = match drift.derive(pilot_id.0).derive(epoch).below_at(0, 3) {
                0 => -0.5,
                1 => 0.5,
                _ => 0.0,
            };
            let fire = range > 0.0 && error.abs() < AI_FIRE_CONE && distance <= range;
            chosen.push((
                pilot_id,
                ControlState {
                    thrust: quantise(thrust),
                    strafe: quantise(sideways),
                    turn: quantise(AI_TURN_GAIN * error),
                    aim: None,
                    buttons: if fire { FIRE } else { 0 },
                },
            ));
        }
        for (id, controls) in chosen {
            if let Some(pilot) = self.pilots.get_mut(&id) {
                pilot.controls = controls;
            }
        }
    }

    /// Weapons of ships whose pilot holds fire sweep a ball along the heading. All
    /// sweeps of a tick see the same state; the damage lands afterwards, in ascending
    /// shooter id, and ships with no hit points left are removed from their world.
    fn fire_weapons(&mut self) {
        let steering = self.steering();
        let tick = self.tick;
        let mut hits: Vec<(u64, u64)> = Vec::new();
        for (&id, ship) in &mut self.ships {
            let Some(weapon) = &mut ship.weapon else {
                continue;
            };
            let held = steering.get(&id).is_some_and(|c| c.buttons & FIRE != 0);
            if !held || tick < weapon.ready_tick {
                continue;
            }
            let Some(world) = self.worlds.get(&ship.system) else {
                continue;
            };
            let Some(handle) = ship.body else {
                continue;
            };
            let Some(body) = world.bodies.get(handle) else {
                continue;
            };
            weapon.ready_tick = tick + Tick::from(weapon.spec.cooldown_ticks);
            let nose = (*body.rotation() * DVec3::Z).normalize();
            if !finite3(nose) {
                continue;
            }
            let spec = weapon.spec;
            if let Some(target) =
                world.sweep(handle, body.translation(), nose, spec.radius, spec.range)
            {
                hits.push((id, target));
            }
        }
        for (shooter, target) in hits {
            let damage = self
                .ships
                .get(&shooter)
                .and_then(|s| s.weapon)
                .map_or(0.0, |w| w.spec.damage);
            if let Some(s) = self.ships.get_mut(&shooter) {
                s.hits += 1;
            }
            if let Some(t) = self.ships.get_mut(&target) {
                t.hit_points -= damage;
            }
        }
        for ship in self.ships.values_mut() {
            if ship.hit_points <= 0.0 {
                ship.hit_points = 0.0;
                if let Some(handle) = ship.body.take()
                    && let Some(world) = self.worlds.get_mut(&ship.system)
                {
                    world.remove_ship(handle);
                }
            }
        }
    }

    /// Thrust, strafe, turn, central gravity and drag, as velocities and impulses on
    /// the bodies, in ascending ship id. Uses only `st_math::det` for transcendental
    /// functions.
    fn apply_controls(&mut self) {
        let steering = self.steering();
        for (id, ship) in &self.ships {
            let Some(handle) = ship.body else {
                continue;
            };
            let Some(body) = self
                .worlds
                .get_mut(&ship.system)
                .and_then(|w| w.bodies.get_mut(handle))
            else {
                continue;
            };
            let controls = steering.get(id).copied().unwrap_or_default();
            let rotation = *body.rotation();
            // Local +Z is the nose and local -X is to the right (ADR 0007).
            let forward = rotation * DVec3::Z;
            let right = rotation * DVec3::NEG_X;
            let position = body.translation();
            let mut accel = forward * (ship.max_accel * (f64::from(controls.thrust) / AXIS_MAX))
                + right * (ship.max_accel * (f64::from(controls.strafe) / AXIS_MAX));
            let r2 = position.length_squared();
            if r2 > 0.0 {
                // Inverse-square pull toward the origin.
                accel -= position * (self.gravity_mu / (r2 * r2.sqrt()));
            }
            let damping = det::exp(-ship.drag * DT);
            let mass = body.mass();
            body.set_linvel(body.linvel() * damping, true);
            body.apply_impulse(accel * (mass * DT), true);
            // Turning is a rate command about +Y: the planar lock holds the other axes.
            let rate = ship.max_turn_rate * (f64::from(controls.turn) / AXIS_MAX);
            body.set_angvel(DVec3::new(0.0, rate, 0.0), true);
        }
    }

    /// The canonical state hash of the whole state (ADR 0008, format
    /// version 1). Excludes anything derived or recomputable.
    ///
    /// # Errors
    ///
    /// [`HashError::NotANumber`] if any state is a NaN.
    pub fn state_hash(&self) -> Result<u64, HashError> {
        let mut h = StateHasher::new();
        self.write_state(&mut h)?;
        Ok(h.finish())
    }

    /// The canonical bytes that [`Sim::state_hash`] folds, one line per
    /// section header and per entity or pilot (the `--dump-tick` input of the
    /// harness, ADR 0008). Two platforms with different hashes can diff these
    /// to find the entity and field that differ.
    ///
    /// # Errors
    ///
    /// [`HashError::NotANumber`] if any state is a NaN.
    pub fn state_dump(&self) -> Result<Vec<DumpLine>, HashError> {
        let mut h = StateHasher::recording();
        self.write_state(&mut h)?;
        Ok(h.into_dump())
    }

    /// The one place that defines the canonical stream, so the hash and the
    /// dump cannot disagree.
    fn write_state(&self, h: &mut StateHasher) -> Result<(), HashError> {
        h.begin_line("STQH", None);
        h.tag(b"STQH");
        h.u32(HASH_FORMAT_VERSION);
        h.u64(self.tick);
        h.u64(self.seed);

        h.begin_line("ENTS", None);
        h.tag(b"ENTS");
        h.count(self.ships.len());
        for (id, ship) in &self.ships {
            h.begin_line("ENTS", Some(*id));
            ship.state_hash_with_id(*id, &self.worlds, h)?;
        }

        h.begin_line("PILT", None);
        h.tag(b"PILT");
        h.count(self.pilots.len());
        for (id, pilot) in &self.pilots {
            h.begin_line("PILT", Some(id.0));
            h.u64(id.0);
            h.option_tag(pilot.controlled.is_some());
            if let Some(ship) = pilot.controlled {
                h.u64(ship);
            }
            h.u64(pilot.last_seq);
            h.count(1);
            h.u32(CONTROLS_COMPONENT_TAG);
            pilot.controls.state_hash(h)?;
        }

        h.begin_line("GLOB", None);
        h.tag(b"GLOB");
        h.f64(self.gravity_mu)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ship_at(id: u64, position: DVec3) -> ShipInit {
        ShipInit {
            id,
            position,
            max_accel: 1.0,
            max_turn_rate: 1.0,
            drag: 0.0,
            ..ShipInit::default()
        }
    }

    fn setup() -> Setup {
        Setup {
            ships: vec![ship_at(1, DVec3::new(10.0, 0.0, 0.0))],
            pilots: vec![(PilotId(1), Some(1))],
            ai_pilots: Vec::new(),
            gravity_mu: 0.0,
        }
    }

    fn command(pilot: u64, seq: u64) -> Command {
        Command {
            tick: 0,
            pilot: PilotId(pilot),
            seq,
            kind: CommandKind::Controls(ControlState::default()),
        }
    }

    fn controls(sim: &Sim, pilot: u64, c: ControlState) -> Command {
        Command {
            tick: sim.tick(),
            pilot: PilotId(pilot),
            seq: sim.tick() + 1,
            kind: CommandKind::Controls(c),
        }
    }

    #[test]
    fn a_pilot_controlled_inside_the_simulation_takes_no_commands() {
        let mut s = setup();
        s.pilots.clear();
        s.ai_pilots.push((PilotId(1), 1));
        let mut sim = Sim::new(&s, 0).unwrap();
        let before = sim.state_hash().unwrap();
        assert_eq!(
            sim.step(&[command(1, 1)]),
            Err(StepError::NotExternal { index: 0 })
        );
        assert_eq!((sim.tick(), sim.state_hash().unwrap()), (0, before));
        // The check is per pilot: the session is still external.
        assert_eq!(
            sim.step(&[command(0, 1)]),
            Err(StepError::WrongSource { index: 0 })
        );
    }

    #[test]
    fn thrust_accelerates_along_the_nose_in_the_xz_plane() {
        let mut sim = Sim::new(&setup(), 0).unwrap();
        let full = ControlState {
            thrust: 32767,
            ..ControlState::default()
        };
        for _ in 0..120 {
            let c = controls(&sim, 1, full);
            sim.step(&[c]).unwrap();
        }
        // Heading 0 is +Z; one second at 1 m/s^2.
        let (p, v) = sim.ship_state(1).unwrap();
        assert!(
            (v.z - 1.0).abs() < 1e-9 && v.x.abs() < 1e-12 && v.y == 0.0,
            "{v:?}"
        );
        assert!(p.z > 0.4 && p.z < 0.6 && p.y == 0.0, "{p:?}");
    }

    #[test]
    fn a_turn_command_yaws_counter_clockwise_about_plus_y() {
        let mut sim = Sim::new(&setup(), 0).unwrap();
        let turn = ControlState {
            turn: 32767,
            ..ControlState::default()
        };
        for _ in 0..120 {
            let c = controls(&sim, 1, turn);
            sim.step(&[c]).unwrap();
        }
        // 1 rad/s for 1 s.
        let heading = sim.ship_heading(1).unwrap();
        assert!((heading - 1.0).abs() < 1e-6, "{heading}");
    }

    #[test]
    fn two_ships_collide_and_bounce_instead_of_passing_through() {
        let mut s = setup();
        s.ships = vec![
            ShipInit {
                velocity: DVec3::new(5.0, 0.0, 0.0),
                ..ship_at(1, DVec3::new(-20.0, 0.0, 0.0))
            },
            ShipInit {
                velocity: DVec3::new(-5.0, 0.0, 0.0),
                ..ship_at(2, DVec3::new(20.0, 0.0, 0.0))
            },
        ];
        s.pilots.clear();
        let mut sim = Sim::new(&s, 0).unwrap();
        for _ in 0..1200 {
            sim.step(&[]).unwrap();
        }
        let (p1, v1) = sim.ship_state(1).unwrap();
        let (p2, _) = sim.ship_state(2).unwrap();
        assert!(
            p1.x < p2.x,
            "the ships passed through each other: {p1:?} {p2:?}"
        );
        assert!(v1.x < 5.0, "ship 1 kept its velocity: {v1:?}");
    }

    #[test]
    fn a_clone_continues_bit_for_bit() {
        let mut s = setup();
        s.ships = (1..=6)
            .map(|i| ShipInit {
                ccd: i % 2 == 0,
                ..ship_at(i, DVec3::new(i as f64 * 6.0, 0.0, 0.0))
            })
            .collect();
        s.pilots = (1..=6).map(|i| (PilotId(i), Some(i))).collect();
        let mut a = Sim::new(&s, 5).unwrap();
        let full = ControlState {
            thrust: 20000,
            turn: 10000,
            ..ControlState::default()
        };
        for _ in 0..200 {
            let cmds: Vec<_> = (1..=6).map(|p| controls(&a, p, full)).collect();
            a.step(&cmds).unwrap();
        }
        let mut b = a.clone();
        for _ in 0..200 {
            let cmds: Vec<_> = (1..=6).map(|p| controls(&a, p, full)).collect();
            a.step(&cmds).unwrap();
            b.step(&cmds).unwrap();
            assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
        }
    }

    #[test]
    fn the_order_of_the_ship_list_does_not_matter() {
        let mut s = setup();
        s.ships = (1..=4)
            .map(|i| ship_at(i, DVec3::new(i as f64 * 5.0, 0.0, 0.0)))
            .collect();
        s.pilots.clear();
        let mut r = s.clone();
        r.ships.reverse();
        let mut a = Sim::new(&s, 1).unwrap();
        let mut b = Sim::new(&r, 1).unwrap();
        for _ in 0..50 {
            a.step(&[]).unwrap();
            b.step(&[]).unwrap();
        }
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
    }

    fn duel() -> Setup {
        let gun = Weapon {
            range: 500.0,
            damage: 10.0,
            radius: 2.0,
            cooldown_ticks: 6,
        };
        let fighter = |id, x, team, heading| ShipInit {
            heading,
            team,
            weapon: Some(gun),
            hit_points: 100.0,
            ..ship_at(id, DVec3::new(x, 0.0, 0.0))
        };
        Setup {
            ships: vec![
                fighter(1, -150.0, 1, core::f64::consts::FRAC_PI_2),
                fighter(2, 150.0, 2, -core::f64::consts::FRAC_PI_2),
            ],
            pilots: Vec::new(),
            ai_pilots: vec![(PilotId(1), 1), (PilotId(2), 2)],
            gravity_mu: 0.0,
        }
    }

    #[test]
    fn ai_pilots_fight_and_a_ship_can_be_destroyed() {
        let mut sim = Sim::new(&duel(), 3).unwrap();
        for _ in 0..1200 {
            sim.step(&[]).unwrap();
        }
        let hp = (
            sim.ship_hit_points(1).unwrap(),
            sim.ship_hit_points(2).unwrap(),
        );
        assert!(hp.0 < 100.0 || hp.1 < 100.0, "nobody was hit: {hp:?}");
        // A destroyed ship has no body and no state to read.
        for id in [1, 2] {
            assert_eq!(
                sim.ship_state(id).is_none(),
                sim.ship_hit_points(id) == Some(0.0)
            );
        }
        let mut again = Sim::new(&duel(), 3).unwrap();
        for _ in 0..1200 {
            again.step(&[]).unwrap();
        }
        assert_eq!(sim.state_hash().unwrap(), again.state_hash().unwrap());
    }

    #[test]
    fn setup_errors() {
        let mut s = setup();
        s.ships[0].hull.clear();
        assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::BadHull(1));
        let mut s = setup();
        s.ships[0].hit_points = 0.0;
        assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::BadHitPoints(1));
        let mut s = setup();
        s.ships[0].weapon = Some(Weapon {
            range: 1.0,
            damage: 1.0,
            radius: 1.0,
            cooldown_ticks: 0,
        });
        assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::BadWeapon(1));
        let mut s = setup();
        s.ships.push(s.ships[0].clone());
        assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::DuplicateShip(1));
        let mut s = setup();
        s.ai_pilots.push((PilotId(1), 1));
        assert_eq!(
            Sim::new(&s, 0).unwrap_err(),
            SetupError::DuplicatePilot(PilotId(1))
        );
        let mut s = setup();
        s.ai_pilots.push((PilotId(9), 77));
        assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::UnknownShip(77));
    }

    #[test]
    fn size_limits() {
        assert_eq!(account_size(0, 15), Ok(55));
        assert_eq!(
            account_size(0, wire::MAX_PAYLOAD_BYTES),
            Ok(wire::MAX_PAYLOAD_BYTES + wire::FRAME_OVERHEAD)
        );
        assert_eq!(
            account_size(0, wire::MAX_PAYLOAD_BYTES + 1),
            Err(SizeLimit::Payload)
        );
        // Two maximal payloads fit in a tick's 2 MiB only without the framing.
        let one = account_size(0, wire::MAX_PAYLOAD_BYTES).unwrap();
        assert_eq!(
            account_size(one, wire::MAX_PAYLOAD_BYTES),
            Err(SizeLimit::Tick)
        );
        // The last command that fits, and the one that does not.
        assert_eq!(
            account_size(wire::MAX_TICK_BYTES - 55, 15),
            Ok(wire::MAX_TICK_BYTES)
        );
        assert_eq!(
            account_size(wire::MAX_TICK_BYTES - 54, 15),
            Err(SizeLimit::Tick)
        );
        assert_eq!(account_size(usize::MAX, 1), Err(SizeLimit::Tick));
    }
}
