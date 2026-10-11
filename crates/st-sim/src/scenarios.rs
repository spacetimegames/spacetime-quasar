//! Determinism scenarios: a seed, a command log and a tick count each.
//!
//! The harness (`tools/determinism`) runs every scenario and compares the
//! checkpoint hashes with the committed reference file on every platform. The
//! command logs are generated here from `st-rng` streams, so they are as
//! reproducible as the simulation itself. Each log is sorted by (tick, pilot,
//! sequence number); it contains only commands the simulation accepts.

use std::fmt::Write as _;

use st_math::det;
use st_math::glam::DVec3;
use st_rng::Stream;
use st_time::Tick;

use crate::command::{Command, CommandKind, ControlState, PilotId, StepError};
use crate::hash::{DumpLine, HashError};
use crate::sim::{AiParams, CHECKPOINT_INTERVAL, Setup, SetupError, ShipInit, Sim};

mod fleets;
mod kernels;

pub use fleets::{combat_60s, physics_swarm};
pub use kernels::{EDGE, MathKernels, math_kernels};

/// Attractor strength shared by the scenarios, in m^3/s^2.
const MU: f64 = 1.0e9;
/// The speed limit of the pinned scenarios, in metres per second. The scenarios are defined in
/// code (ADR 0009), so they carry their own limit; a game builds its setups from data.
const MAX_SPEED_MPS: f64 = 20_000.0;
/// The AI numbers of the pinned scenarios (`combat-60s`). A provisional determinism fixture
/// (SPAC-128), not the PD-28 combat rules; scenarios without AI pilots carry them unused.
const AI: AiParams = AiParams {
    turn_gain: 3.0,
    fire_cone: 0.05,
    standoff: 0.5,
    unarmed_standoff: 100.0,
    drift_ticks: 240,
};
/// Distance from the origin of the far-out scenarios: 500 km, where `f32`
/// positions would already be coarse (ADR 0007).
const FAR: f64 = 500_000.0;

/// A seed, an initial setup, a command log and a length.
#[derive(Clone, Debug)]
pub struct Scenario {
    /// Unique, lower-case, hyphenated name used in the reference file.
    pub name: &'static str,
    /// World seed.
    pub seed: u64,
    /// Initial state.
    pub setup: Setup,
    /// Every command, sorted by (tick, pilot, sequence number).
    pub commands: Vec<Command>,
    /// How many ticks to run.
    pub ticks: Tick,
}

/// The state hash at one checkpoint tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    /// Tick the hash was taken at.
    pub tick: Tick,
    /// Canonical state hash at that tick.
    pub hash: u64,
}

/// A scenario being replayed: the simulation and how many commands it has
/// consumed.
struct Run<'a> {
    scenario: &'a Scenario,
    sim: Sim,
    next: usize,
}

impl<'a> Run<'a> {
    fn new(scenario: &'a Scenario) -> Result<Self, RunError> {
        Ok(Self {
            scenario,
            sim: Sim::new(&scenario.setup, scenario.seed).map_err(RunError::Setup)?,
            next: 0,
        })
    }

    /// Steps one tick with the commands stamped at or before it.
    fn step(&mut self) -> Result<(), RunError> {
        let tick = self.sim.tick();
        let commands = &self.scenario.commands;
        let start = self.next;
        while self.next < commands.len() && commands[self.next].tick <= tick {
            self.next += 1;
        }
        self.sim
            .step(&commands[start..self.next])
            .map_err(|error| RunError::Step { tick, error })
    }
}

/// A scenario failed to run.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunError {
    /// The setup was rejected.
    Setup(SetupError),
    /// A step rejected the commands of this tick.
    Step {
        /// Tick being stepped.
        tick: Tick,
        /// Why.
        error: StepError,
    },
    /// The state could not be hashed.
    Hash(HashError),
}

impl core::fmt::Display for RunError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RunError::Setup(e) => write!(f, "setup: {e}"),
            RunError::Step { tick, error } => write!(f, "tick {tick}: {error}"),
            RunError::Hash(e) => write!(f, "hash: {e}"),
        }
    }
}

impl core::error::Error for RunError {}

impl Scenario {
    /// Runs the scenario and returns its checkpoints: tick 0, every
    /// [`CHECKPOINT_INTERVAL`] ticks, and the final tick.
    ///
    /// # Errors
    ///
    /// A [`RunError`] if the setup is invalid, the log breaks the step rules
    /// or the state contains a NaN.
    pub fn run(&self) -> Result<Vec<Checkpoint>, RunError> {
        self.run_with_interval(CHECKPOINT_INTERVAL)
    }

