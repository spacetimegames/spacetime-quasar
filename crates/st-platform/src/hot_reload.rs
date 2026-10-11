//! Polling file watcher for development hot reload (ADR 0010, "Hot reload").
//!
//! Compiled only with the `hot-reload` cargo feature on non-wasm targets.
//!
//! # How it works
//!
//! * *Polling, no thread, no dependency.* The main loop calls
//!   [`Watcher::poll`] once per frame. Nothing runs in between.
//! * *Sliced work.* One call visits at most
//!   [`WatcherConfig::entries_per_poll`] directory entries (256 by default).
//!   Opening a directory and reading each entry, including its metadata, each
//!   count as one entry, so a big tree is swept over many frames and no call
//!   walks a whole tree. The sweep resumes where the previous call stopped.
//! * *What counts as a change.* A different modification time or a different
//!   length. Files that appear or disappear are noticed by the listing itself.
//! * *Batches.* Changes are held back until none has been seen for
//!   [`WatcherConfig::quiet_period`] (200 ms by default). An editor that writes
//!   a temporary file and renames it, or a "save all", therefore produces one
//!   batch, and a file that is added and removed again within the window
//!   produces nothing.
//! * *All or nothing is the consumer's job.* The watcher only reports which
//!   files changed. The application reads them all ([`Change::read`]), hands
//!   every (name, bytes) pair to the data set as one batch and applies the
//!   batch only if every file and cross-file check passes.
//!
//! The watcher never reads a clock. The caller passes the current time as
//! [`Nanos`] (from [`Clock::now`](crate::Clock::now)), which makes it testable
//! without sleeping and keeps all wall-clock use on the caller's side.

use crate::fs::{FileSource, FsError, matches_extension, read_file_as, source_name_of};
use st_time::Nanos;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, ReadDir};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Directory entries visited per [`Watcher::poll`] unless configured
/// otherwise (ADR 0010).
pub const DEFAULT_ENTRIES_PER_POLL: usize = 256;

/// How long the watched files must stay unchanged before a batch is released,
/// unless configured otherwise (ADR 0010).
pub const DEFAULT_QUIET_PERIOD: Nanos = Nanos::from_millis(200);

/// Settings for a [`Watcher`].
///
/// ```
/// use st_platform::WatcherConfig;
/// use st_time::Nanos;
///
/// let config = WatcherConfig::new()
///     .entries_per_poll(64)
///     .quiet_period(Nanos::from_millis(100))
///     .extension("ron");
/// # let _ = config;
/// ```
#[derive(Clone, Debug)]
pub struct WatcherConfig {
    entries_per_poll: usize,
    quiet_period: Nanos,
    extension: Option<String>,
}

impl WatcherConfig {
    /// The ADR 0010 defaults: 256 entries per poll, 200 ms quiet period, all
    /// file extensions.
    pub fn new() -> WatcherConfig {
        WatcherConfig {
            entries_per_poll: DEFAULT_ENTRIES_PER_POLL,
            quiet_period: DEFAULT_QUIET_PERIOD,
            extension: None,
        }
    }

    /// Sets how many directory entries one poll may visit. Zero is treated as
    /// one, so that a sweep always makes progress.
    pub fn entries_per_poll(mut self, entries: usize) -> WatcherConfig {
        self.entries_per_poll = entries.max(1);
        self
    }

    /// Sets how long the files must stay unchanged before a batch is released.
    pub fn quiet_period(mut self, quiet: Nanos) -> WatcherConfig {
        self.quiet_period = quiet;
        self
    }

    /// Watches only files with this extension (without the dot, for example
    /// `"ron"`). Directories are always searched.
    ///
    /// Not part of ADR 0010's text: it keeps editor swap and backup files out
    /// of the batches, where the data set would report them as unknown
    /// sources. Without it every file is watched, as the ADR describes.
    pub fn extension(mut self, extension: impl Into<String>) -> WatcherConfig {
        self.extension = Some(extension.into());
        self
    }
}

impl Default for WatcherConfig {
    fn default() -> WatcherConfig {
        WatcherConfig::new()
    }
}

