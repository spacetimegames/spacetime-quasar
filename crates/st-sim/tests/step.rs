//! Behaviour of `Sim::step`: validation, atomicity, determinism and the fixed
//! step clock.

use st_math::glam::DVec3;
use st_sim::{
    AiParams, Command, CommandKind, ControlState, PilotId, Setup, ShipInit, Sim, StepError,
    command, scenarios,
};
use st_time::{DT, FixedStep, Nanos};

fn setup() -> Setup {
    Setup {
        ships: vec![ShipInit {
            id: 7,
            system: 0,
            position: DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            heading: 0.0,
            max_accel: 10.0,
            max_turn_rate: 1.0,
            drag: 0.0,
            ..ShipInit::default()
        }],
        pilots: vec![(PilotId(1), Some(7)), (PilotId(2), None)],
        ai_pilots: Vec::new(),
        gravity_mu: 0.0,
        max_speed_mps: 20_000.0,
        ai: AiParams {
            turn_gain: 3.0,
            fire_cone: 0.05,
            standoff: 0.5,
            unarmed_standoff: 100.0,
            drift_ticks: 240,
        },
    }
}

fn cmd(tick: u64, pilot: u64, seq: u64, thrust: i16) -> Command {
    Command {
        tick,
        pilot: PilotId(pilot),
        seq,
        kind: CommandKind::Controls(ControlState {
            thrust,
            ..ControlState::default()
        }),
    }
}

#[test]
fn thrust_accelerates_along_the_heading() {
    let mut sim = Sim::new(&setup(), 0).unwrap();
    sim.step(&[cmd(0, 1, 1, 32767)]).unwrap();
    let (p, v) = sim.ship_state(7).unwrap();
    // Heading 0 is +Z (ADR 0007). The body goes through Rapier, so compare with a
    // tolerance rather than bit for bit.
    assert!(
        (v - DVec3::new(0.0, 0.0, 10.0 * DT)).length() < 1e-12,
        "{v:?}"
    );
    assert!(
        (p - DVec3::new(1000.0, 0.0, 10.0 * DT * DT)).length() < 1e-9,
        "{p:?}"
    );
}

#[test]
fn held_controls_persist_without_new_commands() {
    let mut sim = Sim::new(&setup(), 0).unwrap();
    sim.step(&[cmd(0, 1, 1, 32767)]).unwrap();
    let (_, v1) = sim.ship_state(7).unwrap();
    sim.step(&[]).unwrap();
    let (_, v2) = sim.ship_state(7).unwrap();
    assert!(v2.z > v1.z);
}

#[test]
fn rejects_bad_commands_and_leaves_state_unchanged() {
    let mut sim = Sim::new(&setup(), 5).unwrap();
    let before = sim.state_hash().unwrap();
    let bad = |slice: &[Command]| sim.clone().step(slice).unwrap_err();

    assert_eq!(bad(&[cmd(1, 1, 1, 0)]), StepError::WrongTick { index: 0 });
    assert_eq!(
        bad(&[cmd(0, 3, 1, 0)]),
        StepError::UnknownPilot { index: 0 }
    );
    assert_eq!(bad(&[cmd(0, 0, 1, 0)]), StepError::WrongSource { index: 0 });
    assert_eq!(
        bad(&[cmd(0, 1, 0, 0)]),
        StepError::StaleSequence { index: 0 }
    );
    assert_eq!(
        bad(&[cmd(0, 1, 1, i16::MIN)]),
        StepError::InvalidPayload { index: 0 }
    );
    assert_eq!(
        bad(&[cmd(0, 2, 1, 0), cmd(0, 1, 1, 0)]),
        StepError::NotSorted { index: 1 }
    );
    assert_eq!(
        bad(&[cmd(0, 1, 1, 0), cmd(0, 1, 1, 0)]),
        StepError::NotSorted { index: 1 }
    );
    let many = vec![cmd(0, 1, 1, 0); 1025];
    assert_eq!(bad(&many), StepError::TooManyCommands { count: 1025 });

    // A bad command after a good one: nothing is applied.
    let err = sim
        .step(&[cmd(0, 1, 1, 32767), cmd(0, 3, 1, 0)])
        .unwrap_err();
    assert_eq!(err, StepError::UnknownPilot { index: 1 });
    assert_eq!(sim.tick(), 0);
    assert_eq!(sim.state_hash().unwrap(), before);
}