    /// Like [`Scenario::run`] with a checkpoint every `every` ticks (at least
    /// 1). A smaller interval helps to localise a divergence.
    ///
    /// # Errors
    ///
    /// As [`Scenario::run`].
    pub fn run_with_interval(&self, every: Tick) -> Result<Vec<Checkpoint>, RunError> {
        let every = every.max(1);
        let mut run = Run::new(self)?;
        let mut out = vec![Checkpoint {
            tick: 0,
            hash: run.sim.state_hash().map_err(RunError::Hash)?,
        }];
        while run.sim.tick() < self.ticks {
            run.step()?;
            let now = run.sim.tick();
            if now % every == 0 || now == self.ticks {
                out.push(Checkpoint {
                    tick: now,
                    hash: run.sim.state_hash().map_err(RunError::Hash)?,
                });
            }
        }
        Ok(out)
    }

    /// Runs the scenario to `tick` and returns the canonical hash input at
    /// that tick, one [`DumpLine`] per section header and entity (the
    /// harness `--dump-tick` output). `Ok(None)` if the scenario is shorter
    /// than `tick`.
    ///
    /// # Errors
    ///
    /// As [`Scenario::run`].
    pub fn dump_at(&self, tick: Tick) -> Result<Option<Vec<DumpLine>>, RunError> {
        if tick > self.ticks {
            return Ok(None);
        }
        let mut run = Run::new(self)?;
        while run.sim.tick() < tick {
            run.step()?;
        }
        run.sim.state_dump().map(Some).map_err(RunError::Hash)
    }

    /// Runs the scenario and formats one line per checkpoint in the ADR 0003
    /// format: `<name> <tick, 8 digits> <hash, 16 lower-case hex digits>`,
    /// LF-terminated.
    ///
    /// # Errors
    ///
    /// As [`Scenario::run`].
    pub fn report(&self) -> Result<String, RunError> {
        let mut text = String::new();
        for c in self.run()? {
            // Writing to a `String` cannot fail.
            let _ = writeln!(text, "{} {:08} {:016x}", self.name, c.tick, c.hash);
        }
        Ok(text)
    }
}

/// Every scenario, in name order.
pub fn all() -> Vec<Scenario> {
    vec![
        combat_60s(),
        orbit_coast(),
        physics_swarm(),
        solo_burn(),
        swarm_controls(),
    ]
}

/// Looks a scenario up by name.
pub fn by_name(name: &str) -> Option<Scenario> {
    all().into_iter().find(|s| s.name == name)
}

fn controls(tick: Tick, pilot: u64, seq: u64, thrust: i16, strafe: i16, turn: i16) -> Command {
    controls_with(tick, pilot, seq, (thrust, strafe, turn), 0)
}

/// A `Controls` command with held buttons.
fn controls_with(
    tick: Tick,
    pilot: u64,
    seq: u64,
    (thrust, strafe, turn): (i16, i16, i16),
    buttons: u32,
) -> Command {
    Command {
        tick,
        pilot: PilotId(pilot),
        seq,
        kind: CommandKind::Controls(ControlState {
            thrust,
            strafe,
            turn,
            aim: None,
            buttons,
        }),
    }
}

fn ship(id: u64, position: DVec3, velocity: DVec3, heading: f64) -> ShipInit {
    ShipInit {
        id,
        position,
        velocity,
        heading,
        ..ShipInit::default()
    }
}

/// One ship near the origin flown by a scripted pilot: burn, turn, strafe and
/// brake over 5 s.
pub fn solo_burn() -> Scenario {
    let commands = vec![
        controls(0, 1, 1, 32767, 0, 0),
        controls(120, 1, 2, 32767, 0, 16000),
        controls(240, 1, 3, 0, -32767, 0),
        controls(360, 1, 4, -32767, 0, -32767),
        controls(480, 1, 5, 0, 0, 0),
    ];
    Scenario {
        name: "solo-burn",
        seed: 1,
        setup: Setup {
            ships: vec![ship(1, DVec3::new(10_000.0, 0.0, 0.0), DVec3::ZERO, 0.0)],
            pilots: vec![(PilotId(1), Some(1))],
            ai_pilots: Vec::new(),
            gravity_mu: MU,
            max_speed_mps: MAX_SPEED_MPS,
            ai: AI,
        },
        commands,
        ticks: 600,
    }
}

/// Ships on near-circular orbits at different radii, coasting for 10 s. One
/// pilot brakes one ship; the others have no pilot.
pub fn orbit_coast() -> Scenario {
    let seed = 2;
    let rng = Stream::from_seed(seed).derive_label("orbit-coast");
    let mut ships = Vec::new();
    for id in 1..=8_u64 {
        let ring = rng.derive(id);
        let radius = 20_000.0 + 10_000.0 * ring.range_at(0, 0, 40) as f64;
        let phase = ring.f64_at(1) * (2.0 * core::f64::consts::PI);
        let (s, c) = det::sin_cos(phase);
        let speed = (MU / radius).sqrt();
        // Circular orbit in the XZ plane, nose along the velocity.
        let velocity = DVec3::new(-speed * s, 0.0, speed * c);
        ships.push(ship(
            id,
            DVec3::new(radius * c, 0.0, radius * s),
            velocity,
            det::atan2(velocity.x, velocity.z),
        ));
    }
    Scenario {
        name: "orbit-coast",
        seed,
        setup: Setup {
            ships,
            pilots: vec![(PilotId(1), Some(1))],
            ai_pilots: Vec::new(),
            gravity_mu: MU,
            max_speed_mps: MAX_SPEED_MPS,
            ai: AI,
        },
        commands: vec![
            controls(600, 1, 1, -32767, 0, 0),
            controls(900, 1, 2, 0, 0, 0),
        ],
        ticks: 1200,
    }
}

