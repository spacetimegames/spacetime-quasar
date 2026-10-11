//! The replay: a deterministic `st-sim` scenario built from the envelope.

use std::collections::BTreeMap;
use std::fmt;

use st_math::glam::DVec3;
use st_sim::scenarios::{RunError, Scenario};
use st_sim::{Command, CommandKind, ControlState, PilotId, Setup, SetupError, ShipInit};
use st_time::{DT, TICK_RATE_HZ, Tick};

use crate::envelope::SpaceEnvelope;

/// The longest replay the demo builds: three simulated hours. A bound on the
/// work one edit can cause, not a game value.
pub const MAX_REPLAY_TICKS: Tick = TICK_RATE_HZ as Tick * 3 * 3600;

/// Full thrust, as the simulation's quantised axis value.
const FULL: i16 = 32767;

/// Why the replay cannot run for this envelope.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum ReplayError {
    /// A derived number is not finite (a value in the file is absurdly large).
    NotFinite,
    /// The slowest ship needs more than [`MAX_REPLAY_TICKS`] to cross.
    TooLong {
        /// The crossing time in seconds.
        seconds: f64,
    },
    /// `st-sim` rejected the setup.
    Setup(SetupError),
    /// `st-sim` failed while running.
    Run(RunError),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplayError::NotFinite => f.write_str("a derived number is not finite"),
            ReplayError::TooLong { seconds } => write!(
                f,
                "the slowest ship needs {seconds:.0} s to cross, more than the {} s the demo replays",
                MAX_REPLAY_TICKS / TICK_RATE_HZ as Tick
            ),
            ReplayError::Setup(e) => write!(f, "setup: {e}"),
            ReplayError::Run(e) => write!(f, "run: {e}"),
        }
    }
}

impl std::error::Error for ReplayError {}

/// What a replay did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Replay {
    /// How many ticks it ran.
    pub ticks: Tick,
    /// The state hash at the final tick (ADR 0008, format version 1).
    pub hash: u64,
    /// Seconds the normal-speed ship needs to cross the system edge to edge,
    /// from rest to rest.
    pub normal_crossing_s: f64,
    /// Seconds the cruise-speed ship needs for the same trip.
    pub cruise_crossing_s: f64,
}

/// One edge-to-edge trip: speed up, coast, slow down.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Trip {
    /// Seconds of speeding up (and of slowing down).
    ramp_s: f64,
    /// Seconds at constant speed.
    coast_s: f64,
}

impl Trip {
    /// A trip of `length_m` metres that tops out at `speed` m/s, or lower when
    /// the trip is too short to reach it.
    fn new(speed: f64, accel: f64, length_m: f64) -> Trip {
        let ramp_length = speed * speed / accel;
        if ramp_length <= length_m {
            Trip {
                ramp_s: speed / accel,
                coast_s: (length_m - ramp_length) / speed,
            }
        } else {
            // Triangular: speed up over half the trip, slow down over the rest.
            Trip {
                ramp_s: (accel * length_m).sqrt() / accel,
                coast_s: 0.0,
            }
        }
    }

    fn total_s(&self) -> f64 {
        2.0 * self.ramp_s + self.coast_s
    }
}

/// Adds the thrust changes of one burn of `duration_ticks` ticks (fractional)
/// that starts at tick `start`. `sign` is `1` to speed up and `-1` to slow
/// down. The last tick gets a part of full thrust so the burn is not rounded
/// to a whole number of ticks.
fn burn(events: &mut BTreeMap<Tick, i16>, start: Tick, duration_ticks: f64, sign: i16) {
    let whole = duration_ticks.floor();
    let part = ((duration_ticks - whole) * f64::from(FULL)).round() as i16;
    let whole = whole as Tick;
    let end = start + whole + Tick::from(part > 0);
    events.insert(end, 0);
    if part > 0 {
        events.insert(start + whole, sign * part);
    }
    if whole > 0 {
        events.insert(start, sign * FULL);
    }
}

/// The thrust changes of a trip as (tick, sequence number, thrust), in order.
fn commands_for(trip: &Trip) -> Vec<(Tick, u64, i16)> {
    let ramp_ticks = trip.ramp_s / DT;
    let mut events = BTreeMap::new();
    burn(&mut events, 0, ramp_ticks, 1);
    // Start slowing down when the coast ends, but never before the speed-up
    // has finished.
    let first_end = events.keys().next_back().copied().unwrap_or(0);
    let brake_at = ((trip.ramp_s + trip.coast_s) / DT).floor() as Tick;
    let brake_at = if brake_at < first_end {
        first_end
    } else {
        brake_at
    };
    burn(&mut events, brake_at, ramp_ticks, -1);
    events
        .into_iter()
        .enumerate()
        .map(|(i, (tick, thrust))| (tick, i as u64 + 1, thrust))
        .collect()
}

