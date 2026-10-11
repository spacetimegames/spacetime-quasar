//! The session end to end: real files, a fake clock.
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;

use phase1_demo::{REMINDER_EVERY, Session};
use st_time::Nanos;

fn text(speed: &str) -> String {
    format!(
        r#"(kind: "space_envelope", version: 1, data: (
    normal_speed_mps: {speed}, cruise_multiplier: 10.0, system_radius_km: 2000.0,
    body_spacing_km: (min: 60.0, max: 120.0), acceleration_mps2: 100.0, replay_seed: 31,
    reference_speeds: [(name: "a jet airliner", mps: 250.0)],
    reference_lengths: [(name: "a marathon", km: 42.195)]))"#
    )
}

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let path = std::env::temp_dir().join(format!("phase1-demo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Dir(path)
    }
    fn write(&self, file: &str, text: &str) {
        std::fs::write(self.0.join(file), text).unwrap();
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Polls once a second of fake time until something is reported.
fn next_event(session: &mut Session, now: &mut Nanos) -> String {
    for _ in 0..200 {
        *now = Nanos::new(now.as_nanos() + 1_000_000_000);
        if let Some(text) = session.poll(*now) {
            return text;
        }
    }
    panic!("no event");
}

#[test]
fn a_valid_edit_prints_the_new_values_and_a_new_hash() {
    let dir = Dir::new("valid");
    dir.write("a.ron", &text("1000.0"));
    let mut session = Session::open(&dir.0).unwrap();
    let first = session.report("started");
    assert!(first.contains("final state hash:"));
    let hash_of = |t: &str| {
        t.lines()
            .find(|l| l.contains("final state hash:"))
            .unwrap()
            .to_owned()
    };

    // Same data, same hash.
    assert_eq!(hash_of(&first), hash_of(&session.report("again")));

    dir.write("a.ron", &text("1200.0"));
    let mut now = Nanos::ZERO;
    let event = next_event(&mut session, &mut now);
    assert!(event.contains("RELOADED"), "{event}");
    assert!(event.contains("1,200 m/s"), "{event}");
    assert_ne!(hash_of(&event), hash_of(&first));
    assert_eq!(session.generation(), 1);
    assert!(session.last_error().is_none());
}

#[test]
fn a_bad_edit_keeps_the_old_values_and_the_error_until_a_good_edit() {
    let dir = Dir::new("bad");
    dir.write("a.ron", &text("1000.0"));
    let mut session = Session::open(&dir.0).unwrap();
    let mut now = Nanos::ZERO;

    dir.write("a.ron", &text("-5.0"));
    let event = next_event(&mut session, &mut now);
    assert!(event.contains("EDIT REJECTED"), "{event}");
    assert!(event.contains("a.ron:"), "the file is named: {event}");
    assert!(event.contains("speed_positive"), "{event}");
    assert!(
        event.contains("1,000 m/s"),
        "the old value is still shown: {event}"
    );
    assert!(event.contains("OLD values"), "{event}");
    assert_eq!(session.generation(), 0);
    assert!(session.last_error().is_some());

    // Nothing new happens, but the error is shown again after a while.
    let mut reminded = None;
    for _ in 0..20 {
        now = Nanos::new(now.as_nanos() + 1_000_000_000);
        if let Some(t) = session.poll(now) {
            reminded = Some(t);
            break;
        }
    }
    let reminder = reminded.expect("a reminder");
    assert!(reminder.contains("STILL WAITING"), "{reminder}");
    assert!(reminder.contains("speed_positive"), "{reminder}");
    assert!(now >= REMINDER_EVERY);

    // A syntax error replaces the standing error.
    dir.write(
        "a.ron",
        "(kind: \"space_envelope\", version: 1, data: (oops",
    );
    let event = next_event(&mut session, &mut now);
    assert!(
        event.contains("EDIT REJECTED") && event.contains("syntax"),
        "{event}"
    );

    dir.write("a.ron", &text("1500.0"));
    let event = next_event(&mut session, &mut now);
    assert!(
        event.contains("RELOADED") && event.contains("1,500 m/s"),
        "{event}"
    );
    assert!(!event.contains("LAST RELOAD ERROR"), "{event}");
    assert!(session.last_error().is_none());
    assert_eq!(session.generation(), 1);
}

/// A good file A and a bad file B in one batch: nothing changes, and fixing B
/// brings A along (SPAC-124).
#[test]
fn fixing_the_bad_file_brings_the_good_file_of_the_same_batch_along() {
    let dir = Dir::new("batch");
    dir.write("a.ron", &text("1000.0"));
    dir.write("b.ron", &text("1000.0"));
    let mut session = Session::open(&dir.0).unwrap();
    let mut now = Nanos::ZERO;

    dir.write("a.ron", &text("1100.0"));
    dir.write("b.ron", &text("-1.0"));
    let event = next_event(&mut session, &mut now);
    assert!(event.contains("EDIT REJECTED"), "{event}");
    assert!(event.contains("b.ron:"), "{event}");
    assert!(
        !event.contains("1,100 m/s"),
        "A must not apply alone: {event}"
    );
    assert_eq!(session.generation(), 0);

    dir.write("b.ron", &text("1300.0"));
    let event = next_event(&mut session, &mut now);
    assert!(event.contains("RELOADED"), "{event}");
    assert!(event.contains("1,100 m/s"), "A came along: {event}");
    assert!(event.contains("1,300 m/s"), "{event}");
    assert_eq!(session.generation(), 1);
    assert!(session.last_error().is_none());
}

#[test]
fn a_bad_file_at_start_is_an_error_with_its_path() {
    let dir = Dir::new("start");
    dir.write("a.ron", "(kind: \"space_envelope\"");
    let error = Session::open(&dir.0).err().expect("must fail");
    assert!(error.contains("a.ron:"), "{error}");
}

/// The owner-found bug (SPAC-108): reject, then restore the identical file.
/// Nothing in the values changes, yet the error must be cleared and said so.
#[test]
fn restoring_the_same_file_after_a_bad_edit_clears_the_error_and_says_so() {
    let dir = Dir::new("recover");
    dir.write("a.ron", &text("1000.0"));
    let mut session = Session::open(&dir.0).unwrap();
    let mut now = Nanos::ZERO;

    // A value that breaks a rule, then the identical original.
    dir.write("a.ron", &text("-5.0"));
    let event = next_event(&mut session, &mut now);
    assert!(event.contains("EDIT REJECTED"), "{event}");
    dir.write("a.ron", &text("1000.0"));
    let event = next_event(&mut session, &mut now);
    assert!(event.contains("RECOVERED"), "{event}");
    assert!(event.contains("a.ron is valid again"), "{event}");
    assert!(event.contains("The error is cleared."), "{event}");
    assert!(!event.contains("LAST RELOAD ERROR"), "{event}");
    assert!(!event.contains("OLD values"), "{event}");
    assert!(session.last_error().is_none());
    assert_eq!(session.generation(), 0);

    // The reminder is gone with the error: nothing is printed for a while.
    for _ in 0..30 {
        now = Nanos::new(now.as_nanos() + 1_000_000_000);
        assert!(session.poll(now).is_none(), "no more STILL WAITING");
    }

    // The same with a syntax error.
    dir.write(
        "a.ron",
        "(kind: \"space_envelope\", version: 1, data: (oops",
    );
    let event = next_event(&mut session, &mut now);
    assert!(event.contains("syntax"), "{event}");
    dir.write("a.ron", &text("1000.0"));
    let event = next_event(&mut session, &mut now);
    assert!(event.contains("RECOVERED"), "{event}");
    assert!(session.last_error().is_none());
    assert_eq!(session.generation(), 0);
}