/// 64 ships 500 km out, each flown by its own pilot, whose random control
/// changes (with sequence-number gaps) come from `st-rng`; 20 s.
pub fn swarm_controls() -> Scenario {
    const SHIPS: u64 = 64;
    const TICKS: Tick = 2400;
    let seed = 3;
    let rng = Stream::from_seed(seed).derive_label("swarm");
    let mut ships = Vec::new();
    let mut pilots = Vec::new();
    for id in 1..=SHIPS {
        let s = rng.derive_label("setup").derive(id);
        let angle = s.f64_at(0) * (2.0 * core::f64::consts::PI);
        let (sin, cos) = det::sin_cos(angle);
        let radius = FAR + 1_000.0 * s.f64_at(1);
        let heading = s.f64_at(2) * (2.0 * core::f64::consts::PI);
        let (hs, hc) = det::sin_cos(heading);
        let speed = 20.0 * s.f64_at(3);
        ships.push(ship(
            id,
            DVec3::new(radius * cos, 0.0, radius * sin),
            DVec3::new(speed * hs, 0.0, speed * hc),
            heading,
        ));
        pilots.push((PilotId(id), Some(id)));
    }
    let mut next_seq = [1_u64; (SHIPS + 1) as usize];
    let mut commands = Vec::new();
    for tick in 0..TICKS {
        for pilot in 1..=SHIPS {
            let s = rng.derive_label("input").derive(pilot).derive(tick);
            // About one change per second per pilot (tick 0 always sets).
            if tick != 0 && s.below_at(0, 120) != 0 {
                continue;
            }
            let axis = |i: u64| s.range_at(i, 0, 65_535) as i64 - 32_767;
            let seq = &mut next_seq[pilot as usize];
            let cmd = controls(
                tick,
                pilot,
                *seq,
                axis(1) as i16,
                axis(2) as i16,
                axis(3) as i16,
            );
            *seq += 1 + s.below_at(4, 3);
            commands.push(cmd);
        }
    }
    Scenario {
        name: "swarm-controls",
        seed,
        setup: Setup {
            ships,
            pilots,
            ai_pilots: Vec::new(),
            gravity_mu: MU,
            max_speed_mps: MAX_SPEED_MPS,
            ai: AI,
        },
        commands,
        ticks: TICKS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_are_sorted_and_accepted() {
        for s in all() {
            let sorted = s
                .commands
                .windows(2)
                .all(|w| (w[0].tick, w[0].pilot, w[0].seq) < (w[1].tick, w[1].pilot, w[1].seq));
            assert!(sorted, "{} log is not sorted", s.name);
            assert!(s.run().is_ok(), "{} failed to run", s.name);
        }
    }

    #[test]
    fn checkpoints_every_120_ticks_and_at_the_end() {
        let ticks: Vec<_> = solo_burn().run().unwrap().iter().map(|c| c.tick).collect();
        assert_eq!(ticks, [0, 120, 240, 360, 480, 600]);
        let mut s = solo_burn();
        s.ticks = 130;
        let ticks: Vec<_> = s.run().unwrap().iter().map(|c| c.tick).collect();
        assert_eq!(ticks, [0, 120, 130]);
    }

    #[test]
    fn names_are_unique_and_sorted() {
        let names: Vec<_> = all().iter().map(|s| s.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted);
        assert!(by_name("solo-burn").is_some());
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn report_format() {
        let text = solo_burn().report().unwrap();
        let first = text.lines().next().unwrap();
        assert_eq!(first.len(), "solo-burn".len() + 1 + 8 + 1 + 16);
        assert!(first.starts_with("solo-burn 00000000 "));
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn the_dump_is_the_hashed_stream() {
        use crate::hash::StateHasher;
        let s = solo_burn();
        let want = s
            .run()
            .unwrap()
            .iter()
            .find(|c| c.tick == 240)
            .unwrap()
            .hash;
        let lines = s.dump_at(240).unwrap().unwrap();
        let mut h = StateHasher::new();
        for line in &lines {
            h.bytes(&line.bytes);
        }
        assert_eq!(h.finish(), want);
        assert!(lines.iter().any(|l| l.section == "ENTS" && l.key.is_some()));
        assert!(lines.iter().any(|l| l.section == "PILT" && l.key.is_some()));
        assert!(s.dump_at(s.ticks + 1).unwrap().is_none());
    }

    #[test]
    fn different_seed_gives_different_hashes() {
        let a = solo_burn();
        let mut b = solo_burn();
        b.seed = 99;
        assert_ne!(a.run().unwrap(), b.run().unwrap());
    }
}