/// What happened to a file between two batches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// The file did not exist at the previous batch.
    Added,
    /// The file's modification time or length differs from the previous batch.
    Modified,
    /// The file no longer exists.
    Removed,
}

/// One changed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    path: PathBuf,
    name: String,
    kind: ChangeKind,
}

impl Change {
    /// Returns the file's path, under the watched root it was found in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the source name: the path relative to its watched root, with
    /// `/` separators on every platform (for example `ships/frigate.ron`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns what happened to the file.
    pub fn kind(&self) -> ChangeKind {
        self.kind
    }

    /// Reads the file's current contents under [`Change::name`].
    ///
    /// Reading is separate from noticing so that the caller decides when to
    /// pay for it. For a [`ChangeKind::Removed`] file this returns a
    /// not-found error; a file may also vanish or change again after the
    /// batch was released, and the next batch then reports that.
    pub fn read(&self) -> Result<FileSource, FsError> {
        read_file_as(&self.path, self.name.clone())
    }
}

/// The files that changed together, sorted by path.
///
/// A batch is never empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeBatch {
    changes: Vec<Change>,
}

impl ChangeBatch {
    /// Returns the changes in path order, so a batch is processed in the same
    /// order on every machine.
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    /// Returns how many files changed.
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Returns whether the batch has no changes (it never does when it comes
    /// from [`Watcher::poll`]).
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Reads every file that still exists, in path order, for passing to a
    /// loader as (name, bytes) pairs. Removed files are skipped: they have no
    /// bytes, and the caller learns of them from [`ChangeBatch::changes`].
    ///
    /// Fails with the first read error, so the caller can keep the previous
    /// values (the batch is all-or-nothing).
    pub fn read_present(&self) -> Result<Vec<FileSource>, FsError> {
        self.changes
            .iter()
            .filter(|c| c.kind != ChangeKind::Removed)
            .map(Change::read)
            .collect()
    }
}

impl<'a> IntoIterator for &'a ChangeBatch {
    type Item = &'a Change;
    type IntoIter = std::slice::Iter<'a, Change>;

    fn into_iter(self) -> Self::IntoIter {
        self.changes.iter()
    }
}

/// What the watcher knows about a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    // `None` where the file system keeps no modification time; the length
    // alone is then compared.
    modified: Option<SystemTime>,
    len: u64,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    stamp: Stamp,
    /// Index into `Watcher::roots`.
    root: usize,
}

/// A directory being listed over several polls.
#[derive(Debug)]
struct Listing {
    dir: PathBuf,
    root: usize,
    entries: ReadDir,
}

/// One full pass over the roots, suspended and resumed across polls.
#[derive(Debug)]
struct Sweep {
    /// Roots `0..roots` take part. A root added during the sweep waits for
    /// the next one.
    roots: usize,
    queue: VecDeque<(PathBuf, usize)>,
    listing: Option<Listing>,
    /// Files met so far; known files missing from it at the end are removed.
    seen: BTreeSet<PathBuf>,
    /// Directories that could not be listed for a reason other than being
    /// gone. Files below them are kept rather than reported removed.
    unreadable: Vec<PathBuf>,
}

/// Notices changed files under a set of directories by polling.
///
/// ```
/// use st_platform::{Clock, Watcher, WatcherConfig};
///
/// # let dir = std::env::temp_dir().join(format!("st-platform-doc-watch-{}", std::process::id()));
/// # std::fs::create_dir_all(&dir).unwrap();
/// let clock = Clock::new();
/// let mut watcher = Watcher::new(WatcherConfig::new().extension("ron"));
/// watcher.watch_root(&dir);
///
/// // Once per frame:
/// if let Some(batch) = watcher.poll(clock.now()) {
///     for source in batch.read_present()? {
///         // hand (source.name(), source.as_str()?) to the data set
///         # let _ = source;
///     }
/// }
/// # std::fs::remove_dir_all(&dir).unwrap();
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// The first complete sweep of a root only records what is there: files that
/// exist when watching starts are not reported as added.
#[derive(Debug)]
pub struct Watcher {
    config: WatcherConfig,
    roots: Vec<PathBuf>,
    /// Roots `0..primed` have had a complete first sweep.
    primed: usize,
    /// What the last sweep found.
    current: BTreeMap<PathBuf, Entry>,
    /// What the last released batch (or the first sweep) told the consumer.
    reported: BTreeMap<PathBuf, Entry>,
    /// Paths whose `current` may differ from `reported`.
    dirty: BTreeSet<PathBuf>,
    last_change: Nanos,
    sweep: Option<Sweep>,
    sweeps_completed: u64,
}

