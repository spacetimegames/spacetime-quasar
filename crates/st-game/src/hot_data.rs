//! Hot reload of a directory of `.ron` data files (ADR 0010, "Hot reload").
//!
//! # Where this lives, and why
//!
//! `st-data` must stay free of threads and file access (it is a sim crate,
//! compiles for `wasm32-unknown-unknown`, and ADR 0010 keeps "reload
//! bookkeeping" as ordinary code that takes bytes). `st-platform` must not know
//! any loader (its docs say so). Something has to know both, and
//! `st-game` is the crate whose stated job is "content wiring" and which only
//! connects lower crates; it is an exempt crate (ADR 0008), so depending on
//! `st-platform` costs the sim crates nothing. Neither lower crate gains a
//! dependency, and the glue is compiled out on `wasm32` and without the
//! `hot-reload` feature.
//!
//! # The feature is for development builds
//!
//! The cargo feature `hot-reload` is off by default (ADR 0010: release and Nix
//! builds carry no watcher). The application enables it only in dev builds,
//! for example through its own `hot-reload` feature. This crate's tests turn
//! it on through a dev-dependency on itself.
//!
//! # How a reload goes
//!
//! The main loop calls [`HotData::poll`] once per frame with the current time.
//! A poll does a bounded slice of watcher work and never sleeps. When the
//! watcher releases a batch of changed files, the files are read through
//! `st-platform` synchronously, on the calling thread, then parsed, migrated
//! and validated through `st-data`.
//!
//! What is atomic: the files of one batch that are present and valid become
//! visible together, in one new [`DataSnapshot`] and one generation, or not at
//! all. A consumer holds `Arc<DataSnapshot>` values, so one still holding the
//! old snapshot keeps seeing the old, complete data; it never sees a mix.
//!
//! * If every present file of the batch reads, parses and validates, the
//!   snapshot is swapped.
//! * If any present file fails (syntax, validation, read error), no value of
//!   that batch changes and a [`ReloadError`] names every problem. The files
//!   that were fine are handed back to the watcher with
//!   `Watcher::requeue_changes`, so the fix to the bad file brings them along.
//!   The bad file is reported once and not requeued: it comes back when it is
//!   edited again, so it never holds back later edits to other files.
//! * A read error that may be transient (not found after release, permission
//!   denied, other I/O: a file still being written) is requeued and tried
//!   again with the next batch, at most [`MAX_READ_RETRIES`] times; after that
//!   it is reported and dropped. A too-large file is not retried.
//! * A removed or renamed-away file keeps its last value, is reported once as
//!   `missing-source`, and is never requeued. It does not hold back the other
//!   files of its batch, and creating the path again loads it.
//!   Remove-then-create or rename-over (how editors save) ends in a reload.
//! * A file whose content hash did not change produces no event, and a batch
//!   with no hash change at all produces nothing.

use st_data::{Code, ContentHash, Diagnostic, Loaded, Loader, Schema, Validate};
use st_platform::{
    Change, ChangeBatch, ChangeKind, FileSource, FsError, FsErrorKind, Watcher, WatcherConfig,
    list_files, read_file_as,
};
use st_time::Nanos;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

/// An immutable view of every loaded file at one generation.
///
/// Obtain one with [`HotData::snapshot`]; it never changes afterwards.
///
/// ```
/// # let dir = std::env::temp_dir().join(format!("st-game-doc-snap-{}", std::process::id()));
/// # std::fs::create_dir_all(&dir).unwrap();
/// # std::fs::write(dir.join("a.ron"), r#"(kind: "n", version: 1, data: (v: 1))"#).unwrap();
/// # use serde::{Deserialize, Serialize};
/// # #[derive(Serialize, Deserialize)] #[serde(deny_unknown_fields)] struct N { v: u32 }
/// # impl st_data::Schema for N { const KIND: &'static str = "n"; const VERSION: u32 = 1;
/// #   type Previous = st_data::Genesis; fn migrate(p: st_data::Genesis) -> Result<Self, st_data::MigrationError> { match p {} } }
/// # impl st_data::Validate for N { fn validate(&self, _: &mut st_data::Validator) {} }
/// let hot = st_game::HotData::<N>::open(&dir, st_data::Loader::new())?;
/// let snapshot = hot.snapshot();
/// assert_eq!(snapshot.generation(), 0);
/// assert_eq!(snapshot.get("a.ron").unwrap().v, 1);
/// # std::fs::remove_dir_all(&dir).unwrap();
/// # Ok::<(), st_game::ReloadError>(())
/// ```
#[derive(Debug)]
pub struct DataSnapshot<T> {
    generation: u64,
    files: BTreeMap<String, Arc<Loaded<T>>>,
}

