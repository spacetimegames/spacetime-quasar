//! Test helpers: temporary directories created at runtime.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A fresh directory under the system temp dir, removed on drop.
///
/// Tests never use fixed paths: parallel tests and repeated runs must not
/// collide, and the repository must not contain machine-specific paths.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new() -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..10_000 {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("st-platform-test-{}-{n}", std::process::id()));
            // `create_dir` fails if the name exists, so a stale directory from
            // a crashed run is skipped rather than reused.
            if std::fs::create_dir(&path).is_ok() {
                return TempDir(path);
            }
        }
        panic!("cannot create a temporary directory under the system temp dir");
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