impl Watcher {
    /// Creates a watcher with no roots; add some with [`Watcher::watch_root`].
    pub fn new(config: WatcherConfig) -> Watcher {
        Watcher {
            config,
            roots: Vec::new(),
            primed: 0,
            current: BTreeMap::new(),
            reported: BTreeMap::new(),
            dirty: BTreeSet::new(),
            last_change: Nanos::ZERO,
            sweep: None,
            sweeps_completed: 0,
        }
    }

    /// Adds a directory to watch, searched recursively. Adding a root that is
    /// already watched does nothing. Roots should not be nested in each other.
    ///
    /// A root that does not exist (yet) is not an error; the watcher reports
    /// its files once it appears. Symbolic links to directories are not
    /// followed.
    pub fn watch_root(&mut self, root: impl Into<PathBuf>) {
        let root = root.into();
        if !self.roots.contains(&root) {
            self.roots.push(root);
        }
    }

    /// Returns the watched roots in the order they were added.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Returns how many files the watcher currently knows of.
    pub fn tracked_files(&self) -> usize {
        self.current.len()
    }

    /// Returns how many complete sweeps have finished. After the first, every
    /// root added before it has its baseline.
    pub fn sweeps_completed(&self) -> u64 {
        self.sweeps_completed
    }

    /// Does one slice of work and returns a batch if one is due.
    ///
    /// Call once per frame with the current time (any monotonic time base
    /// works, for example [`Clock::now`](crate::Clock::now)). Never blocks
    /// beyond the file system calls for at most
    /// [`WatcherConfig::entries_per_poll`] entries, and never panics on
    /// unreadable or vanishing files: those are skipped and tried again on a
    /// later sweep.
    pub fn poll(&mut self, now: Nanos) -> Option<ChangeBatch> {
        self.advance_sweep(now);
        self.release(now)
    }

    fn advance_sweep(&mut self, now: Nanos) {
        if self.roots.is_empty() {
            return;
        }
        let mut sweep = self.sweep.take().unwrap_or_else(|| Sweep {
            roots: self.roots.len(),
            queue: self
                .roots
                .iter()
                .cloned()
                .enumerate()
                .map(|(i, p)| (p, i))
                .collect(),
            listing: None,
            seen: BTreeSet::new(),
            unreadable: Vec::new(),
        });
        let mut budget = self.config.entries_per_poll;

        while budget > 0 {
            let Some(listing) = sweep.listing.as_mut() else {
                let Some((dir, root)) = sweep.queue.pop_front() else {
                    // Queue and listing are both empty: the sweep is done.
                    self.finish_sweep(sweep, now);
                    return;
                };
                budget -= 1;
                match fs::read_dir(&dir) {
                    Ok(entries) => sweep.listing = Some(Listing { dir, root, entries }),
                    Err(e) => note_unlistable(&mut sweep, dir, &e),
                }
                continue;
            };
            match listing.entries.next() {
                None => sweep.listing = None,
                Some(Err(_)) => {
                    budget -= 1;
                    let dir = listing.dir.clone();
                    sweep.listing = None;
                    // Stop listing this directory; what it holds stays as it was.
                    sweep.unreadable.push(dir);
                }
                Some(Ok(entry)) => {
                    budget -= 1;
                    let root = listing.root;
                    self.visit(&mut sweep, entry, root, now);
                }
            }
        }
        self.sweep = Some(sweep);
    }