impl<T> DataSnapshot<T> {
    /// The generation: 0 for the initial load, plus one per accepted batch.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The value loaded from the file with this source name (its path relative
    /// to the root, with `/` separators), if there is one.
    pub fn get(&self, source: &str) -> Option<&T> {
        self.files.get(source).map(|l| &l.value)
    }

    /// The loaded file (value, hash, file version) for a source name.
    pub fn loaded(&self, source: &str) -> Option<&Loaded<T>> {
        self.files.get(source).map(|l| &**l)
    }

    /// All files in source-name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &T)> {
        self.files.iter().map(|(k, v)| (k.as_str(), &v.value))
    }

    /// How many files are loaded.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether no file is loaded.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// One file whose value changed in an accepted batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadEvent {
    /// The source name of the file.
    pub source: String,
    /// The content hash before, or `None` for a file that is new.
    pub old_hash: Option<ContentHash>,
    /// The content hash now.
    pub new_hash: ContentHash,
}

/// The result of an accepted batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reloaded {
    /// The generation of the new snapshot.
    pub generation: u64,
    /// The files whose content hash changed, in source-name order.
    pub events: Vec<ReloadEvent>,
}

/// A rejected batch or a failed initial load: every diagnostic found.
///
/// Each diagnostic names its file by source name (the path relative to the
/// root), so [`fmt::Display`] gives one readable line per problem.
///
/// ```
/// use st_data::{Code, Diagnostic};
/// use st_game::ReloadError;
///
/// let err = ReloadError::new(vec![Diagnostic::new("ships/a.ron", Code::Syntax, "oops")]);
/// assert_eq!(err.diagnostics().len(), 1);
/// assert!(err.to_string().contains("ships/a.ron"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadError {
    diagnostics: Vec<Diagnostic>,
}

impl ReloadError {
    /// Wraps diagnostics.
    pub fn new(diagnostics: Vec<Diagnostic>) -> ReloadError {
        ReloadError { diagnostics }
    }

    /// The diagnostics, in file order.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

impl fmt::Display for ReloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, d) in self.diagnostics.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{d}")?;
        }
        Ok(())
    }
}

impl Error for ReloadError {}

/// What one released batch did: a swap, problems, or both.
///
/// ```
/// use st_game::ReloadReport;
///
/// fn log(report: &ReloadReport) {
///     if let Some(r) = &report.reloaded {
///         println!("data generation {}: {} file(s) changed", r.generation, r.events.len());
///     }
///     if let Some(e) = &report.error {
///         eprintln!("{e}");
///     }
/// }
/// # let _ = log;
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadReport {
    /// Set when a new snapshot was swapped in.
    pub reloaded: Option<Reloaded>,
    /// Set when something was wrong; the previous value of each file at fault
    /// stays in use.
    pub error: Option<ReloadError>,
}

/// How often a file whose read keeps failing is tried again before it is
/// reported for the last time and dropped.
pub const MAX_READ_RETRIES: u32 = 3;

