//! Edits a temporary asset and watches `HotData` follow it.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "exempt from PD-06: ADR 0008"
)]

use serde::{Deserialize, Serialize};
use st_data::{Genesis, Loader, MigrationError, Schema, Validate, Validator};
use st_game::{HotData, ReloadReport};
use st_platform::{Clock, WatcherConfig};
use st_time::Nanos;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
struct Engine {
    thrust: f32,
}

impl Schema for Engine {
    const KIND: &'static str = "engine";
    const VERSION: u32 = 1;
    type Previous = Genesis;
    fn migrate(p: Genesis) -> Result<Self, MigrationError> {
        match p {}
    }
}

impl Validate for Engine {
    fn validate(&self, v: &mut Validator) {
        if self.thrust < 0.0 {
            v.field("thrust", |v| {
                v.error("engine.thrust", "must not be negative")
            });
        }
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!("st-game-hot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_file(dir: &Path, name: &str, thrust: &str) {
    let text = format!(r#"(kind: "engine", version: 1, data: (thrust: {thrust}))"#);
    std::fs::write(dir.join(name), text).unwrap();
}

fn write(dir: &Path, thrust: &str) {
    write_file(dir, "main.ron", thrust);
}

fn open(dir: &Path) -> HotData<Engine> {
    let config = WatcherConfig::new().quiet_period(Nanos::from_millis(50));
    HotData::<Engine>::open_with(dir, Loader::new(), config).unwrap()
}

/// Polls like a main loop would until a report satisfies `done`. The sleep only
/// keeps the test from spinning a core; the bound is generous so that slow
/// CI file systems pass.
fn wait_until(
    hot: &mut HotData<Engine>,
    clock: &Clock,
    mut done: impl FnMut(&ReloadReport) -> bool,
) -> ReloadReport {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(report) = hot.poll(clock.now())
            && done(&report)
        {
            return report;
        }
        assert!(Instant::now() < deadline, "no matching report in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn thrust(hot: &HotData<Engine>, name: &str) -> f32 {
    hot.snapshot().get(name).unwrap().thrust
}

#[test]
fn valid_edit_reloads_invalid_edit_keeps_old_value_and_valid_edit_recovers() {
    let dir = TempDir::new("flow");
    // Each edit differs in length: the watcher compares modification time and
    // length, and a coarse timestamp must not hide an edit.
    write(&dir.0, "1.5");
    let mut hot = open(&dir.0);
    let clock = Clock::new();
    assert_eq!(hot.generation(), 0);
    let before = hot.snapshot();
    assert_eq!(before.get("main.ron").unwrap().thrust, 1.5);

    // 1. A valid edit is picked up.
    write(&dir.0, "12.25");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert!(report.error.is_none());
    let reloaded = report.reloaded.expect("valid edit is accepted");
    assert_eq!(reloaded.generation, 1);
    assert_eq!(reloaded.events.len(), 1);
    assert_eq!(reloaded.events[0].source, "main.ron");
    assert!(reloaded.events[0].old_hash.is_some());
    assert_eq!(thrust(&hot, "main.ron"), 12.25);
    // A consumer that kept the old snapshot still sees the old, whole value.
    assert_eq!(before.get("main.ron").unwrap().thrust, 1.5);

    // 2. An invalid edit (syntax) keeps the value and reports the file.
    write(&dir.0, "12.25.7.9, oops");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert!(report.reloaded.is_none());
    let error = report.error.expect("syntax error is rejected");
    assert_eq!(error.diagnostics()[0].source, "main.ron");
    assert!(error.to_string().contains("main.ron"), "{error}");
    assert_eq!(hot.generation(), 1);
    assert_eq!(thrust(&hot, "main.ron"), 12.25);

    // 3. A validation failure is rejected the same way.
    write(&dir.0, "-3.0");
    let report = wait_until(&mut hot, &clock, |_| true);
    let error = report.error.expect("validation failure is rejected");
    assert_eq!(error.diagnostics()[0].code.as_str(), "validation");
    assert_eq!(thrust(&hot, "main.ron"), 12.25);

    // 4. A later valid edit recovers.
    write(&dir.0, "100.0");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert_eq!(report.reloaded.expect("recovers").generation, 2);
    assert_eq!(thrust(&hot, "main.ron"), 100.0);
}

#[test]
fn a_deleted_file_keeps_its_value_and_does_not_block_edits_to_other_files() {
    let dir = TempDir::new("delete");
    write_file(&dir.0, "a.ron", "1.5");
    write_file(&dir.0, "b.ron", "2.5");
    let mut hot = open(&dir.0);
    let clock = Clock::new();

    std::fs::remove_file(dir.0.join("a.ron")).unwrap();
    let report = wait_until(&mut hot, &clock, |r| r.error.is_some());
    let error = report.error.unwrap();
    assert_eq!(error.diagnostics()[0].source, "a.ron");
    assert_eq!(error.diagnostics()[0].code.as_str(), "missing-source");
    assert_eq!(thrust(&hot, "a.ron"), 1.5);

    // The removal is not reported again, and b still reloads.
    write_file(&dir.0, "b.ron", "33.75");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert!(report.error.is_none(), "{:?}", report.error);
    assert_eq!(report.reloaded.unwrap().events[0].source, "b.ron");
    assert_eq!(thrust(&hot, "b.ron"), 33.75);
    assert_eq!(thrust(&hot, "a.ron"), 1.5);
}

#[test]
fn a_file_deleted_and_created_again_recovers() {
    let dir = TempDir::new("recreate");
    write_file(&dir.0, "a.ron", "1.5");
    let mut hot = open(&dir.0);
    let clock = Clock::new();

    std::fs::remove_file(dir.0.join("a.ron")).unwrap();
    let report = wait_until(&mut hot, &clock, |r| r.error.is_some());
    assert!(report.reloaded.is_none());

    write_file(&dir.0, "a.ron", "42.125");
    let report = wait_until(&mut hot, &clock, |r| r.reloaded.is_some());
    assert!(report.error.is_none());
    assert_eq!(thrust(&hot, "a.ron"), 42.125);
}

#[test]
fn remove_then_create_in_one_save_ends_in_a_reload() {
    let dir = TempDir::new("resave");
    write_file(&dir.0, "a.ron", "1.5");
    let mut hot = open(&dir.0);
    let clock = Clock::new();

    // How many editors save: write a temporary file, then rename it over.
    write_file(&dir.0, "a.tmp", "77.5");
    std::fs::remove_file(dir.0.join("a.ron")).unwrap();
    std::fs::rename(dir.0.join("a.tmp"), dir.0.join("a.ron")).unwrap();
    let report = wait_until(&mut hot, &clock, |r| r.reloaded.is_some());
    assert_eq!(thrust(&hot, "a.ron"), 77.5);
    assert_eq!(report.reloaded.unwrap().generation, 1);
}

#[test]
fn a_bad_file_does_not_block_a_later_edit_to_another_file() {
    let dir = TempDir::new("isolated");
    write_file(&dir.0, "a.ron", "1.5");
    write_file(&dir.0, "b.ron", "2.5");
    let mut hot = open(&dir.0);
    let clock = Clock::new();

    write_file(&dir.0, "a.ron", "-1.0");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert!(report.error.is_some());
    write_file(&dir.0, "b.ron", "9.75");
    let report = wait_until(&mut hot, &clock, |r| r.reloaded.is_some());
    assert!(report.error.is_none(), "{:?}", report.error);
    assert_eq!(thrust(&hot, "b.ron"), 9.75);
    assert_eq!(thrust(&hot, "a.ron"), 1.5);
}

#[test]
fn a_bad_file_at_startup_is_an_error_with_the_file_name() {
    let dir = TempDir::new("startup");
    std::fs::write(dir.0.join("bad.ron"), "not ron at all").unwrap();
    let error = HotData::<Engine>::open(&dir.0, Loader::new()).unwrap_err();
    assert_eq!(error.diagnostics()[0].source, "bad.ron");
}

/// The owner-found bug (SPAC-108): a bad edit followed by a restore to content
/// identical to what is in use changes nothing in the snapshot, but it must
/// still end the problem and say so.
#[test]
fn restoring_identical_content_after_a_validation_error_reports_recovered() {
    let dir = TempDir::new("undo-validation");
    write(&dir.0, "1.5");
    let mut hot = open(&dir.0);
    let clock = Clock::new();

    write(&dir.0, "-3.0");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert!(report.error.is_some() && report.recovered.is_empty());
    assert!(hot.standing_error().is_some());

    // Back to exactly the original bytes.
    write(&dir.0, "1.5");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert_eq!(report.recovered, ["main.ron"]);
    assert!(report.error.is_none(), "{:?}", report.error);
    assert!(report.reloaded.is_none(), "the values did not change");
    assert!(hot.standing_error().is_none());
    assert_eq!(hot.generation(), 0);
    assert_eq!(thrust(&hot, "main.ron"), 1.5);
}

#[test]
fn restoring_identical_content_after_a_syntax_error_reports_recovered() {
    let dir = TempDir::new("undo-syntax");
    write(&dir.0, "1.5");
    let mut hot = open(&dir.0);
    let clock = Clock::new();

    write(&dir.0, "1.5,, oops");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert_eq!(
        report.error.unwrap().diagnostics()[0].code.as_str(),
        "syntax"
    );

    // A second bad save replaces the first problem; still one standing.
    write(&dir.0, "-3.0");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert!(report.error.is_some());
    assert_eq!(hot.standing_error().unwrap().diagnostics().len(), 1);

    write(&dir.0, "1.5");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert_eq!(report.recovered, ["main.ron"]);
    assert!(report.reloaded.is_none() && report.error.is_none());
    assert!(hot.standing_error().is_none());
    assert_eq!(hot.generation(), 0);
}

/// A good edit to another file does not end a file's problem, and a restore of
/// the bad file does, with the other file's newer value kept.
#[test]
fn a_standing_problem_survives_another_files_good_edit_until_its_own_file_loads() {
    let dir = TempDir::new("undo-two-files");
    write_file(&dir.0, "a.ron", "1.5");
    write_file(&dir.0, "b.ron", "2.5");
    let mut hot = open(&dir.0);
    let clock = Clock::new();

    write_file(&dir.0, "a.ron", "-1.0");
    wait_until(&mut hot, &clock, |r| r.error.is_some());

    write_file(&dir.0, "b.ron", "9.75");
    let report = wait_until(&mut hot, &clock, |r| r.reloaded.is_some());
    assert!(report.recovered.is_empty());
    let standing = hot.standing_error().expect("a.ron still has its problem");
    assert_eq!(standing.diagnostics()[0].source, "a.ron");

    write_file(&dir.0, "a.ron", "1.5");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert_eq!(report.recovered, ["a.ron"]);
    assert!(hot.standing_error().is_none());
    assert_eq!(hot.generation(), 1);
    assert_eq!(thrust(&hot, "b.ron"), 9.75);
}

/// A file removed while it has a problem keeps a `missing-source` problem
/// (its last value is still in use) and recovers when created again.
#[test]
fn a_file_removed_while_in_error_and_created_again_valid_recovers() {
    let dir = TempDir::new("remove-in-error");
    write(&dir.0, "1.5");
    let mut hot = open(&dir.0);
    let clock = Clock::new();

    write(&dir.0, "-3.0");
    wait_until(&mut hot, &clock, |r| r.error.is_some());
    std::fs::remove_file(dir.0.join("main.ron")).unwrap();
    let report = wait_until(&mut hot, &clock, |r| r.error.is_some());
    assert_eq!(
        report.error.unwrap().diagnostics()[0].code.as_str(),
        "missing-source"
    );
    let standing = hot.standing_error().expect("the removal stands");
    assert_eq!(standing.diagnostics().len(), 1);
    assert_eq!(standing.diagnostics()[0].code.as_str(), "missing-source");
    assert_eq!(thrust(&hot, "main.ron"), 1.5);

    write(&dir.0, "8.25");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert_eq!(report.recovered, ["main.ron"]);
    assert_eq!(report.reloaded.expect("new value").generation, 1);
    assert!(hot.standing_error().is_none());
    assert_eq!(thrust(&hot, "main.ron"), 8.25);
}

#[test]
fn an_error_followed_by_a_different_valid_edit_recovers_and_reloads() {
    let dir = TempDir::new("error-then-other");
    write(&dir.0, "1.5");
    let mut hot = open(&dir.0);
    let clock = Clock::new();

    write(&dir.0, "-3.0");
    wait_until(&mut hot, &clock, |r| r.error.is_some());
    assert!(hot.standing_error().is_some());

    write(&dir.0, "20.5");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert_eq!(report.recovered, ["main.ron"]);
    assert_eq!(report.reloaded.expect("new value").generation, 1);
    assert!(report.error.is_none());
    assert!(hot.standing_error().is_none());
    assert_eq!(thrust(&hot, "main.ron"), 20.5);
}

/// Good A and bad B saved together: nothing lands. When B is fixed later, A
/// lands with it and only B is reported as recovered.
#[test]
fn a_mixed_batch_lands_together_once_the_bad_file_is_fixed() {
    let dir = TempDir::new("mixed-batch");
    write_file(&dir.0, "a.ron", "1.5");
    write_file(&dir.0, "b.ron", "2.5");
    let mut hot = open(&dir.0);
    let clock = Clock::new();

    write_file(&dir.0, "a.ron", "11.25");
    write_file(&dir.0, "b.ron", "-1.0");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert!(report.reloaded.is_none());
    assert_eq!(report.error.unwrap().diagnostics()[0].source, "b.ron");
    assert_eq!(thrust(&hot, "a.ron"), 1.5);
    assert_eq!(hot.standing_error().unwrap().diagnostics().len(), 1);

    write_file(&dir.0, "b.ron", "33.75");
    let report = wait_until(&mut hot, &clock, |_| true);
    assert_eq!(report.recovered, ["b.ron"]);
    let reloaded = report.reloaded.expect("both land");
    assert_eq!(reloaded.generation, 1);
    let names: Vec<&str> = reloaded.events.iter().map(|e| e.source.as_str()).collect();
    assert_eq!(names, ["a.ron", "b.ron"]);
    assert_eq!(thrust(&hot, "a.ron"), 11.25);
    assert_eq!(thrust(&hot, "b.ron"), 33.75);
    assert!(hot.standing_error().is_none());
}
