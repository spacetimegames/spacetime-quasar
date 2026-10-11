//! The simulation: bodies, pilots, the fixed step and the state hash.

use std::collections::BTreeMap;

use st_math::det;
use st_math::glam::DVec3;
use st_time::{DT, TICK_RATE_HZ, Tick};

use crate::command::{
    CONTROLS_COMPONENT_TAG, Command, CommandKind, ControlState, MAX_COMMANDS_PER_TICK, PilotId,
    StepError,
};
use crate::hash::{HASH_FORMAT_VERSION, HashError, StateHash, StateHasher};

/// Hashes are taken every this many ticks (1 s at 120 Hz), at tick 0 and at
/// the final tick (ADR 0008, "Checkpoints").
pub const CHECKPOINT_INTERVAL: Tick = TICK_RATE_HZ as Tick;

/// Axis values are divided by this to get a fraction in `-1.0..=1.0`.
const AXIS_MAX: f64 = 32767.0;

/// Initial state of one ship.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShipInit {
    /// Entity id, unique in the setup.
    pub id: u64,
    /// Star system the ship is in.
    pub system: u64,
    /// Position in the system frame, metres.
    pub position: DVec3,
    /// Velocity, metres per second.
    pub velocity: DVec3,
    /// Heading in radians, counter-clockwise from +x.
    pub heading: f64,
    /// Acceleration at full thrust or strafe, metres per second squared.
    pub max_accel: f64,
    /// Turn rate at full turn, radians per second.
    pub max_turn_rate: f64,
    /// Velocity damping per second (`v *= exp(-drag * dt)` each tick).
    pub drag: f64,
}

/// The initial state of a simulation: everything except the seed.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    /// The ships, in any order.
    pub ships: Vec<ShipInit>,
    /// Pilots other than the session pilot, which always exists. Each names
    /// the ship it controls, if any.
    pub pilots: Vec<(PilotId, Option<u64>)>,
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
}

impl core::fmt::Display for SetupError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SetupError::DuplicateShip(id) => write!(f, "duplicate ship id {id}"),
            SetupError::DuplicatePilot(p) => write!(f, "duplicate pilot id {}", p.0),
            SetupError::ReservedPilot => f.write_str("pilot 0 is reserved for the session"),
            SetupError::UnknownShip(id) => write!(f, "pilot controls unknown ship {id}"),
            SetupError::NotFinite => f.write_str("a number in the setup is not finite"),
        }
    }
}

impl core::error::Error for SetupError {}

#[derive(Clone, Debug)]
struct Ship {
    system: u64,
    position: DVec3,
    velocity: DVec3,
    heading: f64,
    max_accel: f64,
    max_turn_rate: f64,
    drag: f64,
}

