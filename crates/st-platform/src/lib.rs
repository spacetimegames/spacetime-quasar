//! Operating-system abstractions: file reads, a monotonic clock and (behind a
//! feature) a polling file watcher.
//!
//! This is the one crate allowed to touch the operating system. Simulation
//! crates may not read a wall clock, spawn threads or open files (ADR 0008);
//! they receive bytes and elapsed time as plain values instead, and this crate
//! is where those values come from. Everything here is exempt from the
//! simulation's determinism rules, and nothing here knows about game types.
//!
//! # Pieces
//!
//! * [`read_file`], [`read_file_as`], [`read_file_limited`] and [`list_files`]
//!   return a [`FileSource`], the bytes of a file together with a *source name*
//!   that a loader can put in its diagnostics. A data loader that accepts
//!   `&str` or `&[u8]` plus a name is fed with `source.name()` and
//!   `source.bytes()`; this crate does not depend on any loader.
//! * [`Clock`] and [`FrameTimer`] measure real elapsed time and return
//!   [`st_time::Nanos`], ready to pass to a fixed-step accumulator.
//! * `Watcher` (cargo feature `hot-reload`, native targets only) notices
//!   changed files by polling, without threads, and releases the changes as
//!   one batch.
//!
//! # Targets
//!
//! The crate compiles for Linux, Windows, macOS and `wasm32`.
//!
//! | Facility | Native | `wasm32` |
//! | --- | --- | --- |
//! | File reads and listing | `std::fs` | every call returns [`FsErrorKind::Unsupported`] |
//! | [`Clock`] | `std::time::Instant` | works on `wasm32-wasip1`; on `wasm32-unknown-unknown` [`Clock::is_supported`] is `false` and the clock stays at zero |
//! | `Watcher` | feature `hot-reload` | not compiled (ADR 0010: hot reload is dev-only and native-only) |
//!
//! Nothing panics on a target that lacks a facility: the caller gets an
//! explicit error or a documented constant.
//!
//! # Deferred
//!
//! Phase 1 needs only the above, so these are left out on purpose:
//!
//! * *Threads.* ADR 0008 has the game run simulation steps on a thread that
//!   this crate provides. The session loop that needs it does not exist yet, so
//!   the seam is added with it. Until then there is no spawning API here.
//! * *Async and browser loading.* Reads are synchronous and blocking, and
//!   return owned bytes, so a caller can already run them on a worker thread
//!   once one exists. A web `fetch` or `performance.now` back-end for
//!   `wasm32-unknown-unknown` needs a new dependency and the browser target;
//!   it arrives with that target.
//! * *Window, input and per-OS integration* (decorations, VSync, HiDPI,
//!   desktop entry) are not part of this change.
//!
//! # Example
//!
//! ```
//! use st_platform::{FrameTimer, read_file_as};
//! use st_time::FixedStep;
//!
//! let mut timer = FrameTimer::new();
//! let mut steps = FixedStep::default();
//! let ticks_due = steps.advance(timer.lap()).ticks();
//! assert!(ticks_due <= 8);
//!
//! // A missing file is an error value, not a panic.
//! assert!(read_file_as("no/such/dir/ship.ron", "ship.ron").is_err());
//! ```

mod clock;
mod fs;
#[cfg(all(feature = "hot-reload", not(target_arch = "wasm32")))]
mod hot_reload;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod test_util;

pub use clock::{Clock, FrameTimer};
pub use fs::{
    DEFAULT_MAX_FILE_BYTES, FileSource, FsError, FsErrorKind, list_files, read_file, read_file_as,
    read_file_limited,
};
#[cfg(all(feature = "hot-reload", not(target_arch = "wasm32")))]
pub use hot_reload::{
    Change, ChangeBatch, ChangeKind, DEFAULT_ENTRIES_PER_POLL, DEFAULT_QUIET_PERIOD, Watcher,
    WatcherConfig,
};