/// A directory of `.ron` files of one schema, kept current while the game
/// runs. See the module documentation for the rules.
///
/// ```
/// # let dir = std::env::temp_dir().join(format!("st-game-doc-hot-{}", std::process::id()));
/// # std::fs::create_dir_all(&dir).unwrap();
/// # std::fs::write(dir.join("a.ron"), r#"(kind: "n", version: 1, data: (v: 1))"#).unwrap();
/// # use serde::{Deserialize, Serialize};
/// # #[derive(Serialize, Deserialize)] #[serde(deny_unknown_fields)] struct N { v: u32 }
/// # impl st_data::Schema for N { const KIND: &'static str = "n"; const VERSION: u32 = 1;
/// #   type Previous = st_data::Genesis; fn migrate(p: st_data::Genesis) -> Result<Self, st_data::MigrationError> { match p {} } }
/// # impl st_data::Validate for N { fn validate(&self, _: &mut st_data::Validator) {} }
/// use st_game::HotData;
/// use st_time::Nanos;
///
/// let mut hot = HotData::<N>::open(&dir, st_data::Loader::new())?;
/// // Once per frame; nothing changed, so nothing is reported.
/// assert!(hot.poll(Nanos::ZERO).is_none());
/// assert_eq!(hot.snapshot().get("a.ron").unwrap().v, 1);
/// # std::fs::remove_dir_all(&dir).unwrap();
/// # Ok::<(), st_game::ReloadError>(())
/// ```
pub struct HotData<T> {
    watcher: Watcher,
    loader: Loader,
    current: Arc<DataSnapshot<T>>,
    // Failed reads per source name, for the bounded retry.
    attempts: BTreeMap<String, u32>,
}

impl<T: Schema + Validate> HotData<T> {
    /// Loads every `.ron` file under `root` and starts watching it, with the
    /// watcher's default settings.
    ///
    /// # Errors
    ///
    /// Every diagnostic from the initial load; there is no previous value to
    /// keep, so a bad file at startup is an error.
    pub fn open(root: impl AsRef<Path>, loader: Loader) -> Result<HotData<T>, ReloadError> {
        Self::open_with(root, loader, WatcherConfig::new())
    }

    /// Like [`HotData::open`] with watcher settings (the `ron` extension filter
    /// is always applied).
    ///
    /// The watcher's baseline sweep is taken before the files are read, so an
    /// edit that lands during startup is reported by the first batches rather
    /// than lost. This call does that sweep to completion, which is bounded by
    /// the size of the tree; do it at startup, not per frame.
    ///
    /// # Errors
    ///
    /// As [`HotData::open`].
    pub fn open_with(
        root: impl AsRef<Path>,
        loader: Loader,
        config: WatcherConfig,
    ) -> Result<HotData<T>, ReloadError> {
        let root = root.as_ref();
        let mut watcher = Watcher::new(config.extension("ron"));
        watcher.watch_root(root);
        // The first sweep records the baseline; changes are relative to it.
        while watcher.sweeps_completed() == 0 {
            watcher.poll(Nanos::ZERO);
        }

        let mut diags = Vec::new();
        let mut files = BTreeMap::new();
        let listed = list_files(root, Some("ron"))
            .map_err(|e| ReloadError::new(vec![read_diagnostic(&e)]))?;
        for path in listed {
            let name = relative_name(root, &path);
            match read_file_as(&path, name.clone()) {
                Err(e) => diags.push(read_diagnostic(&e)),
                Ok(source) => match loader.load::<T>(source.name(), source.bytes()) {
                    Ok(loaded) => {
                        files.insert(name, Arc::new(loaded));
                    }
                    Err(d) => diags.extend(d),
                },
            }
        }
        if !diags.is_empty() {
            return Err(ReloadError::new(diags));
        }
        Ok(HotData {
            watcher,
            loader,
            attempts: BTreeMap::new(),
            current: Arc::new(DataSnapshot {
                generation: 0,
                files,
            }),
        })
    }

    /// The current snapshot. Keep it for as long as a consistent view is
    /// needed; call again at a point of your choosing to see newer data.
    pub fn snapshot(&self) -> Arc<DataSnapshot<T>> {
        Arc::clone(&self.current)
    }