    fn visit(&mut self, sweep: &mut Sweep, entry: fs::DirEntry, root: usize, now: Nanos) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            // Cannot tell what it is: keep what we know about it.
            sweep.seen.insert(path);
            return;
        };
        if file_type.is_dir() {
            sweep.queue.push_back((path, root));
            return;
        }
        if !matches_extension(&path, self.config.extension.as_deref()) {
            return;
        }
        let metadata = if file_type.is_symlink() {
            // Follow links to files (a linked asset); never to directories.
            fs::metadata(&path)
        } else if file_type.is_file() {
            entry.metadata()
        } else {
            return;
        };
        let metadata = match metadata {
            Ok(m) if m.is_file() => m,
            Ok(_) => return,
            // Gone between listing and stat: leave it unseen, so a known file
            // is reported removed at the end of the sweep.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(_) => {
                sweep.seen.insert(path);
                return;
            }
        };
        sweep.seen.insert(path.clone());
        let new = Entry {
            stamp: Stamp {
                modified: metadata.modified().ok(),
                len: metadata.len(),
            },
            root,
        };
        // The first sweep of a root only records a baseline.
        let baseline = root >= self.primed;
        match self.current.get(&path) {
            Some(known) if known.stamp == new.stamp => {}
            Some(_) => {
                self.current.insert(path.clone(), new);
                self.mark_dirty(path, now);
            }
            None => {
                self.current.insert(path.clone(), new);
                if baseline {
                    self.reported.insert(path, new);
                } else {
                    self.mark_dirty(path, now);
                }
            }
        }
    }

    fn mark_dirty(&mut self, path: PathBuf, now: Nanos) {
        self.dirty.insert(path);
        self.last_change = now;
    }

    fn finish_sweep(&mut self, sweep: Sweep, now: Nanos) {
        let gone: Vec<PathBuf> = self
            .current
            .keys()
            .filter(|p| !sweep.seen.contains(*p))
            .filter(|p| !sweep.unreadable.iter().any(|d| p.starts_with(d)))
            .cloned()
            .collect();
        for path in gone {
            self.current.remove(&path);
            self.mark_dirty(path, now);
        }
        self.primed = self.primed.max(sweep.roots);
        self.sweeps_completed += 1;
    }

    /// Releases the dirty set once it has been quiet for the quiet period.
    ///
    /// The batch is computed by comparing the current state with what the
    /// consumer was last told, not by replaying events. That way
    /// add-then-remove cancels out, remove-then-add (a rename over the file)
    /// becomes a modification, and a change that is undone disappears.
    fn release(&mut self, now: Nanos) -> Option<ChangeBatch> {
        if self.dirty.is_empty() || now.saturating_sub(self.last_change) < self.config.quiet_period
        {
            return None;
        }
        let mut changes = Vec::new();
        // `BTreeSet` iterates in path order, which fixes the batch order.
        for path in std::mem::take(&mut self.dirty) {
            let current = self.current.get(&path).copied();
            let reported = self.reported.get(&path).copied();
            let (kind, root) = match (reported, current) {
                (None, Some(c)) => (ChangeKind::Added, c.root),
                (Some(r), Some(c)) if r.stamp != c.stamp => (ChangeKind::Modified, c.root),
                (Some(r), None) => (ChangeKind::Removed, r.root),
                _ => continue,
            };
            match current {
                Some(c) => self.reported.insert(path.clone(), c),
                None => self.reported.remove(&path),
            };
            let name = self
                .roots
                .get(root)
                .and_then(|r| path.strip_prefix(r).ok())
                .map_or_else(|| source_name_of(&path), source_name_of);
            changes.push(Change { path, name, kind });
        }
        (!changes.is_empty()).then_some(ChangeBatch { changes })
    }
}