/// Builds the replay scenario for an envelope.
///
/// Two ships start at rest at the edge of the system and fly across it to the
/// opposite edge in a straight line, one topping out at normal speed and one
/// at cruise speed. Each is flown by its own pilot, who speeds up, coasts and
/// slows down with held thrust commands. Everything comes from the envelope.
/// The scenario runs until the slower ship arrives.
///
/// # Errors
///
/// [`ReplayError::NotFinite`] or [`ReplayError::TooLong`].
///
/// ```
/// use phase1_demo::{SpaceEnvelope, scenario};
/// use st_data::Loader;
///
/// let text = br#"(kind: "space_envelope", version: 1, data: (
///     normal_speed_mps: 1000.0, cruise_multiplier: 10.0, system_radius_km: 2000.0,
///     body_spacing_km: (min: 60.0, max: 120.0), acceleration_mps2: 100.0,
///     replay_seed: 31, reference_speeds: [], reference_lengths: []))"#;
/// let env = Loader::new().load::<SpaceEnvelope>("pd-31.ron", text).unwrap().value;
/// let s = scenario::build(&env)?;
/// assert_eq!(s.seed, 31);
/// assert_eq!(s.setup.ships.len(), 2);
/// # Ok::<(), phase1_demo::ReplayError>(())
/// ```
pub fn build(env: &SpaceEnvelope) -> Result<Scenario, ReplayError> {
    let (scenario, _, _) = build_with_times(env)?;
    Ok(scenario)
}

fn build_with_times(env: &SpaceEnvelope) -> Result<(Scenario, f64, f64), ReplayError> {
    let radius_m = env.system_radius_km * 1000.0;
    let length_m = 2.0 * radius_m;
    let accel = env.acceleration_mps2;
    let normal = Trip::new(env.normal_speed_mps, accel, length_m);
    let cruise = Trip::new(env.cruise_speed_mps(), accel, length_m);
    let (normal_s, cruise_s) = (normal.total_s(), cruise.total_s());
    if !(normal_s.is_finite() && cruise_s.is_finite() && radius_m.is_finite()) {
        return Err(ReplayError::NotFinite);
    }
    let slowest = if normal_s > cruise_s {
        normal_s
    } else {
        cruise_s
    };
    let ticks_f = (slowest / DT).ceil();
    if ticks_f > MAX_REPLAY_TICKS as f64 {
        return Err(ReplayError::TooLong { seconds: slowest });
    }

    // Both ships start on the +Z axis side by side and fly along +Z (heading 0), 100 m
    // apart on x so that their hulls do not overlap.
    let ship = |id: u64| ShipInit {
        id,
        position: DVec3::new(100.0 * id as f64, 0.0, -radius_m),
        max_accel: accel,
        max_turn_rate: 0.0,
        drag: 0.0,
        ..ShipInit::default()
    };
    let mut commands = Vec::new();
    for (pilot, trip) in [(1_u64, &normal), (2, &cruise)] {
        for (tick, seq, thrust) in commands_for(trip) {
            commands.push(Command {
                tick,
                pilot: PilotId(pilot),
                seq,
                kind: CommandKind::Controls(ControlState {
                    thrust,
                    ..ControlState::default()
                }),
            });
        }
    }
    commands.sort_by_key(|c| (c.tick, c.pilot, c.seq));
    let scenario = Scenario {
        name: "envelope-crossing",
        seed: env.replay_seed,
        setup: Setup {
            ships: vec![ship(1), ship(2)],
            pilots: vec![(PilotId(1), Some(1)), (PilotId(2), Some(2))],
            ai_pilots: Vec::new(),
            gravity_mu: 0.0,
        },
        commands,
        ticks: ticks_f as Tick,
    };
    Ok((scenario, normal_s, cruise_s))
}

/// Builds the replay for an envelope, runs it from the seed and returns the
/// final state hash.
///
/// The same envelope gives the same hash on every run and every platform.
///
/// # Errors
///
/// A [`ReplayError`] if the replay cannot be built or run.
///
/// ```
/// use phase1_demo::{SpaceEnvelope, scenario};
/// use st_data::Loader;
///
/// let text = br#"(kind: "space_envelope", version: 1, data: (
///     normal_speed_mps: 1000.0, cruise_multiplier: 10.0, system_radius_km: 2000.0,
///     body_spacing_km: (min: 60.0, max: 120.0), acceleration_mps2: 100.0,
///     replay_seed: 31, reference_speeds: [], reference_lengths: []))"#;
/// let env = Loader::new().load::<SpaceEnvelope>("pd-31.ron", text).unwrap().value;
/// let a = scenario::replay(&env)?;
/// let b = scenario::replay(&env)?;
/// assert_eq!(a.hash, b.hash);
/// # Ok::<(), phase1_demo::ReplayError>(())
/// ```
pub fn replay(env: &SpaceEnvelope) -> Result<Replay, ReplayError> {
    let (scenario, normal_crossing_s, cruise_crossing_s) = build_with_times(env)?;
    let checkpoints = scenario.run().map_err(ReplayError::Run)?;
    // `run` always returns at least the checkpoint at tick 0.
    let hash = checkpoints.last().map_or(0, |c| c.hash);
    Ok(Replay {
        ticks: scenario.ticks,
        hash,
        normal_crossing_s,
        cruise_crossing_s,
    })
}