    /// The generation of the current snapshot.
    pub fn generation(&self) -> u64 {
        self.current.generation
    }

    /// Does one slice of watcher work; call once per frame with a monotonic
    /// time (for example `st_platform::Clock::now`). Never sleeps.
    ///
    /// Returns `None` when no batch is due or the batch changed nothing.
    /// Otherwise the [`ReloadReport`] says whether a new snapshot was swapped
    /// in, whether problems were found, or both (see the module
    /// documentation for exactly what is atomic).
    ///
    /// The files of a released batch are read synchronously, on the calling
    /// thread, inside this call. The cost is bounded by the size of the batch
    /// (the files an editor saved together), not by the size of the tree.
    pub fn poll(&mut self, now: Nanos) -> Option<ReloadReport> {
        self.poll_with(now, &Change::read)
    }

    fn poll_with(
        &mut self,
        now: Nanos,
        read: &dyn Fn(&Change) -> Result<FileSource, FsError>,
    ) -> Option<ReloadReport> {
        let batch = self.watcher.poll(now)?;
        self.apply(&batch, read)
    }

    fn apply(
        &mut self,
        batch: &ChangeBatch,
        read: &dyn Fn(&Change) -> Result<FileSource, FsError>,
    ) -> Option<ReloadReport> {
        let mut diags = Vec::new();
        let mut loaded: Vec<(&Change, Loaded<T>)> = Vec::new();
        // Files that make the batch unusable, and the ones to try again.
        let mut rejected = false;
        let mut retry: Vec<&Change> = Vec::new();
        for change in batch.changes() {
            let name = change.name();
            if change.kind() == ChangeKind::Removed {
                // Reported once, never requeued, and it does not hold back the
                // other files: the last value stays in use (ADR 0010).
                self.attempts.remove(name);
                if self.current.files.contains_key(name) {
                    diags.push(Diagnostic::new(
                        name,
                        Code::MissingSource,
                        "the file was removed; its last value stays in use until it is created again",
                    ));
                }
                continue;
            }
            match read(change) {
                Err(e) => {
                    rejected = true;
                    diags.push(read_diagnostic(&e));
                    if is_transient(&e) {
                        let n = self.attempts.entry(name.to_owned()).or_insert(0);
                        *n += 1;
                        if *n <= MAX_READ_RETRIES {
                            retry.push(change);
                        } else {
                            self.attempts.remove(name);
                        }
                    } else {
                        self.attempts.remove(name);
                    }
                }
                Ok(source) => {
                    self.attempts.remove(name);
                    match self.loader.load::<T>(source.name(), source.bytes()) {
                        Ok(l) => loaded.push((change, l)),
                        Err(d) => {
                            rejected = true;
                            diags.extend(d);
                        }
                    }
                }
            }
        }

        let error = (!diags.is_empty()).then(|| ReloadError::new(diags));
        if rejected {
            // No value changes. The files that were fine are handed back, so
            // fixing the bad file brings them along; the bad file itself is
            // reported once and comes back only when it is edited again
            // (transient read failures are retried a bounded number of times).
            let innocent = loaded.iter().map(|(c, _)| *c);
            self.watcher.requeue_changes(innocent.chain(retry));
            return error.map(|error| ReloadReport {
                reloaded: None,
                error: Some(error),
            });
        }

        let mut files = self.current.files.clone();
        let mut events = Vec::new();
        for (change, l) in loaded {
            let name = change.name().to_owned();
            let old_hash = files.get(&name).map(|old| old.hash);
            if old_hash == Some(l.hash) {
                continue;
            }
            events.push(ReloadEvent {
                source: name.clone(),
                old_hash,
                new_hash: l.hash,
            });
            files.insert(name, Arc::new(l));
        }
        let reloaded = if events.is_empty() {
            None
        } else {
            let generation = self.current.generation + 1;
            // One pointer assignment: a consumer has the old snapshot or the new.
            self.current = Arc::new(DataSnapshot { generation, files });
            Some(Reloaded { generation, events })
        };
        if reloaded.is_none() && error.is_none() {
            return None;
        }
        Some(ReloadReport { reloaded, error })
    }
}

