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
use crate::hash::HashError;
use crate::sim::{CHECKPOINT_INTERVAL, Setup, SetupError, ShipInit, Sim};

/// Attractor strength shared by the scenarios, in m^3/s^2.
const MU: f64 = 1.0e9;
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
        let mut sim = Sim::new(&self.setup, self.seed).map_err(RunError::Setup)?;
        let mut out = vec![Checkpoint {
            tick: 0,
            hash: sim.state_hash().map_err(RunError::Hash)?,
        }];
        let mut next = 0;
        while sim.tick() < self.ticks {
            let tick = sim.tick();
            let start = next;
            while next < self.commands.len() && self.commands[next].tick <= tick {
                next += 1;
            }
            sim.step(&self.commands[start..next])
                .map_err(|error| RunError::Step { tick, error })?;
            let now = sim.tick();
            if now % every == 0 || now == self.ticks {
                out.push(Checkpoint {
                    tick: now,
                    hash: sim.state_hash().map_err(RunError::Hash)?,
                });
            }
        }
        Ok(out)
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
    vec![orbit_coast(), solo_burn(), swarm_controls()]
}

/// Looks a scenario up by name.
pub fn by_name(name: &str) -> Option<Scenario> {
    all().into_iter().find(|s| s.name == name)
}

fn controls(tick: Tick, pilot: u64, seq: u64, thrust: i16, strafe: i16, turn: i16) -> Command {
    Command {
        tick,
        pilot: PilotId(pilot),
        seq,
        kind: CommandKind::Controls(ControlState {
            thrust,
            strafe,
            turn,
            aim: None,
            buttons: 0,
        }),
    }
}

fn ship(id: u64, position: DVec3, velocity: DVec3, heading: f64) -> ShipInit {
    ShipInit {
        id,
        system: 0,
        position,
        velocity,
        heading,
        max_accel: 25.0,
        max_turn_rate: 1.5,
        drag: 0.01,
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
            gravity_mu: MU,
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
        ships.push(ship(
            id,
            DVec3::new(radius * c, radius * s, 0.0),
            DVec3::new(-speed * s, speed * c, 0.0),
            phase + core::f64::consts::FRAC_PI_2,
        ));
    }
    Scenario {
        name: "orbit-coast",
        seed,
        setup: Setup {
            ships,
            pilots: vec![(PilotId(1), Some(1))],
            gravity_mu: MU,
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
            DVec3::new(radius * cos, radius * sin, 0.0),
            DVec3::new(speed * hc, speed * hs, 0.0),
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
            gravity_mu: MU,
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
    fn different_seed_gives_different_hashes() {
        let a = solo_burn();
        let mut b = solo_burn();
        b.seed = 99;
        assert_ne!(a.run().unwrap(), b.run().unwrap());
    }
}
