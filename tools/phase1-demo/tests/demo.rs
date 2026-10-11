//! Behaviour of the demo's library: schema, figures and the replay.

use phase1_demo::{ReplayError, SpaceEnvelope, figures, scenario};
use st_data::{Code, Diagnostic, Loader};
use st_sim::Sim;

/// The shipped data file. Its values are the owner's to tune (PD-31), so the
/// tests below that pin numbers use `FIXTURE` instead.
const SHIPPED: &[u8] = include_bytes!("../../../assets/space_envelope/pd-31.ron");

fn text(normal: &str, radius: &str, spacing: &str, extra: &str) -> String {
    format!(
        r#"(kind: "space_envelope", version: 1, data: (
    normal_speed_mps: {normal}, cruise_multiplier: 10.0, system_radius_km: {radius},
    body_spacing_km: {spacing}, acceleration_mps2: 100.0, replay_seed: 31,
    reference_speeds: [(name: "a jet airliner", mps: 250.0){extra}],
    reference_lengths: [(name: "a marathon", km: 42.195)]))"#
    )
}

fn fixture() -> String {
    text("1000.0", "2000.0", "(min: 60.0, max: 120.0)", "")
}

fn load(text: &str) -> Result<SpaceEnvelope, Vec<Diagnostic>> {
    Loader::new()
        .load::<SpaceEnvelope>("test.ron", text.as_bytes())
        .map(|l| l.value)
}

fn envelope(text: &str) -> SpaceEnvelope {
    load(text).expect("the fixture is valid")
}

#[test]
fn the_shipped_file_is_valid_and_replays() {
    let env = Loader::new()
        .load::<SpaceEnvelope>("pd-31.ron", SHIPPED)
        .expect("assets/space_envelope/pd-31.ron must load")
        .value;
    let replay = scenario::replay(&env).expect("the shipped values must replay");
    assert!(replay.ticks > 0);
    assert!(!figures::feel(&env).is_empty());
}

#[test]
fn the_same_data_gives_the_same_hash() {
    let env = envelope(&fixture());
    let a = scenario::replay(&env).unwrap();
    let b = scenario::replay(&env).unwrap();
    assert_eq!(a, b);
}

/// The recorded hash of the fixture. It changes only when the simulation, its
/// hash format or the scenario builder changes behaviour; it is the same on
/// every platform (ADR 0008).
#[test]
fn the_fixture_hash_is_pinned() {
    let replay = scenario::replay(&envelope(&fixture())).unwrap();
    assert_eq!(replay.ticks, 481_200);
    assert_eq!(format!("{:016x}", replay.hash), "8c2b6ba222156ecd");
}

/// The hash the try-note (docs/handoff/try/phase-1.adoc) tells the owner to expect after
/// editing `normal_speed_mps` to 500.0, so that the note cannot drift from the code.
#[test]
fn the_try_note_hash_at_500_is_pinned() {
    let env = envelope(&text("500.0", "2000.0", "(min: 60.0, max: 120.0)", ""));
    let replay = scenario::replay(&env).unwrap();
    assert_eq!(format!("{:016x}", replay.hash), "89f12b6e933fea4f");
}

#[test]
fn a_changed_value_changes_the_hash_and_a_changed_yardstick_does_not() {
    let base = scenario::replay(&envelope(&fixture())).unwrap().hash;
    let faster = text("1100.0", "2000.0", "(min: 60.0, max: 120.0)", "");
    assert_ne!(scenario::replay(&envelope(&faster)).unwrap().hash, base);
    let reseeded = fixture().replace("replay_seed: 31", "replay_seed: 32");
    assert_ne!(scenario::replay(&envelope(&reseeded)).unwrap().hash, base);
    // A yardstick and the body spacing are for the printout; the replay does
    // not read them.
    let yardstick = fixture().replace("250.0", "260.0");
    assert_eq!(scenario::replay(&envelope(&yardstick)).unwrap().hash, base);
    let spacing = text("1000.0", "2000.0", "(min: 50.0, max: 110.0)", "");
    assert_eq!(scenario::replay(&envelope(&spacing)).unwrap().hash, base);
}

