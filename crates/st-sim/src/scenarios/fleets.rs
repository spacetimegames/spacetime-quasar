//! Scenarios `physics-swarm` and `combat-60s` (ADR 0008, "Harness").
//!
//! Both are code-defined (ADR 0009, "Version policy"): the ships, hulls and weapons come
//! from the constants and `st-rng` streams in this file, never from `assets/`, so a
//! balance edit in the data cannot move a pinned reference line.

use st_math::det;
use st_math::glam::{DQuat, DVec3};
use st_rng::Stream;
use st_time::Tick;

use super::{FAR, MU, Scenario, controls_with};
use crate::command::{Command, PilotId};
use crate::hull::{FIRE, HullPart, PartShape, Weapon};
use crate::sim::{Setup, ShipInit};

/// An orientation from a yaw about +Y followed by a pitch about +X, using only `det`.
fn orientation(yaw: f64, pitch: f64) -> DQuat {
    let (ys, yc) = det::sin_cos(0.5 * yaw);
    let (ps, pc) = det::sin_cos(0.5 * pitch);
    DQuat::from_xyzw(0.0, ys, 0.0, yc) * DQuat::from_xyzw(ps, 0.0, 0.0, pc)
}

/// A hull of `count` varied parts (boxes, balls and cylinders at offsets and rotations),
/// drawn from `s` at counters `base..`. Every part is a few metres across.
fn varied_hull(s: &Stream, count: u64) -> Vec<HullPart> {
    (0..count)
        .map(|i| {
            let c = 16 * i;
            let size = |k: u64| 0.8 + 1.7 * s.f64_at(c + k);
            let shape = match s.below_at(c, 3) {
                0 => PartShape::Cuboid {
                    half_extents: DVec3::new(size(1), size(2), size(3)),
                },
                1 => PartShape::Ball { radius: size(1) },
                _ => PartShape::Cylinder {
                    half_height: size(1),
                    radius: size(2),
                },
            };
            let spread = |k: u64| 4.0 * (s.f64_at(c + k) - 0.5);
            HullPart {
                shape,
                offset: DVec3::new(spread(4), spread(5), spread(6)),
                rotation: orientation(
                    core::f64::consts::TAU * s.f64_at(c + 7),
                    core::f64::consts::PI * (s.f64_at(c + 8) - 0.5),
                ),
                density: 50.0 + 100.0 * s.f64_at(c + 9),
            }
        })
        .collect()
}

/// 200 externally piloted ships 500 km out, each a compound hull of three or four
/// varied parts with explicit mass properties, planar locked axes, continuous collision
/// detection on every other ship, random thrust and yaw from `st-rng`, and 50 weapon
/// sweeps (Parry shape casts) per tick, for 7,200 ticks (60 s). The ships start in a
/// loose grid a few body lengths apart, so they collide as they manoeuvre.
///
/// ```
/// let s = st_sim::scenarios::physics_swarm();
/// assert_eq!((s.name, s.ticks, s.setup.ships.len()), ("physics-swarm", 7200, 200));
/// ```
pub fn physics_swarm() -> Scenario {
    const SHIPS: u64 = 200;
    const ARMED: u64 = 50;
    const TICKS: Tick = 7200;
    const COLUMNS: u64 = 15;
    const SPACING: f64 = 14.0;
    let seed = 4;
    let rng = Stream::from_seed(seed).derive_label("physics-swarm");
    // Centre of the swarm, 500 km out on x and 300 km on z (ADR 0008).
    let centre = DVec3::new(FAR, 0.0, 0.6 * FAR);
    let gun = Weapon {
        range: 300.0,
        damage: 1.0,
        radius: 1.0,
        cooldown_ticks: 1,
    };
    let mut ships = Vec::new();
    let mut pilots = Vec::new();
    for id in 1..=SHIPS {
        let s = rng.derive_label("setup").derive(id);
        let (col, row) = ((id - 1) % COLUMNS, (id - 1) / COLUMNS);
        let jitter = |k: u64| 6.0 * (s.f64_at(k) - 0.5);
        let position = centre
            + DVec3::new(
                SPACING * (col as f64 - 7.0) + jitter(0),
                0.0,
                SPACING * (row as f64 - 7.0) + jitter(1),
            );
        let velocity = DVec3::new(6.0 * (s.f64_at(2) - 0.5), 0.0, 6.0 * (s.f64_at(3) - 0.5));
        ships.push(ShipInit {
            id,
            position,
            velocity,
            heading: core::f64::consts::TAU * s.f64_at(4),
            hull: varied_hull(&s.derive_label("hull"), 3 + s.below_at(5, 2)),
            ccd: id % 2 == 0,
            // Effectively invulnerable: the sweeps exercise Parry, not attrition.
            hit_points: 1.0e12,
            weapon: (id <= ARMED).then_some(gun),
            ..ShipInit::default()
        });
        pilots.push((PilotId(id), Some(id)));
    }
    let mut next_seq = vec![1_u64; (SHIPS + 1) as usize];
    let mut commands = Vec::new();
    for tick in 0..TICKS {
        for pilot in 1..=SHIPS {
            let s = rng.derive_label("input").derive(pilot).derive(tick);
            // About one change per second per pilot (tick 0 always sets).
            if tick != 0 && s.below_at(0, 120) != 0 {
                continue;
            }
            let axis = |i: u64| (s.range_at(i, 0, 65_535) as i64 - 32_767) as i16;
            let seq = &mut next_seq[pilot as usize];
            // The armed ships hold the trigger for the whole run.
            let buttons = if pilot <= ARMED { FIRE } else { 0 };
            commands.push(controls_with(
                tick,
                pilot,
                *seq,
                (axis(1), axis(2), axis(3)),
                buttons,
            ));
            *seq += 1 + s.below_at(4, 3);
        }
    }
    Scenario {
        name: "physics-swarm",
        seed,
        setup: Setup {
            ships,
            pilots,
            ai_pilots: Vec::new(),
            gravity_mu: MU,
        },
        commands,
        ticks: TICKS,
    }
}

