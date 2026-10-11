//! The combat rules of the simulation: AI pilots, weapon fire and destruction.
//!
//! **Provisional determinism fixture (SPAC-128), not the PD-28 combat rules.** Phase 2
//! replaces it with the PD-28 rules (beam and bolt prototypes, shields, armour facings,
//! sections); that change bumps `SIM_VERSION`. This module is the one replaceable
//! boundary: `Sim::step` calls `run_ai` and `fire_weapons` and nothing else reaches
//! into the rules. Its tuning numbers come from the setup (`AiParams`, `Weapon`); it
//! holds none of its own.

use st_math::det;
use st_math::glam::DVec3;
use st_rng::Stream;
use st_time::Tick;

use super::{AXIS_MAX, Sim, finite3};
use crate::command::ControlState;
use crate::hull::FIRE;

/// An axis fraction in `-1.0..=1.0` as the quantised command value.
fn quantise(fraction: f64) -> i16 {
    (det::clamp(fraction, -1.0, 1.0) * AXIS_MAX).round() as i16
}

/// What an AI pilot sees of one ship. Part of the provisional determinism fixture.
#[derive(Clone, Copy)]
struct Contact {
    id: u64,
    system: u64,
    team: u32,
    position: DVec3,
}

impl Sim {
    /// AI pilots, in ascending pilot id, choose controls from the state at the start
    /// of the tick: face and approach the nearest enemy to a standoff distance, drift
    /// sideways (a seeded choice that changes every [`AiParams::drift_ticks`] ticks) and fire
    /// while it is ahead and in range.
    pub(super) fn run_ai(&mut self) {
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
        let epoch = self.tick / self.ai.drift_ticks;
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
                self.ai.standoff * range
            } else {
                self.ai.unarmed_standoff
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
            let fire = range > 0.0 && error.abs() < self.ai.fire_cone && distance <= range;
            chosen.push((
                pilot_id,
                ControlState {
                    thrust: quantise(thrust),
                    strafe: quantise(sideways),
                    turn: quantise(self.ai.turn_gain * error),
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
    /// shooter id. A ship with no hit points left keeps its body until the physics step of
    /// the tick is over ([`Sim::remove_destroyed`]): Rapier's island manager panics if a
    /// body is removed before its world has taken a first real step.
    pub(super) fn fire_weapons(&mut self) {
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
            }
        }
    }

    /// Removes the bodies of ships with no hit points left, in ascending ship id, after the
    /// physics step of the tick. Until then a destroyed ship still takes part in the step.
    pub(super) fn remove_destroyed(&mut self) {
        for ship in self.ships.values_mut() {
            if ship.hit_points <= 0.0
                && let Some(handle) = ship.body.take()
                && let Some(world) = self.worlds.get_mut(&ship.system)
            {
                world.remove_ship(handle);
            }
        }
    }
}