/// The figures claim how long an edge-to-edge trip takes; the simulation must
/// agree, and both ships must stop at the far edge.
#[test]
fn the_simulation_agrees_with_the_derived_trip_times() {
    let env = envelope(&fixture());
    let s = scenario::build(&env).unwrap();
    let mut sim = Sim::new(&s.setup, s.seed).unwrap();
    let mut next = 0;
    let mut cruise_arrived = None;
    while sim.tick() < s.ticks {
        let tick = sim.tick();
        let start = next;
        while next < s.commands.len() && s.commands[next].tick <= tick {
            next += 1;
        }
        sim.step(&s.commands[start..next]).unwrap();
        let (cruise, _) = sim.ship_state(2).unwrap();
        if cruise_arrived.is_none() && cruise.z >= 0.0 && sim.tick() > 2 {
            cruise_arrived = Some(sim.tick());
        }
    }
    let radius = 2_000_000.0;
    for id in [1, 2] {
        let (position, velocity) = sim.ship_state(id).unwrap();
        assert!(
            (position.z - radius).abs() < 100.0,
            "ship {id} at {}",
            position.z
        );
        assert!(
            velocity.length() < 0.1,
            "ship {id} still moving: {}",
            velocity.length()
        );
        assert_eq!(position.y, 0.0);
    }
    // The cruise ship crosses the middle at half its trip time (the trip is
    // symmetric), 250 s for the fixture.
    let half = cruise_arrived.unwrap() as f64 / 120.0;
    assert!(
        (half - 250.0).abs() < 1.0,
        "cruise ship at the centre after {half} s"
    );
}

/// PD-31 is provisional: no code constant may cap the speed an owner puts in the data. With
/// a cruise speed above the old 20 km/s constant, the cruise ship must reach it and arrive.
#[test]
fn a_cruise_speed_above_20_km_per_s_is_not_clamped() {
    let env = envelope(
        r#"(kind: "space_envelope", version: 1, data: (
    normal_speed_mps: 1000.0, cruise_multiplier: 30.0, system_radius_km: 2000.0,
    body_spacing_km: (min: 60.0, max: 120.0), acceleration_mps2: 1000.0, replay_seed: 31,
    reference_speeds: [], reference_lengths: []))"#,
    );
    assert_eq!(env.cruise_speed_mps(), 30_000.0);
    let s = scenario::build(&env).unwrap();
    assert_eq!(s.setup.max_speed_mps, 60_000.0);
    let mut sim = Sim::new(&s.setup, s.seed).unwrap();
    let mut next = 0;
    let mut peak = 0.0_f64;
    while sim.tick() < s.ticks {
        let tick = sim.tick();
        let start = next;
        while next < s.commands.len() && s.commands[next].tick <= tick {
            next += 1;
        }
        sim.step(&s.commands[start..next]).unwrap();
        let speed = sim.ship_state(2).unwrap().1.length();
        if speed > peak {
            peak = speed;
        }
    }
    assert!(peak > 29_000.0, "the cruise ship peaked at {peak} m/s");
    let (position, velocity) = sim.ship_state(2).unwrap();
    // Switching thrust on a tick boundary costs up to v * DT = 250 m at this speed.
    assert!(
        (position.z - 2_000_000.0).abs() < 1_000.0,
        "at {}",
        position.z
    );
    assert!(velocity.length() < 0.1);
}

#[test]
fn a_short_trip_is_triangular_and_still_arrives() {
    // 10 km radius, speeds far above what 100 m/s^2 reaches in 20 km.
    let env = envelope(&text("1000.0", "10.0", "(min: 1.0, max: 5.0)", ""));
    let s = scenario::build(&env).unwrap();
    let mut sim = Sim::new(&s.setup, s.seed).unwrap();
    let mut next = 0;
    while sim.tick() < s.ticks {
        let tick = sim.tick();
        let start = next;
        while next < s.commands.len() && s.commands[next].tick <= tick {
            next += 1;
        }
        sim.step(&s.commands[start..next]).unwrap();
    }
    let (position, velocity) = sim.ship_state(1).unwrap();
    assert!((position.z - 10_000.0).abs() < 5.0, "at {}", position.z);
    assert!(velocity.length() < 0.1);
}