impl<T> fmt::Debug for HotData<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HotData")
            .field("generation", &self.current.generation)
            .field("files", &self.current.files.len())
            .finish_non_exhaustive()
    }
}

/// The source name a watcher would give `path`: relative to `root`, `/`
/// separators.
fn relative_name(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn read_diagnostic(e: &FsError) -> Diagnostic {
    let code = if matches!(e.kind(), FsErrorKind::TooLarge { .. }) {
        Code::TooLarge
    } else {
        Code::MissingSource
    };
    let what = match e.kind() {
        FsErrorKind::NotFound => "the file does not exist",
        FsErrorKind::PermissionDenied => "permission denied",
        FsErrorKind::TooLarge { .. } => "the file is too large",
        FsErrorKind::Unsupported => "this target cannot read files",
        _ => "the file could not be read",
    };
    Diagnostic::new(e.source_name(), code, what)
        .with_help("the previous value stays in use; save the file again")
}

/// Whether a failed read may succeed on a later try: a file that is still
/// being written, locked by an editor, or replaced under us.
fn is_transient(e: &FsError) -> bool {
    matches!(
        e.kind(),
        FsErrorKind::NotFound | FsErrorKind::PermissionDenied | FsErrorKind::Other
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use st_data::{Genesis, MigrationError, Validator};

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct N {
        v: u32,
    }

    impl Schema for N {
        const KIND: &'static str = "n";
        const VERSION: u32 = 1;
        type Previous = Genesis;
        fn migrate(p: Genesis) -> Result<Self, MigrationError> {
            match p {}
        }
    }

    impl Validate for N {
        fn validate(&self, _: &mut Validator) {}
    }

    /// A read that fails the way a half-written, locked file does.
    fn failing_read(_: &Change) -> Result<FileSource, FsError> {
        read_file_as("no/such/dir/x.ron", "x.ron")
    }

    #[test]
    fn a_failing_read_is_retried_a_bounded_number_of_times_then_dropped() {
        let dir = std::env::temp_dir().join(format!("st-game-unit-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = WatcherConfig::new().quiet_period(Nanos::ZERO);
        let mut hot = HotData::<N>::open_with(&dir, Loader::new(), cfg).unwrap();
        let mut t = Nanos::from_secs(1);
        let step = |hot: &mut HotData<N>, text: &str, t: &mut Nanos| {
            std::fs::write(dir.join("x.ron"), text).unwrap();
            for _ in 0..1000 {
                *t = Nanos::from_secs(t.as_nanos() / 1_000_000_000 + 1);
                if let Some(r) = hot.poll_with(*t, &failing_read) {
                    return r;
                }
            }
            panic!("no report");
        };
        // Each edit releases a batch whose read fails; the requeued change
        // rides along with the next one, until the bound is reached.
        let mut text = String::from("(kind: \"n\", version: 1, data: (v: 1))");
        for _ in 0..MAX_READ_RETRIES {
            let r = step(&mut hot, &text, &mut t);
            assert!(r.error.is_some() && r.reloaded.is_none());
            assert!(hot.attempts.get("x.ron").copied().unwrap_or(0) > 0);
            text.push(' ');
        }
        // The attempt after the bound is reported and dropped.
        let r = step(&mut hot, &text, &mut t);
        assert!(r.error.is_some());
        assert!(hot.attempts.is_empty());
        assert_eq!(hot.generation(), 0);

        // Dropped means a real read works again on the next edit.
        text.push(' ');
        std::fs::write(dir.join("x.ron"), &text).unwrap();
        let mut report = None;
        for _ in 0..1000 {
            t = Nanos::from_secs(t.as_nanos() / 1_000_000_000 + 1);
            if let Some(r) = hot.poll(t) {
                report = Some(r);
                break;
            }
        }
        assert_eq!(report.unwrap().reloaded.unwrap().generation, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
