//! The `space_envelope` data kind: the PD-31 speed and scale numbers.

use serde::{Deserialize, Serialize};
use st_data::{Genesis, MigrationError, Schema, Validate, Validator};

/// Every coordinate stays within this many metres of the system origin
/// (2^23 m, ADR 0007, "Simulation validity bound").
///
/// This is an engineering limit from the ADR, not game data, so it is a
/// constant. The envelope is checked against it whenever it changes (PD-31,
/// "Consequences").
pub const VALIDITY_BOUND_M: f64 = 8_388_608.0;

/// How far apart neighbouring bodies are, in kilometres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpacingKm {
    /// The closest pair of neighbouring bodies.
    pub min: f64,
    /// The farthest pair of neighbouring bodies.
    pub max: f64,
}

/// A real-world speed used as a yardstick in the printout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSpeed {
    /// What it is, as it reads after "times", for example `a jet airliner`.
    pub name: String,
    /// Its speed in metres per second.
    pub mps: f64,
}

/// A real-world length used as a yardstick in the printout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceLength {
    /// What it is, as it reads after "times", for example `a marathon`.
    pub name: String,
    /// Its length in kilometres.
    pub km: f64,
}

/// The speed and scale envelope of PD-31, version 1 of the kind
/// `space_envelope`.
///
/// Field names carry their units. The simulation reads only the first six
/// fields; the yardsticks are for the printout.
///
/// ```
/// use phase1_demo::SpaceEnvelope;
/// use st_data::Loader;
///
/// let text = br#"(kind: "space_envelope", version: 1, data: (
///     normal_speed_mps: 1000.0, cruise_multiplier: 10.0, system_radius_km: 2000.0,
///     body_spacing_km: (min: 60.0, max: 120.0), acceleration_mps2: 100.0,
///     replay_seed: 31, reference_speeds: [], reference_lengths: []))"#;
/// let loaded = Loader::new().load::<SpaceEnvelope>("pd-31.ron", text).unwrap();
/// assert_eq!(loaded.value.normal_speed_mps, 1000.0);
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceEnvelope {
    /// Normal flight speed in metres per second.
    pub normal_speed_mps: f64,
    /// Cruise speed divided by normal speed.
    pub cruise_multiplier: f64,
    /// Distance from the star to the system edge, in kilometres.
    pub system_radius_km: f64,
    /// Distance between neighbouring bodies.
    pub body_spacing_km: SpacingKm,
    /// Acceleration of the replay's test ships, in metres per second squared.
    pub acceleration_mps2: f64,
    /// World seed of the replay.
    pub replay_seed: u64,
    /// Speed yardsticks for the printout.
    pub reference_speeds: Vec<ReferenceSpeed>,
    /// Length yardsticks for the printout.
    pub reference_lengths: Vec<ReferenceLength>,
}

impl SpaceEnvelope {
    /// Cruise speed in metres per second.
    pub fn cruise_speed_mps(&self) -> f64 {
        self.normal_speed_mps * self.cruise_multiplier
    }
}

impl Schema for SpaceEnvelope {
    const KIND: &'static str = "space_envelope";
    const VERSION: u32 = 1;
    type Previous = Genesis;
    fn migrate(previous: Genesis) -> Result<Self, MigrationError> {
        match previous {}
    }
}

impl Validate for SpaceEnvelope {
    fn validate(&self, v: &mut Validator) {
        v.field("normal_speed_mps", |v| {
            if self.normal_speed_mps <= 0.0 {
                v.error_with_help(
                    "space_envelope.speed_positive",
                    "normal speed must be above zero",
                    "write a speed in metres per second, for example 1000.0",
                );
            }
        });
        v.field("cruise_multiplier", |v| {
            if self.cruise_multiplier < 1.0 {
                v.error_with_help(
                    "space_envelope.cruise_not_slower",
                    "cruise speed must not be below normal speed",
                    "the multiplier must be 1.0 or more",
                );
            }
        });
        v.field("system_radius_km", |v| {
            if self.system_radius_km <= 0.0 {
                v.error("space_envelope.radius_positive", "the radius must be above zero");
            } else if self.system_radius_km * 1000.0 > VALIDITY_BOUND_M {
                v.error_with_help(
                    "space_envelope.within_validity_bound",
                    "the radius is outside the simulation validity bound",
                    "ADR 0007 keeps every coordinate within 8,388,608 m of the origin, so the radius must be at most 8,388 km",
                );
            }
        });
        v.field("body_spacing_km", |v| {
            v.field("min", |v| {
                if self.body_spacing_km.min <= 0.0 {
                    v.error(
                        "space_envelope.spacing_positive",
                        "the spacing must be above zero",
                    );
                }
            });
            v.field("max", |v| {
                if self.body_spacing_km.max < self.body_spacing_km.min {
                    v.error_with_help(
                        "space_envelope.spacing_order",
                        "max is below min",
                        "swap the two numbers",
                    );
                } else if self.body_spacing_km.max > self.system_radius_km {
                    v.error(
                        "space_envelope.spacing_fits",
                        "bodies cannot be farther apart than the system radius",
                    );
                }
            });
        });
        v.field("acceleration_mps2", |v| {
            if self.acceleration_mps2 <= 0.0 {
                v.error(
                    "space_envelope.acceleration_positive",
                    "the acceleration must be above zero",
                );
            }
        });
        v.field("reference_speeds", |v| {
            for (i, r) in self.reference_speeds.iter().enumerate() {
                v.index(i, |v| check_reference(v, &r.name, "mps", r.mps));
            }
        });
        v.field("reference_lengths", |v| {
            for (i, r) in self.reference_lengths.iter().enumerate() {
                v.index(i, |v| check_reference(v, &r.name, "km", r.km));
            }
        });
    }
}

fn check_reference(v: &mut Validator, name: &str, field: &str, value: f64) {
    v.field("name", |v| {
        if name.trim().is_empty() {
            v.error("space_envelope.reference_name", "a yardstick needs a name");
        }
    });
    v.field(field, |v| {
        if value <= 0.0 {
            v.error(
                "space_envelope.reference_positive",
                "a yardstick must be above zero",
            );
        }
    });
}