#[test]
fn a_replay_that_would_take_too_long_is_refused_with_a_reason() {
    let env = envelope(&text("1.0", "2000.0", "(min: 60.0, max: 120.0)", ""));
    let err = scenario::replay(&env).unwrap_err();
    assert!(matches!(err, ReplayError::TooLong { .. }));
    assert!(err.to_string().contains("more than"));
}

#[test]
fn validation_rejects_values_that_break_a_rule() {
    let cases: &[(String, &str)] = &[
        (
            text("0.0", "2000.0", "(min: 60.0, max: 120.0)", ""),
            "speed_positive",
        ),
        (
            text("-5.0", "2000.0", "(min: 60.0, max: 120.0)", ""),
            "speed_positive",
        ),
        (
            text("1000.0", "0.0", "(min: 60.0, max: 120.0)", ""),
            "radius_positive",
        ),
        (
            text("1000.0", "9000.0", "(min: 60.0, max: 120.0)", ""),
            "within_validity_bound",
        ),
        (
            text("1000.0", "2000.0", "(min: 0.0, max: 120.0)", ""),
            "spacing_positive",
        ),
        (
            text("1000.0", "2000.0", "(min: 130.0, max: 120.0)", ""),
            "spacing_order",
        ),
        (
            text("1000.0", "100.0", "(min: 60.0, max: 120.0)", ""),
            "spacing_fits",
        ),
        (
            fixture().replace("cruise_multiplier: 10.0", "cruise_multiplier: 0.5"),
            "cruise_not_slower",
        ),
        (
            fixture().replace("acceleration_mps2: 100.0", "acceleration_mps2: 0.0"),
            "acceleration_positive",
        ),
        (
            fixture().replace("\"a marathon\"", "\" \""),
            "reference_name",
        ),
        (
            fixture().replace("km: 42.195", "km: 0.0"),
            "reference_positive",
        ),
    ];
    for (text, rule) in cases {
        let diagnostics = load(text).expect_err(rule);
        assert!(
            diagnostics.iter().any(|d| d.code == Code::Validation
                && d.rule.as_deref() == Some(&format!("space_envelope.{rule}"))),
            "expected {rule}, got {diagnostics:?}"
        );
    }
}

#[test]
fn a_typo_is_reported_with_its_position() {
    let bad = fixture().replace("normal_speed_mps", "normal_speed");
    let diagnostics = load(&bad).unwrap_err();
    let shown = diagnostics[0].to_string();
    assert!(shown.contains("test.ron:"), "{shown}");
    assert!(shown.contains("unknown-field"), "{shown}");
}

#[test]
fn figures_state_the_pd31_trip_times_for_the_fixture() {
    let env = envelope(&fixture());
    let feel = figures::feel(&env);
    assert!(
        feel.contains("at normal speed: to the next body 1 min 00 s to 2 min 00 s"),
        "{feel}"
    );
    assert!(feel.contains("edge to edge 6 min 40 s"), "{feel}");
    assert!(feel.contains("= 4 x a jet airliner (900 km/h)"), "{feel}");
    assert!(feel.contains("1,000 m/s = 3,600 km/h"), "{feel}");
}

#[test]
fn number_and_duration_formats() {
    assert_eq!(figures::number(0.0), "0");
    assert_eq!(figures::number(-1234.6), "-1,235");
    assert_eq!(figures::number(999.4), "999");
    assert_eq!(figures::number(1.0 / 0.0), "a huge number of");
    assert_eq!(figures::number(0.0052), "0.0052");
    assert_eq!(figures::duration(0.5), "0.5 s");
    assert_eq!(figures::duration(59.4), "59 s");
    assert_eq!(figures::duration(3599.7), "1 h 00 min");
    assert_eq!(figures::duration(4000.0), "1 h 07 min");
    assert_eq!(figures::duration(200_000.0), "2 d 08 h");
}
