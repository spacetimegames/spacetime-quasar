//! Human-graspable figures derived from the envelope (PD-31: let the owner
//! feel the numbers). Display only: nothing here feeds the simulation.

use std::fmt::Write as _;

use crate::envelope::{ReferenceLength, ReferenceSpeed, SpaceEnvelope};

/// A number for people: thousands separators and a precision that suits its
/// size (`1,234`, `12.3`, `1.23`, `0.16`).
///
/// ```
/// assert_eq!(phase1_demo::figures::number(1234567.0), "1,234,567");
/// assert_eq!(phase1_demo::figures::number(12.34), "12.3");
/// assert_eq!(phase1_demo::figures::number(0.157), "0.16");
/// ```
pub fn number(x: f64) -> String {
    if !x.is_finite() {
        return "a huge number of".to_owned();
    }
    let size = x.abs();
    if size != 0.0 && size < 0.0001 {
        return format!("{x:.2e}");
    }
    let decimals = if size >= 100.0 || x.fract() == 0.0 {
        0
    } else if size >= 10.0 {
        1
    } else if size >= 0.01 {
        2
    } else {
        4
    };
    let text = format!("{x:.decimals$}");
    let (sign, rest) = text
        .strip_prefix('-')
        .map_or(("", text.as_str()), |r| ("-", r));
    let (whole, fraction) = rest.split_once('.').map_or((rest, ""), |(w, f)| (w, f));
    let mut grouped = String::new();
    for (i, digit) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if fraction.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{fraction}")
    }
}

/// A duration for people: `6.0 s`, `45 s`, `1 min 40 s`, `2 h 46 min`,
/// `3 d 04 h`.
///
/// ```
/// use phase1_demo::figures::duration;
/// assert_eq!(duration(6.0), "6.0 s");
/// assert_eq!(duration(100.0), "1 min 40 s");
/// assert_eq!(duration(2000.0), "33 min 20 s");
/// assert_eq!(duration(9960.0), "2 h 46 min");
/// ```
pub fn duration(seconds: f64) -> String {
    if !seconds.is_finite() {
        return "forever".to_owned();
    }
    if seconds < 10.0 {
        return format!("{seconds:.1} s");
    }
    if seconds < 60.0 {
        return format!("{seconds:.0} s");
    }
    let total = seconds.round() as u64;
    if total < 3600 {
        return format!("{} min {:02} s", total / 60, total % 60);
    }
    let minutes = (seconds / 60.0).round() as u64;
    if minutes < 1440 {
        format!("{} h {:02} min", minutes / 60, minutes % 60)
    } else {
        let hours = (seconds / 3600.0).round() as u64;
        format!("{} d {:02} h", hours / 24, hours % 24)
    }
}

fn kmh(mps: f64) -> f64 {
    mps * 3.6
}

/// One line per yardstick: `4.00 x a jet airliner (900 km/h)`.
fn speed_comparisons(mps: f64, references: &[ReferenceSpeed]) -> Vec<String> {
    references
        .iter()
        .map(|r| {
            format!(
                "{} x {} ({} km/h)",
                number(mps / r.mps),
                r.name,
                number(kmh(r.mps))
            )
        })
        .collect()
}

fn length_comparisons(km: f64, references: &[ReferenceLength]) -> Vec<String> {
    references
        .iter()
        .map(|r| format!("{} x {} ({} km)", number(km / r.km), r.name, number(r.km)))
        .collect()
}

/// The validated values, as the file states them.
///
/// ```
/// # use phase1_demo::{SpaceEnvelope, figures};
/// # use st_data::Loader;
/// # let text = br#"(kind: "space_envelope", version: 1, data: (
/// #     normal_speed_mps: 1000.0, cruise_multiplier: 10.0, system_radius_km: 2000.0,
/// #     body_spacing_km: (min: 60.0, max: 120.0), acceleration_mps2: 100.0,
/// #     replay_seed: 31, reference_speeds: [], reference_lengths: []))"#;
/// # let env = Loader::new().load::<SpaceEnvelope>("pd-31.ron", text).unwrap().value;
/// let text = figures::values(&env);
/// assert!(text.contains("normal speed       1,000 m/s"));
/// ```
pub fn values(env: &SpaceEnvelope) -> String {
    let mut out = String::new();
    let mut line = |name: &str, value: String| {
        // Writing to a `String` cannot fail.
        let _ = writeln!(out, "    {name:<19}{value}");
    };
    line(
        "normal speed",
        format!("{} m/s", number(env.normal_speed_mps)),
    );
    line("cruise multiplier", number(env.cruise_multiplier));
    line(
        "system radius",
        format!("{} km", number(env.system_radius_km)),
    );
    line(
        "body spacing",
        format!(
            "{} to {} km",
            number(env.body_spacing_km.min),
            number(env.body_spacing_km.max)
        ),
    );
    line(
        "acceleration",
        format!("{} m/s^2", number(env.acceleration_mps2)),
    );
    line("replay seed", env.replay_seed.to_string());
    out
}

/// What the numbers feel like: speeds in everyday terms, and how long trips
/// take at a steady speed. Each line is indented four spaces.
///
/// ```
/// # use phase1_demo::{SpaceEnvelope, figures};
/// # use st_data::Loader;
/// # let text = br#"(kind: "space_envelope", version: 1, data: (
/// #     normal_speed_mps: 1000.0, cruise_multiplier: 10.0, system_radius_km: 2000.0,
/// #     body_spacing_km: (min: 60.0, max: 120.0), acceleration_mps2: 100.0,
/// #     replay_seed: 31, reference_speeds: [], reference_lengths: []))"#;
/// # let env = Loader::new().load::<SpaceEnvelope>("pd-31.ron", text).unwrap().value;
/// let text = figures::feel(&env);
/// assert!(text.contains("next body"));
/// ```
pub fn feel(env: &SpaceEnvelope) -> String {
    let normal = env.normal_speed_mps;
    let cruise = env.cruise_speed_mps();
    let radius_m = env.system_radius_km * 1000.0;
    let (near_m, far_m) = (
        env.body_spacing_km.min * 1000.0,
        env.body_spacing_km.max * 1000.0,
    );
    let mut out = String::new();
    let mut w = |text: String| {
        // Writing to a `String` cannot fail.
        let _ = writeln!(out, "    {text}");
    };
    for (label, mps) in [("normal speed", normal), ("cruise speed", cruise)] {
        w(format!(
            "{label}: {} m/s = {} km/h",
            number(mps),
            number(kmh(mps))
        ));
        for line in speed_comparisons(mps, &env.reference_speeds) {
            w(format!("    = {line}"));
        }
    }
    w(format!(
        "to get up to speed from rest: normal {} ({} km), cruise {} ({} km)",
        duration(normal / env.acceleration_mps2),
        number(normal * normal / (2.0 * env.acceleration_mps2) / 1000.0),
        duration(cruise / env.acceleration_mps2),
        number(cruise * cruise / (2.0 * env.acceleration_mps2) / 1000.0),
    ));
    for (label, mps) in [("at normal speed", normal), ("at cruise speed", cruise)] {
        w(format!(
            "{label}: to the next body {} to {}; centre to edge {}; edge to edge {}",
            duration(near_m / mps),
            duration(far_m / mps),
            duration(radius_m / mps),
            duration(2.0 * radius_m / mps),
        ));
    }
    w("(trip times assume the speed is held the whole way; the replay below includes speeding up and slowing down)".to_owned());
    w("the system radius is:".to_owned());
    for line in length_comparisons(env.system_radius_km, &env.reference_lengths) {
        w(format!("    = {line}"));
    }
    out
}