#[test]
fn stale_sequence_after_apply_and_gaps_are_allowed() {
    let mut sim = Sim::new(&setup(), 0).unwrap();
    sim.step(&[cmd(0, 1, 5, 100)]).unwrap();
    assert_eq!(
        sim.step(&[cmd(1, 1, 5, 0)]),
        Err(StepError::StaleSequence { index: 0 })
    );
    sim.step(&[cmd(1, 1, 6, 0)]).unwrap();
}

#[test]
fn sort_makes_a_shuffled_tick_acceptable() {
    let mut sim = Sim::new(&setup(), 0).unwrap();
    let mut tick = vec![cmd(0, 2, 1, 5), cmd(0, 1, 2, 7), cmd(0, 1, 1, 6)];
    assert!(sim.clone().step(&tick).is_err());
    command::sort(&mut tick);
    sim.step(&tick).unwrap();
}

#[test]
fn setup_errors() {
    use st_sim::SetupError;
    let mut s = setup();
    s.pilots.push((PilotId(0), None));
    assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::ReservedPilot);
    let mut s = setup();
    s.pilots.push((PilotId(1), None));
    assert_eq!(
        Sim::new(&s, 0).unwrap_err(),
        SetupError::DuplicatePilot(PilotId(1))
    );
    let mut s = setup();
    s.pilots.push((PilotId(9), Some(99)));
    assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::UnknownShip(99));
    let mut s = setup();
    s.ships.push(s.ships[0].clone());
    assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::DuplicateShip(7));
    let mut s = setup();
    s.ships[0].heading = f64::NAN;
    assert_eq!(Sim::new(&s, 0).unwrap_err(), SetupError::NotFinite);
}

#[test]
fn the_same_inputs_give_the_same_hashes_and_a_clone_continues_identically() {
    let s = scenarios::swarm_controls();
    let a = s.run().unwrap();
    assert_eq!(a, s.run().unwrap());

    // Snapshot at tick 600 (a clone), continue both, compare.
    let mut sim = Sim::new(&s.setup, s.seed).unwrap();
    let mut log = s.commands.iter().peekable();
    let mut step = |sim: &mut Sim| {
        let t = sim.tick();
        let mut batch = Vec::new();
        while let Some(c) = log.next_if(|c| c.tick == t) {
            batch.push(c.clone());
        }
        sim.step(&batch).unwrap();
    };
    for _ in 0..600 {
        step(&mut sim);
    }
    let snapshot = sim.clone();
    for _ in 0..600 {
        step(&mut sim);
    }
    assert_eq!(
        sim.state_hash().unwrap(),
        a.iter().find(|c| c.tick == 1200).unwrap().hash
    );
    assert_eq!(snapshot.tick(), 600);
}

#[test]
fn fixed_step_clock_gives_the_same_ticks_whatever_the_frame_rate() {
    // Two seconds of real time as 10 ms frames and as 25 ms frames: the
    // clock is exact, so both are due 240 ticks and the same state results.
    let run = |frame_ms: u64, frames: u64| {
        let mut clock = FixedStep::default();
        let mut sim = Sim::new(&setup(), 0).unwrap();
        let mut seq = 1;
        for _ in 0..frames {
            for _ in 0..clock.advance(Nanos::from_millis(frame_ms)).ticks() {
                let c = if sim.tick() == 0 {
                    vec![cmd(0, 1, seq, 32767)]
                } else {
                    vec![]
                };
                seq += 1;
                sim.step(&c).unwrap();
            }
        }
        (sim.tick(), sim.state_hash().unwrap())
    };
    assert_eq!(run(10, 200), (240, run(25, 80).1));
    assert_eq!(run(25, 80).0, 240);
}

#[test]
fn the_default_float_environment_lets_a_step_run() {
    // Nothing in safe code can change the environment, so the failing
    // branch is covered by its message only.
    assert!(st_math::det::fp_env_is_default());
    let mut sim = Sim::new(&setup(), 0).unwrap();
    assert_eq!(sim.step(&[]), Ok(()));
    assert_eq!(sim.tick(), 1);
    assert_eq!(
        StepError::FloatEnvironment.to_string(),
        "the floating-point environment is not the default"
    );
}
