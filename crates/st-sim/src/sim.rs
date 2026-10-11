//! The simulation: ships on Rapier bodies, pilots, the fixed step and the state hash.

use std::collections::BTreeMap;

use rapier3d_f64::prelude::{RigidBody, RigidBodyHandle};
use st_math::det;
use st_math::glam::{DQuat, DVec3};
use st_time::{DT, TICK_RATE_HZ, Tick};

use crate::command::{
    CONTROLS_COMPONENT_TAG, Command, CommandKind, ControlState, MAX_COMMANDS_PER_TICK, PilotId,
    StepError, wire,
};
use crate::hash::{DumpLine, HASH_FORMAT_VERSION, HashError, StateHash, StateHasher};
use crate::hull::{HullPart, MAX_HULL_PARTS, MassModel, PartShape, Weapon, mass_model};
use crate::physics::{BodyInit, World};

mod combat;

/// Hashes are taken every this many ticks (1 s at 120 Hz), at tick 0 and at
/// the final tick (ADR 0008, "Checkpoints").
///
/// ```
/// assert_eq!(st_sim::CHECKPOINT_INTERVAL, 120);
/// ```
pub const CHECKPOINT_INTERVAL: Tick = TICK_RATE_HZ as Tick;

/// The most colliders (one per ship) a system world may hold (ADR 0008).
///
/// ```
/// assert_eq!(st_sim::MAX_SHIPS_PER_SYSTEM, 1 << 20);
/// ```
pub const MAX_SHIPS_PER_SYSTEM: usize = 1_048_576;

/// The largest weapon range or radius `Sim::new` accepts, in metres: 10^12 m, about 6.7 AU.
/// A numeric-safety bound, not a design value: Parry's shape casts panic on degenerate
/// geometry (a ball of radius 10^300), far beyond any real distance.
const MAX_WEAPON_EXTENT: f64 = 1.0e12;

/// Axis values are divided by this to get a fraction in `-1.0..=1.0`.
const AXIS_MAX: f64 = 32767.0;

/// Entity component tag of the ship component (ADR 0008, "State hash").
const SHIP_COMPONENT_TAG: u32 = 1;

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
    ///
    /// **Provisional determinism fixture (SPAC-128), not the PD-28 combat rules.** Phase 2
    /// replaces it with the PD-28 rules (beam and bolt prototypes, shields, armour facings,
    /// sections); that change bumps `SIM_VERSION`.
    pub team: u32,
    /// Hit points; the ship is destroyed when they reach zero. Positive and finite.
    ///
    /// **Provisional determinism fixture (SPAC-128), not the PD-28 combat rules.** Phase 2
    /// replaces it with the PD-28 rules (beam and bolt prototypes, shields, armour facings,
    /// sections); that change bumps `SIM_VERSION`.
    pub hit_points: f64,
    /// The ship's weapon, if it has one.
    ///
    /// **Provisional determinism fixture (SPAC-128), not the PD-28 combat rules.** Phase 2
    /// replaces it with the PD-28 rules (beam and bolt prototypes, shields, armour facings,
    /// sections); that change bumps `SIM_VERSION`.
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
    ///
    /// **Provisional determinism fixture (SPAC-128), not the PD-28 combat rules.** Phase 2
    /// replaces it with the PD-28 rules (beam and bolt prototypes, shields, armour facings,
    /// sections); that change bumps `SIM_VERSION`.
    pub ai_pilots: Vec<(PilotId, u64)>,
    /// Strength of the central attractor at the origin, in m^3/s^2.
    pub gravity_mu: f64,
    /// The fastest any body may go, in metres per second: Rapier's
    /// `normalized_max_linear_velocity`, whose 400 m/s default would cap normal flight.
    /// Finite and positive. Whoever builds the setup derives it from data (PD-31 for the
    /// game); the simulation holds no speed constant.
    pub max_speed_mps: f64,
    /// The numbers the AI pilots fly by, one set per setup.
    pub ai: AiParams,
}