impl Ship {
    fn state_hash_with_id(&self, id: u64, h: &mut StateHasher) -> Result<(), HashError> {
        h.u64(id);
        h.u64(self.system);
        // Body block (always present for a ship).
        h.option_tag(true);
        h.vec3(self.position)?;
        h.vec3(self.velocity)?;
        h.f64(self.heading)?;
        h.f64(self.max_accel)?;
        h.f64(self.max_turn_rate)?;
        h.f64(self.drag)?;
        // No components yet.
        h.count(0);
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct Pilot {
    controlled: Option<u64>,
    last_seq: u64,
    controls: ControlState,
}

/// The simulation. `Clone` is a snapshot.
///
/// State advances only through [`Sim::step`], one tick of `st_time::DT` per
/// call. Entities and pilots live in ordered maps, so iteration order is
/// part of the contract.
#[derive(Clone, Debug)]
pub struct Sim {
    tick: Tick,
    seed: u64,
    gravity_mu: f64,
    ships: BTreeMap<u64, Ship>,
    pilots: BTreeMap<PilotId, Pilot>,
}

fn finite3(v: DVec3) -> bool {
    v.x.is_finite() && v.y.is_finite() && v.z.is_finite()
}

impl Sim {
    /// Builds a simulation at tick 0 from a setup and a world seed.
    ///
    /// Pilot 0, the session, exists in every simulation.
    ///
    /// # Errors
    ///
    /// A [`SetupError`] if ids repeat, a pilot names a missing ship, or a
    /// number is NaN or infinite.
    pub fn new(setup: &Setup, seed: u64) -> Result<Sim, SetupError> {
        if !setup.gravity_mu.is_finite() {
            return Err(SetupError::NotFinite);
        }
        let mut ships = BTreeMap::new();
        for s in &setup.ships {
            let finite = finite3(s.position)
                && finite3(s.velocity)
                && s.heading.is_finite()
                && s.max_accel.is_finite()
                && s.max_turn_rate.is_finite()
                && s.drag.is_finite();
            if !finite {
                return Err(SetupError::NotFinite);
            }
            let ship = Ship {
                system: s.system,
                position: s.position,
                velocity: s.velocity,
                heading: s.heading,
                max_accel: s.max_accel,
                max_turn_rate: s.max_turn_rate,
                drag: s.drag,
            };
            if ships.insert(s.id, ship).is_some() {
                return Err(SetupError::DuplicateShip(s.id));
            }
        }
        let mut pilots = BTreeMap::new();
        pilots.insert(
            PilotId::SESSION,
            Pilot {
                controlled: None,
                last_seq: 0,
                controls: ControlState::default(),
            },
        );
        for &(id, controlled) in &setup.pilots {
            if id == PilotId::SESSION {
                return Err(SetupError::ReservedPilot);
            }
            if let Some(ship) = controlled
                && !ships.contains_key(&ship)
            {
                return Err(SetupError::UnknownShip(ship));
            }
            let pilot = Pilot {
                controlled,
                last_seq: 0,
                controls: ControlState::default(),
            };
            if pilots.insert(id, pilot).is_some() {
                return Err(SetupError::DuplicatePilot(id));
            }
        }
        Ok(Sim {
            tick: 0,
            seed,
            gravity_mu: setup.gravity_mu,
            ships,
            pilots,
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

    /// A ship's position and velocity, if it exists. For tests and tools;
    /// the simulation never reads this back.
    pub fn ship_state(&self, id: u64) -> Option<(DVec3, DVec3)> {
        self.ships.get(&id).map(|s| (s.position, s.velocity))
    }

    /// Advances exactly one tick.
    ///
    /// `commands` must all carry [`Sim::tick`] as their tick and be sorted
    /// strictly ascending by (pilot, sequence number); see
    /// [`command::sort`](crate::command::sort). Within the tick, commands
    /// apply first, then the bodies move.
    ///
    /// # Errors
    ///
    /// A [`StepError`] naming the first offending command. The whole slice is
    /// validated before anything changes, so on error the state is unchanged.
    pub fn step(&mut self, commands: &[Command]) -> Result<(), StepError> {
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
        self.integrate();
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
            match &cmd.kind {
                CommandKind::Controls(c) => {
                    if cmd.pilot == PilotId::SESSION {
                        return Err(StepError::WrongSource { index });
                    }
                    if !c.is_valid() {
                        return Err(StepError::InvalidPayload { index });
                    }
                }
            }
            // Sorted strictly ascending, so checking against the state is
            // enough: later commands of the same pilot have higher numbers.
            if cmd.seq <= pilot.last_seq {
                return Err(StepError::StaleSequence { index });
            }
        }
        Ok(())
    }

    /// Semi-implicit Euler for every ship, in ascending id order. Uses only
    /// `st_math::det` for transcendental functions.
    fn integrate(&mut self) {
        // Controls per ship: a ship is steered by the pilot that controls it.
        let mut steering: BTreeMap<u64, ControlState> = BTreeMap::new();
        for pilot in self.pilots.values() {
            if let Some(ship) = pilot.controlled {
                steering.insert(ship, pilot.controls);
            }
        }
        for (id, ship) in &mut self.ships {
            let controls = steering.get(id).copied().unwrap_or_default();
            ship.heading += ship.max_turn_rate * (f64::from(controls.turn) / AXIS_MAX) * DT;
            let (sin, cos) = det::sin_cos(ship.heading);
            let forward = DVec3::new(cos, sin, 0.0);
            let right = DVec3::new(sin, -cos, 0.0);
            let mut accel = forward * (ship.max_accel * (f64::from(controls.thrust) / AXIS_MAX))
                + right * (ship.max_accel * (f64::from(controls.strafe) / AXIS_MAX));
            let r2 = ship.position.length_squared();
            if r2 > 0.0 {
                // Inverse-square pull toward the origin.
                accel -= ship.position * (self.gravity_mu / (r2 * r2.sqrt()));
            }
            let damping = det::exp(-ship.drag * DT);
            ship.velocity = (ship.velocity + accel * DT) * damping;
            ship.position += ship.velocity * DT;
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
        h.tag(b"STQH");
        h.u32(HASH_FORMAT_VERSION);
        h.u64(self.tick);
        h.u64(self.seed);

        h.tag(b"ENTS");
        h.count(self.ships.len());
        for (id, ship) in &self.ships {
            ship.state_hash_with_id(*id, &mut h)?;
        }

        h.tag(b"PILT");
        h.count(self.pilots.len());
        for (id, pilot) in &self.pilots {
            h.u64(id.0);
            h.option_tag(pilot.controlled.is_some());
            if let Some(ship) = pilot.controlled {
                h.u64(ship);
            }
            h.u64(pilot.last_seq);
            h.count(1);
            h.u32(CONTROLS_COMPONENT_TAG);
            pilot.controls.state_hash(&mut h)?;
        }

        h.tag(b"GLOB");
        h.f64(self.gravity_mu)?;
        Ok(h.finish())
    }
}
