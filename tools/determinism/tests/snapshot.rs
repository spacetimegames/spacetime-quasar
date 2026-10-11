//! A snapshot is `Sim::clone()` (ADR 0009, "Snapshots and seeking"): a clone
//! taken mid-run continues bit-identically to the run it was taken from.

use st_sim::scenarios;
use st_sim::{CHECKPOINT_INTERVAL, Command, Sim, command};

const SNAPSHOT_TICK: u64 = 240;
const END_TICK: u64 = 600;

fn step(sim: &mut Sim, commands: &[Command]) {
    let tick = sim.tick();
    let mut now: Vec<Command> = commands
        .iter()
        .filter(|c| c.tick == tick)
        .cloned()
        .collect();
    command::sort(&mut now);
    sim.step(&now)
        .expect("the scenario's commands are accepted");
}

#[test]
fn snapshot_continuity() {
    for s in scenarios::all() {
        let end = END_TICK.min(s.ticks);
        let mut original = Sim::new(&s.setup, s.seed).unwrap();
        while original.tick() < SNAPSHOT_TICK.min(end) {
            step(&mut original, &s.commands);
        }
        let mut snapshot = original.clone();
        let mut checkpoints = 0;
        while original.tick() < end {
            step(&mut original, &s.commands);
            step(&mut snapshot, &s.commands);
            if original.tick().is_multiple_of(CHECKPOINT_INTERVAL) || original.tick() == end {
                checkpoints += 1;
                assert_eq!(
                    original.state_hash().unwrap(),
                    snapshot.state_hash().unwrap(),
                    "{} diverged from its snapshot at tick {}",
                    s.name,
                    original.tick()
                );
            }
        }
        assert!(checkpoints > 0, "{} ran no checkpoint", s.name);
    }
}