/// The tuning numbers of the AI pilots of one [`Setup`].
///
/// **Provisional determinism fixture (SPAC-128), not the PD-28 combat rules.** Phase 2
/// replaces it with the PD-28 rules (beam and bolt prototypes, shields, armour facings,
/// sections); that change bumps `SIM_VERSION`. Nothing in `assets/`, `st-game` or `apps/`
/// uses it as content before then.
///
/// The step holds no tuning numbers of its own; the scenario or test that builds the
/// [`Setup`] supplies these. [`Sim::new`] validates them and they are part of the setup
/// encoding.
///
/// ```
/// use st_sim::AiParams;
/// let ai = AiParams {
///     turn_gain: 3.0,
///     fire_cone: 0.05,
///     standoff: 0.5,
///     unarmed_standoff: 100.0,
///     drift_ticks: 240,
/// };
/// assert!(ai.drift_ticks >= 1);
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AiParams {
    /// How strongly a pilot turns toward its target, in axis fractions per radian of
    /// heading error. Finite and positive.
    pub turn_gain: f64,
    /// A pilot holds fire while the target is within this many radians of dead ahead.
    /// Finite and positive.
    pub fire_cone: f64,
    /// A pilot tries to hold this fraction of its weapon range from the target. Finite
    /// and positive.
    pub standoff: f64,
    /// The range, in metres, a pilot without a weapon keeps from its target. Finite and
    /// positive.
    pub unarmed_standoff: f64,
    /// A pilot re-rolls its sideways drift every this many ticks. At least 1.
    pub drift_ticks: Tick,
}

impl AiParams {
    /// Whether every number is in its range.
    fn is_valid(&self) -> bool {
        positive(self.turn_gain)
            && positive(self.fire_cone)
            && positive(self.standoff)
            && positive(self.unarmed_standoff)
            && self.drift_ticks >= 1
    }
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
    /// The weapon of this ship has a non-positive or non-finite number, or a range or radius
    /// above 10^12 m.
    BadWeapon(u64),
    /// The hit points of this ship are not positive and finite.
    BadHitPoints(u64),
    /// A system has more than [`MAX_SHIPS_PER_SYSTEM`] ships.
    TooManyShips,
    /// The acceleration, turn rate or drag of this ship is negative. A negative drag
    /// would make the velocity damping grow without bound.
    BadDynamics(u64),
    /// [`Setup::max_speed_mps`] is not finite and positive.
    BadMaxSpeed,
    /// A number of [`Setup::ai`] is not finite and positive (`drift_ticks` below 1).
    BadAiParams,
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
            SetupError::BadDynamics(id) => {
                write!(
                    f,
                    "ship {id} has a negative acceleration, turn rate or drag"
                )
            }
            SetupError::BadMaxSpeed => f.write_str("the maximum speed is not finite and positive"),
            SetupError::BadAiParams => f.write_str("an AI parameter is out of range"),
        }
    }
}

impl core::error::Error for SetupError {}

/// A weapon and its cooldown. Part of the provisional determinism fixture of [`combat`].
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
    max_speed_mps: f64,
    ai: AiParams,
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