fn note_unlistable(sweep: &mut Sweep, dir: PathBuf, error: &io::Error) {
    // A directory that is gone has no files any more; any other failure may be
    // temporary (permissions, a busy file system), so keep what we know.
    if error.kind() != io::ErrorKind::NotFound {
        sweep.unreadable.push(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;
    use std::time::Duration;

    const QUIET: Nanos = Nanos::from_millis(200);

    fn config() -> WatcherConfig {
        WatcherConfig::new().extension("ron")
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Polls until one more sweep has finished, at time `now`.
    fn finish_sweep(w: &mut Watcher, now: Nanos) -> Vec<ChangeBatch> {
        let target = w.sweeps_completed() + 1;
        let mut batches = Vec::new();
        for _ in 0..10_000 {
            batches.extend(w.poll(now));
            if w.sweeps_completed() >= target {
                return batches;
            }
        }
        panic!("sweep did not finish");
    }

    fn names(batch: &ChangeBatch) -> Vec<(String, ChangeKind)> {
        batch
            .changes()
            .iter()
            .map(|c| (c.name().to_owned(), c.kind()))
            .collect()
    }

    /// A watcher whose baseline sweep has completed.
    fn primed(dir: &TempDir) -> Watcher {
        let mut w = Watcher::new(config());
        w.watch_root(dir.path());
        assert!(finish_sweep(&mut w, Nanos::ZERO).is_empty());
        w
    }

    #[test]
    fn existing_files_are_a_baseline_not_changes() {
        let dir = TempDir::new();
        write(&dir.path().join("a.ron"), "1");
        write(&dir.path().join("sub/b.ron"), "2");
        let mut w = Watcher::new(config());
        w.watch_root(dir.path());
        assert!(finish_sweep(&mut w, Nanos::ZERO).is_empty());
        assert_eq!(w.tracked_files(), 2);
        // Nothing is released later either.
        assert!(finish_sweep(&mut w, Nanos::from_secs(10)).is_empty());
    }

    #[test]
    fn modification_is_held_for_the_quiet_period_then_released_once() {
        let dir = TempDir::new();
        let file = dir.path().join("ships/a.ron");
        write(&file, "1");
        let mut w = primed(&dir);

        write(&file, "22"); // different length
        let t0 = Nanos::from_secs(1);
        assert!(finish_sweep(&mut w, t0).is_empty()); // seen, not yet released
        let almost = t0.saturating_add(QUIET).saturating_sub(Nanos::new(1));
        assert!(w.poll(almost).is_none());

        let batch = w.poll(t0.saturating_add(QUIET)).expect("batch due");
        assert_eq!(
            names(&batch),
            [("ships/a.ron".to_owned(), ChangeKind::Modified)]
        );
        assert_eq!(batch.changes()[0].path(), file);

        // Reported once only.
        assert!(finish_sweep(&mut w, Nanos::from_secs(5)).is_empty());
        assert!(w.poll(Nanos::from_secs(6)).is_none());
    }

    #[test]
    fn modification_time_alone_counts() {
        let dir = TempDir::new();
        let file = dir.path().join("a.ron");
        write(&file, "same");
        let mut w = primed(&dir);

        let handle = fs::File::options().write(true).open(&file).unwrap();
        handle
            .set_modified(SystemTime::now() + Duration::from_secs(60))
            .unwrap();
        drop(handle);
        finish_sweep(&mut w, Nanos::ZERO);
        let batch = w.poll(QUIET).expect("batch");
        assert_eq!(names(&batch), [("a.ron".to_owned(), ChangeKind::Modified)]);
    }

    #[test]
    fn further_changes_extend_the_wait_and_join_one_batch() {
        let dir = TempDir::new();
        write(&dir.path().join("a.ron"), "1");
        let mut w = primed(&dir);

        write(&dir.path().join("a.ron"), "22");
        finish_sweep(&mut w, Nanos::from_millis(1000));
        // "Save all": another file arrives 150 ms later.
        write(&dir.path().join("b.ron"), "3");
        finish_sweep(&mut w, Nanos::from_millis(1150));
        // 200 ms after the first change is not enough: the clock restarted.
        assert!(w.poll(Nanos::from_millis(1200)).is_none());
        let batch = w.poll(Nanos::from_millis(1350)).expect("batch");
        assert_eq!(
            names(&batch),
            [
                ("a.ron".to_owned(), ChangeKind::Modified),
                ("b.ron".to_owned(), ChangeKind::Added),
            ]
        );
    }

    #[test]
    fn added_and_removed_files_are_noticed() {
        let dir = TempDir::new();
        write(&dir.path().join("gone.ron"), "x");
        let mut w = primed(&dir);

        write(&dir.path().join("deep/er/new.ron"), "y");
        fs::remove_file(dir.path().join("gone.ron")).unwrap();
        finish_sweep(&mut w, Nanos::ZERO);
        let batch = w.poll(QUIET).expect("batch");
        assert_eq!(
            names(&batch),
            [
                ("deep/er/new.ron".to_owned(), ChangeKind::Added),
                ("gone.ron".to_owned(), ChangeKind::Removed),
            ]
        );
        assert_eq!(w.tracked_files(), 1);
        // A removed file cannot be read, and read_present skips it.
        assert_eq!(
            batch.changes()[1].read().unwrap_err().kind(),
            crate::FsErrorKind::NotFound
        );
        let present = batch.read_present().unwrap();
        assert_eq!(present.len(), 1);
        assert_eq!(present[0].name(), "deep/er/new.ron");
        assert_eq!(present[0].as_str(), Ok("y"));
    }

    #[test]
    fn temporary_file_added_and_removed_inside_the_window_cancels_out() {
        let dir = TempDir::new();
        write(&dir.path().join("a.ron"), "1");
        let mut w = primed(&dir);

        // An editor writes a temp file, then renames it over the original.
        write(&dir.path().join("a.ron.tmp"), "new!");
        let ron_tmp = dir.path().join("a.ron.tmp");
        finish_sweep(&mut w, Nanos::ZERO); // the filter ignores ".tmp"
        fs::rename(&ron_tmp, dir.path().join("a.ron")).unwrap();
        finish_sweep(&mut w, Nanos::from_millis(10));
        let batch = w.poll(Nanos::from_millis(300)).expect("batch");
        assert_eq!(names(&batch), [("a.ron".to_owned(), ChangeKind::Modified)]);
    }

    #[test]
    fn add_then_remove_before_release_reports_nothing() {
        let dir = TempDir::new();
        let mut w = primed(&dir);
        let file = dir.path().join("t.ron");
        write(&file, "1");
        finish_sweep(&mut w, Nanos::ZERO);
        fs::remove_file(&file).unwrap();
        finish_sweep(&mut w, Nanos::from_millis(50));
        assert!(w.poll(Nanos::from_secs(5)).is_none());
        assert!(finish_sweep(&mut w, Nanos::from_secs(6)).is_empty());
    }

    #[test]
    fn a_poll_visits_at_most_the_budget_and_a_sweep_spans_frames() {
        let dir = TempDir::new();
        for i in 0..20 {
            write(&dir.path().join(format!("f{i:02}.ron")), "1");
        }
        let mut w = Watcher::new(config().entries_per_poll(5));
        w.watch_root(dir.path());

        let mut polls = 0;
        while w.sweeps_completed() == 0 {
            w.poll(Nanos::ZERO);
            polls += 1;
            assert!(polls < 100);
        }
        // 1 directory open + 20 entries = 21 units at 5 per poll, plus the
        // poll that notices the end: never one call for the whole tree.
        assert!(polls >= 5, "swept in {polls} polls");
        assert_eq!(w.tracked_files(), 20);
    }

    #[test]
    fn budget_of_zero_still_progresses() {
        let dir = TempDir::new();
        write(&dir.path().join("a.ron"), "1");
        let mut w = Watcher::new(config().entries_per_poll(0));
        w.watch_root(dir.path());
        finish_sweep(&mut w, Nanos::ZERO);
        assert_eq!(w.tracked_files(), 1);
    }

    #[test]
    fn change_during_a_sliced_sweep_is_found_by_a_later_sweep() {
        let dir = TempDir::new();
        for i in 0..10 {
            write(&dir.path().join(format!("f{i}.ron")), "1");
        }
        let mut w = Watcher::new(config().entries_per_poll(3));
        w.watch_root(dir.path());
        finish_sweep(&mut w, Nanos::ZERO);

        write(&dir.path().join("f9.ron"), "changed");
        finish_sweep(&mut w, Nanos::ZERO);
        finish_sweep(&mut w, Nanos::ZERO);
        let batch = w.poll(QUIET).expect("batch");
        assert_eq!(names(&batch), [("f9.ron".to_owned(), ChangeKind::Modified)]);
    }

    #[test]
    fn missing_root_is_not_an_error_and_appears_later() {
        let dir = TempDir::new();
        let root = dir.path().join("assets");
        let mut w = Watcher::new(config());
        w.watch_root(&root);
        assert!(finish_sweep(&mut w, Nanos::ZERO).is_empty());

        write(&root.join("a.ron"), "1");
        finish_sweep(&mut w, Nanos::ZERO);
        let batch = w.poll(QUIET).expect("batch");
        assert_eq!(names(&batch), [("a.ron".to_owned(), ChangeKind::Added)]);
    }

    #[test]
    fn a_root_added_later_gets_its_own_baseline() {
        let dir = TempDir::new();
        let other = TempDir::new();
        write(&dir.path().join("a.ron"), "1");
        let mut w = primed(&dir);

        write(&other.path().join("b.ron"), "2");
        w.watch_root(other.path());
        finish_sweep(&mut w, Nanos::ZERO);
        finish_sweep(&mut w, Nanos::ZERO);
        // b.ron existed when its root was added: no change.
        assert!(w.poll(Nanos::from_secs(5)).is_none());
        assert_eq!(w.tracked_files(), 2);
        assert_eq!(w.roots().len(), 2);
    }

    #[test]
    fn duplicate_roots_are_ignored() {
        let dir = TempDir::new();
        let mut w = Watcher::new(config());
        w.watch_root(dir.path());
        w.watch_root(dir.path());
        assert_eq!(w.roots().len(), 1);
    }

    #[test]
    fn polling_with_no_roots_does_nothing() {
        let mut w = Watcher::new(config());
        assert!(w.poll(Nanos::from_secs(1)).is_none());
        assert_eq!(w.sweeps_completed(), 0);
    }

    #[test]
    fn other_extensions_are_ignored() {
        let dir = TempDir::new();
        let mut w = primed(&dir);
        write(&dir.path().join("notes.txt"), "x");
        write(&dir.path().join("a.ron~"), "x");
        finish_sweep(&mut w, Nanos::ZERO);
        assert!(w.poll(Nanos::from_secs(1)).is_none());
        assert_eq!(w.tracked_files(), 0);
    }

    #[test]
    fn without_a_filter_every_file_counts() {
        let dir = TempDir::new();
        let mut w = Watcher::new(WatcherConfig::default());
        w.watch_root(dir.path());
        finish_sweep(&mut w, Nanos::ZERO);
        write(&dir.path().join("notes.txt"), "x");
        finish_sweep(&mut w, Nanos::ZERO);
        let batch = w.poll(QUIET).expect("batch");
        assert_eq!(names(&batch), [("notes.txt".to_owned(), ChangeKind::Added)]);
    }

    #[test]
    fn time_going_backwards_does_not_panic_or_release_early() {
        let dir = TempDir::new();
        let mut w = primed(&dir);
        write(&dir.path().join("a.ron"), "1");
        finish_sweep(&mut w, Nanos::from_secs(10));
        assert!(w.poll(Nanos::from_secs(1)).is_none());
        assert!(w.poll(Nanos::from_secs(11)).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_not_followed() {
        let dir = TempDir::new();
        write(&dir.path().join("real/a.ron"), "1");
        std::os::unix::fs::symlink(dir.path(), dir.path().join("real/loop")).unwrap();
        let mut w = Watcher::new(config());
        w.watch_root(dir.path());
        finish_sweep(&mut w, Nanos::ZERO);
        assert_eq!(w.tracked_files(), 1);
    }
}