/// Two teams of eight fighters, 600 m apart and 500 km out, fight for 60 s. One
/// fighter per team has a scripted external pilot (a command log from `st-rng`); the
/// other fourteen fly with AI pilots inside the simulation. Weapons sweep, hits take
/// hit points, and destroyed ships leave their physics world.
///
/// ```
/// let s = st_sim::scenarios::combat_60s();
/// assert_eq!((s.name, s.ticks, s.setup.ships.len()), ("combat-60s", 7200, 16));
/// assert_eq!((s.setup.pilots.len(), s.setup.ai_pilots.len()), (2, 14));
/// ```
pub fn combat_60s() -> Scenario {
    const PER_TEAM: u64 = 8;
    const TICKS: Tick = 7200;
    let seed = 5;
    let rng = Stream::from_seed(seed).derive_label("combat-60s");
    let gun = Weapon {
        range: 600.0,
        damage: 8.0,
        radius: 3.0,
        cooldown_ticks: 12,
    };
    let mut ships = Vec::new();
    for id in 1..=2 * PER_TEAM {
        let team = if id <= PER_TEAM { 1_u32 } else { 2 };
        let index = (id - 1) % PER_TEAM;
        let s = rng.derive_label("setup").derive(id);
        let side = if team == 1 { -1.0 } else { 1.0 };
        let position = DVec3::new(
            FAR + side * 300.0 + 80.0 * (s.f64_at(0) - 0.5),
            0.0,
            0.6 * FAR + 40.0 * (index as f64 - 3.5) + 20.0 * (s.f64_at(1) - 0.5),
        );
        // Teams face each other: team 1 looks along +x, team 2 along -x.
        let heading = -side * core::f64::consts::FRAC_PI_2;
        ships.push(ShipInit {
            id,
            position,
            heading,
            max_accel: 30.0,
            max_turn_rate: 2.0,
            hull: varied_hull(&s.derive_label("hull"), 3),
            ccd: true,
            team,
            hit_points: 120.0,
            weapon: Some(gun),
            ..ShipInit::default()
        });
    }
    // Ship 1 and ship 9 are flown by scripted external pilots, the rest by AI.
    let scripted = [1_u64, PER_TEAM + 1];
    let pilots = scripted.iter().map(|&id| (PilotId(id), Some(id))).collect();
    let ai_pilots = (1..=2 * PER_TEAM)
        .filter(|id| !scripted.contains(id))
        .map(|id| (PilotId(id), id))
        .collect();
    let mut commands: Vec<Command> = Vec::new();
    for &pilot in &scripted {
        let mut seq = 1_u64;
        for tick in (0..TICKS).step_by(60) {
            let s = rng.derive_label("input").derive(pilot).derive(tick);
            let axis = |i: u64| (s.range_at(i, 0, 65_535) as i64 - 32_767) as i16;
            commands.push(controls_with(
                tick,
                pilot,
                seq,
                (axis(0) / 2 + 16_000, axis(1) / 2, axis(2) / 4),
                FIRE,
            ));
            seq += 1 + s.below_at(3, 3);
        }
    }
    commands.sort_by_key(|c| (c.tick, c.pilot, c.seq));
    Scenario {
        name: "combat-60s",
        seed,
        setup: Setup {
            ships,
            pilots,
            ai_pilots,
            gravity_mu: MU,
        },
        commands,
        ticks: TICKS,
    }
}