impl Sim {
    /// Builds a simulation at tick 0 from a setup and a world seed.
    ///
    /// Pilot 0, the session, exists in every simulation.
    ///
    /// # Errors
    ///
    /// A [`SetupError`] if ids repeat, a pilot names a missing ship, a number is NaN
    /// or infinite, a ship has a negative acceleration, turn rate or drag, the maximum
    /// speed or an AI parameter is out of range, or a hull, weapon or hit-point value is
    /// invalid. No setup that passes is known to make [`Sim::step`] panic.
    pub fn new(setup: &Setup, seed: u64) -> Result<Sim, SetupError> {
        if !setup.gravity_mu.is_finite() {
            return Err(SetupError::NotFinite);
        }
        if !positive(setup.max_speed_mps) {
            return Err(SetupError::BadMaxSpeed);
        }
        if !setup.ai.is_valid() {
            return Err(SetupError::BadAiParams);
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
            if s.max_accel < 0.0 || s.max_turn_rate < 0.0 || s.drag < 0.0 {
                return Err(SetupError::BadDynamics(s.id));
            }
            if !positive(s.hit_points) {
                return Err(SetupError::BadHitPoints(s.id));
            }
            if let Some(w) = &s.weapon
                && !(positive(w.range)
                    && w.range <= MAX_WEAPON_EXTENT
                    && positive(w.damage)
                    && positive(w.radius)
                    && w.radius <= MAX_WEAPON_EXTENT
                    && w.cooldown_ticks >= 1)
            {
                return Err(SetupError::BadWeapon(s.id));
            }
            if s.hull.len() > MAX_HULL_PARTS {
                return Err(SetupError::BadHull(s.id));
            }
            let mass: MassModel = mass_model(&s.hull).ok_or(SetupError::BadHull(s.id))?;
            let world = worlds
                .entry(s.system)
                .or_insert_with(|| World::new(setup.max_speed_mps));
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
            max_speed_mps: setup.max_speed_mps,
            ai: setup.ai,
            ships,
            pilots,
            worlds,
        })
    }

    /// The current tick: the number of steps taken so far.
    ///
    /// ```
    /// use st_sim::scenarios;
    /// let s = scenarios::solo_burn();
    /// let mut sim = st_sim::Sim::new(&s.setup, s.seed)?;
    /// assert_eq!(sim.tick(), 0);
    /// sim.step(&[])?;
    /// assert_eq!(sim.tick(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn tick(&self) -> Tick {
        self.tick
    }

    /// The world seed.
    ///
    /// ```
    /// use st_sim::scenarios;
    /// let s = scenarios::solo_burn();
    /// let sim = st_sim::Sim::new(&s.setup, 99)?;
    /// assert_eq!(sim.seed(), 99);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn seed(&self) -> u64 {
        self.seed
    }

    fn body_of(&self, ship: &Ship) -> Option<&RigidBody> {
        self.worlds.get(&ship.system)?.bodies.get(ship.body?)
    }

    /// A ship's position and velocity, if it exists and is not destroyed. For
    /// tests and tools; the simulation never reads this back.
    ///
    /// ```
    /// use st_math::glam::DVec3;
    /// use st_sim::{Setup, ShipInit, Sim};
    /// # use st_sim::AiParams;
    /// # let ai = AiParams { turn_gain: 3.0, fire_cone: 0.05, standoff: 0.5, unarmed_standoff: 100.0, drift_ticks: 240 };
    /// let ship = ShipInit { id: 1, position: DVec3::new(5.0, 0.0, 0.0), ..ShipInit::default() };
    /// let setup = Setup { ships: vec![ship], pilots: Vec::new(), ai_pilots: Vec::new(), gravity_mu: 0.0, max_speed_mps: 20_000.0, ai };
    /// let sim = Sim::new(&setup, 0)?;
    /// let (position, velocity) = sim.ship_state(1).expect("the ship exists");
    /// assert_eq!(position, DVec3::new(5.0, 0.0, 0.0));
    /// assert_eq!(velocity, DVec3::ZERO);
    /// assert!(sim.ship_state(2).is_none());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn ship_state(&self, id: u64) -> Option<(DVec3, DVec3)> {
        let body = self.body_of(self.ships.get(&id)?)?;
        Some((body.translation(), body.linvel()))
    }

    /// A ship's heading (yaw about +Y, radians in `-pi..=pi`), if it exists and is
    /// not destroyed. For tests and tools.
    ///
    /// ```
    /// use st_math::glam::DVec3;
    /// use st_sim::{Setup, ShipInit, Sim};
    /// # use st_sim::AiParams;
    /// # let ai = AiParams { turn_gain: 3.0, fire_cone: 0.05, standoff: 0.5, unarmed_standoff: 100.0, drift_ticks: 240 };
    /// let ship = ShipInit { id: 1, position: DVec3::new(5.0, 0.0, 0.0), ..ShipInit::default() };
    /// let setup = Setup { ships: vec![ship], pilots: Vec::new(), ai_pilots: Vec::new(), gravity_mu: 0.0, max_speed_mps: 20_000.0, ai };
    /// let sim = Sim::new(&setup, 0)?;
    /// // Heading 0 is the +Z axis (ADR 0007).
    /// assert!(sim.ship_heading(1).expect("the ship exists").abs() < 1e-12);
    /// assert!(sim.ship_heading(2).is_none());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn ship_heading(&self, id: u64) -> Option<f64> {
        let body = self.body_of(self.ships.get(&id)?)?;
        let nose = *body.rotation() * DVec3::Z;
        Some(det::atan2(nose.x, nose.z))
    }

    /// A ship's remaining hit points (zero once destroyed), if it exists. For tests
    /// and tools.
    ///
    /// **Provisional determinism fixture (SPAC-128), not the PD-28 combat rules.** Phase 2
    /// replaces it with the PD-28 rules (beam and bolt prototypes, shields, armour facings,
    /// sections); that change bumps `SIM_VERSION`.
    ///
    /// ```
    /// use st_math::glam::DVec3;
    /// use st_sim::{Setup, ShipInit, Sim};
    /// # use st_sim::AiParams;
    /// # let ai = AiParams { turn_gain: 3.0, fire_cone: 0.05, standoff: 0.5, unarmed_standoff: 100.0, drift_ticks: 240 };
    /// let ship = ShipInit { id: 1, position: DVec3::new(5.0, 0.0, 0.0), ..ShipInit::default() };
    /// let setup = Setup { ships: vec![ship], pilots: Vec::new(), ai_pilots: Vec::new(), gravity_mu: 0.0, max_speed_mps: 20_000.0, ai };
    /// let sim = Sim::new(&setup, 0)?;
    /// assert_eq!(sim.ship_hit_points(1), Some(100.0));
    /// assert_eq!(sim.ship_hit_points(2), None);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn ship_hit_points(&self, id: u64) -> Option<f64> {
        self.ships.get(&id).map(|s| s.hit_points)
    }

    /// Advances exactly one tick.
    ///
    /// `commands` must all carry [`Sim::tick`] as their tick and be sorted
    /// strictly ascending by (pilot, sequence number); see
    /// [`command::sort`](crate::command::sort). Within the tick, commands
    /// apply first, then AI pilots choose their controls, then weapons fire,
    /// then controls and gravity act on the bodies, every physics world
    /// takes one step and the bodies of ships destroyed in the tick are removed.
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
        self.remove_destroyed();
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
            // A product that overflows (or divides by a denormal above) would put an
            // infinity in the body and a NaN after it, which Rapier cannot survive. Skipping
            // the impulse is deterministic: it depends on the state alone.
            let impulse = accel * (mass * DT);
            if finite3(impulse) {
                body.apply_impulse(impulse, true);
            }
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
        h.f64(self.max_speed_mps)?;
        h.f64(self.ai.turn_gain)?;
        h.f64(self.ai.fire_cone)?;
        h.f64(self.ai.standoff)?;
        h.f64(self.ai.unarmed_standoff)?;
        h.u64(self.ai.drift_ticks);
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

    /// AI numbers for the tests. The step holds none of its own.
    fn test_ai() -> AiParams {
        AiParams {
            turn_gain: 3.0,
            fire_cone: 0.05,
            standoff: 0.5,
            unarmed_standoff: 100.0,
            drift_ticks: 240,
        }
    }

    fn setup() -> Setup {
        Setup {
            ships: vec![ship_at(1, DVec3::new(10.0, 0.0, 0.0))],
            pilots: vec![(PilotId(1), Some(1))],
            ai_pilots: Vec::new(),
            gravity_mu: 0.0,
            max_speed_mps: 20_000.0,
            ai: test_ai(),
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
            max_speed_mps: 20_000.0,
            ai: test_ai(),
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
    fn negative_dynamics_are_refused() {
        // A drag of -1e5 made `exp(-drag * DT)` overflow, the velocity NaN and Rapier panic.
        for (drag, accel, turn) in [
            (-1.0e5, 25.0, 1.5),
            (-1.0e-9, 25.0, 1.5),
            (-f64::MAX, 25.0, 1.5),
            (0.01, -1.0, 1.5),
            (0.01, 25.0, -1.0),
            (0.01, f64::NEG_INFINITY, 1.5),
        ] {
            let mut s = setup();
            s.ships[0].drag = drag;
            s.ships[0].max_accel = accel;
            s.ships[0].max_turn_rate = turn;
            let err = Sim::new(&s, 0).unwrap_err();
            assert!(
                matches!(err, SetupError::BadDynamics(1) | SetupError::NotFinite),
                "{drag} {accel} {turn}: {err:?}"
            );
        }
        let mut s = setup();
        s.ships[0].drag = f64::NAN;
        assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::NotFinite);
        // Zero is allowed, and so is a large positive drag.
        for drag in [0.0, 1.0e300] {
            let mut s = setup();
            s.ships[0].drag = drag;
            s.ships[0].max_accel = 0.0;
            s.ships[0].max_turn_rate = 0.0;
            let mut sim = Sim::new(&s, 0).unwrap();
            sim.step(&[]).unwrap();
        }
    }

    #[test]
    fn the_speed_limit_and_the_ai_numbers_are_validated() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut s = setup();
            s.max_speed_mps = bad;
            assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::BadMaxSpeed);
        }
        type Field = fn(&mut AiParams) -> &mut f64;
        let fields: [Field; 4] = [
            |a| &mut a.turn_gain,
            |a| &mut a.fire_cone,
            |a| &mut a.standoff,
            |a| &mut a.unarmed_standoff,
        ];
        for field in fields {
            for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
                let mut s = setup();
                *field(&mut s.ai) = bad;
                assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::BadAiParams);
            }
        }
        let mut s = setup();
        s.ai.drift_ticks = 0;
        assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::BadAiParams);
        s.ai.drift_ticks = 1;
        assert!(Sim::new(&s, 0).is_ok());
    }

    #[test]
    fn the_speed_limit_is_the_setups_not_a_constant() {
        // 30 km/s is above the old 20 km/s code constant and above Rapier's 400 m/s.
        let mut s = setup();
        s.ships[0].velocity = DVec3::new(0.0, 0.0, 30_000.0);
        s.ships[0].drag = 0.0;
        s.max_speed_mps = 50_000.0;
        let mut sim = Sim::new(&s, 0).unwrap();
        sim.step(&[]).unwrap();
        assert!((sim.ship_state(1).unwrap().1.z - 30_000.0).abs() < 1e-6);
        // A lower limit clamps.
        s.max_speed_mps = 1_000.0;
        let mut sim = Sim::new(&s, 0).unwrap();
        sim.step(&[]).unwrap();
        assert!(sim.ship_state(1).unwrap().1.length() <= 1_000.0 + 1e-6);
    }

    /// Huge but valid numbers must never panic, whatever they do to the state.
    #[test]
    fn extreme_valid_setups_never_panic() {
        let big = 1.0e300;
        let gun = Weapon {
            range: big,
            damage: big,
            radius: big,
            cooldown_ticks: u32::MAX,
        };
        type Change = Box<dyn Fn(&mut Setup)>;
        let variants: Vec<Change> = vec![
            Box::new(move |s| s.ships[0].drag = big),
            Box::new(move |s| s.ships[0].max_accel = big),
            Box::new(move |s| s.ships[0].max_turn_rate = big),
            Box::new(move |s| s.ships[0].position = DVec3::splat(big)),
            Box::new(move |s| s.ships[0].velocity = DVec3::splat(big)),
            Box::new(move |s| s.gravity_mu = big),
            Box::new(move |s| s.gravity_mu = -big),
            Box::new(move |s| s.max_speed_mps = f64::MAX),
            Box::new(move |s| s.max_speed_mps = f64::MIN_POSITIVE),
            Box::new(move |s| s.ships[0].hit_points = f64::MIN_POSITIVE),
            Box::new(move |s| s.ships[0].weapon = Some(gun)),
            Box::new(move |s| {
                s.ai = AiParams {
                    turn_gain: big,
                    fire_cone: big,
                    standoff: big,
                    unarmed_standoff: big,
                    drift_ticks: u64::MAX,
                }
            }),
            Box::new(move |s| {
                s.ai = AiParams {
                    turn_gain: f64::MIN_POSITIVE,
                    fire_cone: f64::MIN_POSITIVE,
                    standoff: f64::MIN_POSITIVE,
                    unarmed_standoff: f64::MIN_POSITIVE,
                    drift_ticks: 1,
                }
            }),
        ];
        for (n, variant) in variants.iter().enumerate() {
            eprintln!("variant {n}");
            let mut s = duel();
            variant(&mut s);
            s.ships[1].weapon = Some(Weapon {
                range: 500.0,
                damage: 10.0,
                radius: 2.0,
                cooldown_ticks: 6,
            });
            let Ok(mut sim) = Sim::new(&s, 7) else {
                continue;
            };
            for _ in 0..40 {
                // The state may turn to NaN; a step must still return, not panic.
                let _ = sim.step(&[]);
            }
            let _ = sim.state_hash();
        }
    }

    /// Rapier's island manager panics when a body is removed before its world has taken a
    /// real step; a ship that dies on the first tick must not trigger that.
    #[test]
    fn a_ship_destroyed_on_the_first_tick_is_removed_without_a_panic() {
        let mut s = duel();
        s.ships[0].hit_points = 5.0;
        let mut sim = Sim::new(&s, 7).unwrap();
        sim.step(&[]).unwrap();
        assert_eq!(sim.ship_hit_points(1), Some(0.0));
        assert_eq!(sim.ship_state(1), None);
        for _ in 0..20 {
            sim.step(&[]).unwrap();
        }
        assert!(sim.ship_state(2).is_some());
    }

    /// A seeded sweep over extreme magnitudes of every number a setup carries. A panic
    /// anywhere (in this crate, Rapier or Parry) fails the test. The state may well turn to
    /// NaN; a step must still return.
    #[test]
    fn random_extreme_setups_never_panic() {
        let mags = [1e-300, 1e-9, 1.0, 1e3, 1e6, 1e9, 1e12, 1e100, 1e300];
        let state = std::cell::Cell::new(0x1234567_u64);
        let next = || {
            let mut x = state.get();
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            state.set(x);
            x
        };
        for case in 0..600 {
            let pick = |signed: bool| {
                let m = mags[(next() % mags.len() as u64) as usize];
                if signed && next() % 2 == 0 { -m } else { m }
            };
            let mut s = duel();
            for i in 0..4 {
                let ship = &mut s.ships[i % 2];
                let i = i % 2;
                let which = next() % 8;
                match which {
                    0 => {
                        let v = pick(true);
                        ship.position = DVec3::new(-150.0 + 300.0 * i as f64, 0.0, v);
                    }
                    1 => {
                        let v = pick(true);
                        ship.velocity = DVec3::new(v, 0.0, v);
                    }
                    2 => {
                        let v = pick(false);
                        ship.max_accel = v;
                    }
                    3 => {
                        let v = pick(false);
                        ship.drag = v;
                    }
                    4 => {
                        let (r, d, rad) = (pick(false), pick(false), pick(false));
                        ship.weapon = Some(Weapon {
                            range: r,
                            damage: d,
                            radius: rad,
                            cooldown_ticks: 1,
                        });
                    }
                    5 => {
                        let v = pick(false);
                        ship.hit_points = v;
                    }
                    6 => {
                        let v = pick(false);
                        if let PartShape::Cuboid { half_extents } = &mut ship.hull[0].shape {
                            *half_extents = DVec3::splat(v);
                        }
                    }
                    _ => {
                        let v = pick(false);
                        ship.hull[0].density = v;
                    }
                }
            }
            if next() % 3 == 0 {
                let v = pick(true);
                s.gravity_mu = v;
            }
            if next() % 3 == 0 {
                let v = pick(false);
                s.max_speed_mps = v;
            }
            if let Ok(mut sim) = Sim::new(&s, case) {
                for _ in 0..40 {
                    let _ = sim.step(&[]);
                }
                let _ = sim.state_hash();
            }
        }
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
