//! Edits a temporary asset and watches `HotData` follow it.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "exempt from PD-06: ADR 0008"
)]

use serde::{Deserialize, Serialize};
use st_data::{Genesis, Loader, MigrationError, Schema, Validate, Validator};
use st_game::{HotData, ReloadError, Reloaded};
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

fn write(dir: &Path, thrust: &str) {
    let text = format!(r#"(kind: "engine", version: 1, data: (thrust: {thrust}))"#);
    std::fs::write(dir.join("main.ron"), text).unwrap();
}

/// Polls like a main loop would, until a batch is reported. The sleep only
/// keeps the test from spinning a core; the bound is generous so that slow
/// CI file systems pass.
fn wait_for_report(hot: &mut HotData<Engine>, clock: &Clock) -> Result<Reloaded, ReloadError> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(result) = hot.poll(clock.now()) {
            return result;
        }
        assert!(Instant::now() < deadline, "no reload was reported in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn valid_edit_reloads_invalid_edit_keeps_old_value_and_valid_edit_recovers() {
    let dir = TempDir::new("flow");
    // Each edit differs in length: the watcher compares modification time and
    // length, and a coarse timestamp must not hide an edit.
    write(&dir.0, "1.5");
    let config = WatcherConfig::new().quiet_period(Nanos::from_millis(50));
    let mut hot = HotData::<Engine>::open_with(&dir.0, Loader::new(), config).unwrap();
    let clock = Clock::new();
    assert_eq!(hot.generation(), 0);
    let before = hot.snapshot();
    assert_eq!(before.get("main.ron").unwrap().thrust, 1.5);

    // 1. A valid edit is picked up.
    write(&dir.0, "12.25");
    let reloaded = wait_for_report(&mut hot, &clock).expect("valid edit is accepted");
    assert_eq!(reloaded.generation, 1);
    assert_eq!(reloaded.events.len(), 1);
    assert_eq!(reloaded.events[0].source, "main.ron");
    assert!(reloaded.events[0].old_hash.is_some());
    assert_eq!(hot.snapshot().get("main.ron").unwrap().thrust, 12.25);
    // A consumer that kept the old snapshot still sees the old, whole value.
    assert_eq!(before.get("main.ron").unwrap().thrust, 1.5);

    // 2. An invalid edit (syntax) keeps the value and reports the file.
    write(&dir.0, "12.25.7.9, oops");
    let error = wait_for_report(&mut hot, &clock).expect_err("syntax error is rejected");
    assert_eq!(error.diagnostics()[0].source, "main.ron");
    assert!(error.to_string().contains("main.ron"), "{error}");
    assert_eq!(hot.generation(), 1);
    assert_eq!(hot.snapshot().get("main.ron").unwrap().thrust, 12.25);

    // 3. A validation failure is rejected the same way.
    write(&dir.0, "-3.0");
    let error = wait_for_report(&mut hot, &clock).expect_err("validation failure is rejected");
    assert_eq!(error.diagnostics()[0].code.as_str(), "validation");
    assert_eq!(hot.snapshot().get("main.ron").unwrap().thrust, 12.25);

    // 4. A later valid edit recovers.
    write(&dir.0, "100.0");
    let reloaded = wait_for_report(&mut hot, &clock).expect("recovers");
    assert_eq!(reloaded.generation, 2);
    assert_eq!(hot.snapshot().get("main.ron").unwrap().thrust, 100.0);
}

#[test]
fn a_bad_file_at_startup_is_an_error_with_the_file_name() {
    let dir = TempDir::new("startup");
    std::fs::write(dir.0.join("bad.ron"), "not ron at all").unwrap();
    let error = HotData::<Engine>::open(&dir.0, Loader::new()).unwrap_err();
    assert_eq!(error.diagnostics()[0].source, "bad.ron");
}
