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
//! # How a reload goes
//!
//! The main loop calls [`HotData::poll`] once per frame with the current time.
//! A poll does a bounded slice of watcher work and never sleeps. When the
//! watcher releases a batch of changed files, every file is read through
//! `st-platform`, then parsed, migrated and validated through `st-data`. The
//! batch is all or nothing:
//!
//! * If every file passes, a new [`DataSnapshot`] replaces the old one in one
//!   step and the generation increases. Consumers hold `Arc<DataSnapshot>`
//!   values, so one that is still holding the old snapshot keeps seeing the
//!   old, complete data; it never sees a mix.
//! * If any file fails (it does not read, does not parse, fails validation,
//!   or was removed), the previous snapshot stays in use, [`HotData::poll`]
//!   returns a [`ReloadError`] with every diagnostic, and the batch is handed
//!   back to the watcher with `Watcher::requeue`. A half-written file is the
//!   typical cause: it fails once, and the editor's next write releases a new
//!   batch that carries the rest of the rejected one with it, so a later valid
//!   edit recovers.
//! * A file whose content hash did not change produces no event, and a batch
//!   with no hash change at all produces nothing.

use st_data::{Code, ContentHash, Diagnostic, Loaded, Loader, Schema, Validate};
use st_platform::{
    ChangeBatch, ChangeKind, FsError, FsErrorKind, Watcher, WatcherConfig, list_files, read_file_as,
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
    /// Returns `None` when no batch is due or the batch changed no value,
    /// `Some(Ok(_))` when a new snapshot was swapped in, and `Some(Err(_))`
    /// when the batch was rejected and the previous snapshot stays.
    pub fn poll(&mut self, now: Nanos) -> Option<Result<Reloaded, ReloadError>> {
        let batch = self.watcher.poll(now)?;
        match self.apply(&batch) {
            Ok(None) => None,
            Ok(Some(reloaded)) => Some(Ok(reloaded)),
            Err(error) => {
                // Hand the batch back so a later fix brings its other files
                // along, instead of the rejected files being forgotten.
                self.watcher.requeue(&batch);
                Some(Err(error))
            }
        }
    }

    fn apply(&mut self, batch: &ChangeBatch) -> Result<Option<Reloaded>, ReloadError> {
        let mut diags = Vec::new();
        let mut loaded: BTreeMap<String, Loaded<T>> = BTreeMap::new();
        for change in batch.changes() {
            if change.kind() == ChangeKind::Removed {
                // A removed file keeps its last value until restart (ADR 0010).
                if self.current.files.contains_key(change.name()) {
                    diags.push(Diagnostic::new(
                        change.name(),
                        Code::MissingSource,
                        "the file was removed; its last value stays in use until restart",
                    ));
                }
                continue;
            }
            match change.read() {
                Err(e) => diags.push(read_diagnostic(&e)),
                Ok(source) => match self.loader.load::<T>(source.name(), source.bytes()) {
                    Ok(l) => {
                        loaded.insert(change.name().to_owned(), l);
                    }
                    Err(d) => diags.extend(d),
                },
            }
        }
        if !diags.is_empty() {
            return Err(ReloadError::new(diags));
        }

        let mut files = self.current.files.clone();
        let mut events = Vec::new();
        for (name, l) in loaded {
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
        if events.is_empty() {
            return Ok(None);
        }
        let generation = self.current.generation + 1;
        // One pointer assignment: a consumer has the old snapshot or the new.
        self.current = Arc::new(DataSnapshot { generation, files });
        Ok(Some(Reloaded { generation, events }))
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
    let what = match e.kind() {
        FsErrorKind::NotFound => "the file does not exist",
        FsErrorKind::PermissionDenied => "permission denied",
        FsErrorKind::TooLarge { .. } => "the file is too large",
        FsErrorKind::Unsupported => "this target cannot read files",
        _ => "the file could not be read",
    };
    Diagnostic::new(e.source_name(), Code::MissingSource, what)
        .with_help("the previous value stays in use; save the file again")
}
