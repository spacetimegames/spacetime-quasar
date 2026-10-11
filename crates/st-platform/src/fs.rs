//! File reads that return bytes together with a source name.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::Utf8Error;

/// The size limit that [`read_file`] and [`read_file_as`] apply: 64 MiB.
///
/// Player-shared files are untrusted input (ADR 0010), and reading an
/// unbounded file into memory would let one of them exhaust it. Use
/// [`read_file_limited`] for a different bound.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// The bytes of a file and the name to show when talking about it.
///
/// The name is whatever the caller chose with [`read_file_as`], or the path
/// as given for [`read_file`]. Loaders put it in their diagnostics, so it
/// uses `/` as the separator on every platform; a diagnostic or a hash that
/// includes it then does not depend on the host operating system.
///
/// ```
/// use st_platform::FileSource;
///
/// let source = FileSource::new("ships/frigate.ron", b"(mass: 1.0)".to_vec());
/// assert_eq!(source.name(), "ships/frigate.ron");
/// assert_eq!(source.as_str(), Ok("(mass: 1.0)"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSource {
    name: String,
    bytes: Vec<u8>,
}

impl FileSource {
    /// Pairs a name with bytes that the caller already has.
    pub fn new(name: impl Into<String>, bytes: Vec<u8>) -> FileSource {
        FileSource {
            name: name.into(),
            bytes,
        }
    }

    /// Returns the source name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the file contents.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the contents as text, or the UTF-8 error if they are not text.
    pub fn as_str(&self) -> Result<&str, Utf8Error> {
        std::str::from_utf8(&self.bytes)
    }

    /// Splits the source into its name and bytes.
    pub fn into_parts(self) -> (String, Vec<u8>) {
        (self.name, self.bytes)
    }
}

/// What went wrong in a file operation.
///
/// Matching on this is stable across platforms, unlike [`io::ErrorKind`]
/// strings. New kinds may be added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FsErrorKind {
    /// The file or directory does not exist.
    NotFound,
    /// The operating system denied access.
    PermissionDenied,
    /// The file is larger than the limit that the caller allowed.
    TooLarge {
        /// The limit in bytes.
        limit: u64,
    },
    /// This target has no file system access (`wasm32`).
    Unsupported,
    /// Any other input/output failure; the underlying error is the `source` of the [`FsError`].
    Other,
}

/// A failed file operation, naming the source it concerned.
///
/// ```
/// use st_platform::{FsErrorKind, read_file_as};
///
/// let err = read_file_as("no/such/dir/ship.ron", "ship.ron").unwrap_err();
/// assert_eq!(err.source_name(), "ship.ron");
/// assert!(matches!(err.kind(), FsErrorKind::NotFound | FsErrorKind::Unsupported));
/// ```
#[derive(Debug)]
pub struct FsError {
    kind: FsErrorKind,
    source_name: String,
    io: Option<io::Error>,
}

impl FsError {
    pub(crate) fn from_io(source_name: String, io: io::Error) -> FsError {
        let kind = match io.kind() {
            io::ErrorKind::NotFound => FsErrorKind::NotFound,
            io::ErrorKind::PermissionDenied => FsErrorKind::PermissionDenied,
            io::ErrorKind::Unsupported => FsErrorKind::Unsupported,
            _ => FsErrorKind::Other,
        };
        FsError {
            kind,
            source_name,
            io: Some(io),
        }
    }

    fn without_io(source_name: String, kind: FsErrorKind) -> FsError {
        FsError {
            kind,
            source_name,
            io: None,
        }
    }

    /// Returns what went wrong.
    pub fn kind(&self) -> FsErrorKind {
        self.kind
    }

    /// Returns the name of the source that the operation concerned.
    pub fn source_name(&self) -> &str {
        &self.source_name
    }
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot read `{}`: ", self.source_name)?;
        match self.kind {
            FsErrorKind::NotFound => f.write_str("not found"),
            FsErrorKind::PermissionDenied => f.write_str("permission denied"),
            FsErrorKind::TooLarge { limit } => write!(f, "larger than {limit} bytes"),
            FsErrorKind::Unsupported => f.write_str("no file system on this target"),
            FsErrorKind::Other => match &self.io {
                Some(io) => write!(f, "{io}"),
                None => f.write_str("input/output error"),
            },
        }
    }
}

impl Error for FsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.io.as_ref().map(|e| e as &(dyn Error + 'static))
    }
}

/// Turns a path into a source name: lossy UTF-8 with `/` separators.
pub(crate) fn source_name_of(path: &Path) -> String {
    let text = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        text.into_owned()
    } else {
        text.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

/// Reads a whole file, naming the source after the path as given.
///
/// Fails with [`FsErrorKind::TooLarge`] above [`DEFAULT_MAX_FILE_BYTES`].
/// Blocking; do not call it from a thread that must not stall.
///
/// ```no_run
/// let source = st_platform::read_file("assets/ships/frigate.ron")?;
/// println!("{}: {} bytes", source.name(), source.bytes().len());
/// # Ok::<(), st_platform::FsError>(())
/// ```
pub fn read_file(path: impl AsRef<Path>) -> Result<FileSource, FsError> {
    let path = path.as_ref();
    read_file_limited(path, source_name_of(path), DEFAULT_MAX_FILE_BYTES)
}

/// Reads a whole file under a name of the caller's choosing, typically the
/// path relative to an asset root.
///
/// Same limit as [`read_file`].
///
/// ```
/// # let dir = std::env::temp_dir().join(format!("st-platform-doc-{}", std::process::id()));
/// # std::fs::create_dir_all(&dir).unwrap();
/// # let path = dir.join("ship.ron");
/// # std::fs::write(&path, "(mass: 2.0)").unwrap();
/// let source = st_platform::read_file_as(&path, "ships/ship.ron")?;
/// assert_eq!(source.name(), "ships/ship.ron");
/// assert_eq!(source.as_str(), Ok("(mass: 2.0)"));
/// # std::fs::remove_dir_all(&dir).unwrap();
/// # Ok::<(), st_platform::FsError>(())
/// ```
pub fn read_file_as(
    path: impl AsRef<Path>,
    name: impl Into<String>,
) -> Result<FileSource, FsError> {
    read_file_limited(path.as_ref(), name.into(), DEFAULT_MAX_FILE_BYTES)
}

/// Reads a whole file, failing with [`FsErrorKind::TooLarge`] if it holds
/// more than `max_bytes`.
///
/// At most `max_bytes + 1` bytes are ever read, so a huge or endless file
/// cannot use up memory.
///
/// ```
/// # let dir = std::env::temp_dir().join(format!("st-platform-doc-lim-{}", std::process::id()));
/// # std::fs::create_dir_all(&dir).unwrap();
/// # let path = dir.join("big.ron");
/// # std::fs::write(&path, "0123456789").unwrap();
/// use st_platform::{FsErrorKind, read_file_limited};
///
/// let err = read_file_limited(&path, "big.ron", 4).unwrap_err();
/// assert_eq!(err.kind(), FsErrorKind::TooLarge { limit: 4 });
/// assert!(read_file_limited(&path, "big.ron", 10).is_ok());
/// # std::fs::remove_dir_all(&dir).unwrap();
/// ```
pub fn read_file_limited(
    path: impl AsRef<Path>,
    name: impl Into<String>,
    max_bytes: u64,
) -> Result<FileSource, FsError> {
    let name = name.into();
    match imp::read(path.as_ref(), max_bytes) {
        Ok(Some(bytes)) => Ok(FileSource { name, bytes }),
        Ok(None) => Err(FsError::without_io(
            name,
            FsErrorKind::TooLarge { limit: max_bytes },
        )),
        Err(e) => Err(FsError::from_io(name, e)),
    }
}

/// Lists the files under `root`, in a fixed order.
///
/// Searches subdirectories. Returns paths (each starting with `root`) in
/// byte-wise sorted order, so that loading them gives the same sequence on
/// every machine; directory order from the operating system is unspecified
/// and ADR 0008 forbids letting that leak into the simulation. With
/// `extension` set (without the dot, for example `"ron"`), only files with
/// that extension are returned.
///
/// Symbolic links to directories are not followed, which rules out loops;
/// symbolic links to files are listed. Blocking, and walks the whole tree:
/// meant for start-up and tests. Per-frame code uses the `Watcher` instead.
///
/// ```
/// # let dir = std::env::temp_dir().join(format!("st-platform-doc-ls-{}", std::process::id()));
/// # std::fs::create_dir_all(dir.join("ships")).unwrap();
/// # std::fs::write(dir.join("ships/b.ron"), "").unwrap();
/// # std::fs::write(dir.join("ships/a.ron"), "").unwrap();
/// # std::fs::write(dir.join("notes.txt"), "").unwrap();
/// let files = st_platform::list_files(&dir, Some("ron"))?;
/// assert_eq!(files.len(), 2);
/// assert!(files[0].ends_with("a.ron") && files[1].ends_with("b.ron"));
/// # std::fs::remove_dir_all(&dir).unwrap();
/// # Ok::<(), st_platform::FsError>(())
/// ```
pub fn list_files(
    root: impl AsRef<Path>,
    extension: Option<&str>,
) -> Result<Vec<PathBuf>, FsError> {
    let root = root.as_ref();
    imp::list(root, extension).map_err(|e| FsError::from_io(source_name_of(root), e))
}

/// Whether `path` has the wanted extension (`None` accepts everything).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn matches_extension(path: &Path, extension: Option<&str>) -> bool {
    match extension {
        None => true,
        Some(wanted) => path.extension().is_some_and(|e| e == wanted),
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::matches_extension;
    use std::fs::{self, File};
    use std::io::{self, Read};
    use std::path::{Path, PathBuf};

    pub(super) fn read(path: &Path, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
        let file = File::open(path)?;
        let mut bytes = Vec::new();
        // One byte past the limit is enough to tell "exactly at the limit"
        // from "over it" without reading the rest.
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max_bytes {
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    pub(super) fn list(root: &Path, extension: Option<&str>) -> io::Result<Vec<PathBuf>> {
        let mut found = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                let file_type = entry.file_type()?;
                if file_type.is_dir() {
                    pending.push(path);
                    continue;
                }
                // A symbolic link counts as a file only if its target is one.
                let is_file = if file_type.is_symlink() {
                    fs::metadata(&path).is_ok_and(|m| m.is_file())
                } else {
                    file_type.is_file()
                };
                if is_file && matches_extension(&path, extension) {
                    found.push(path);
                }
            }
        }
        found.sort();
        Ok(found)
    }
}

// `wasm32` has no file system we can rely on: `wasm32-unknown-unknown` has
// none, and the browser fallback comes with the browser target. Returning an
// explicit error keeps callers' code identical on every target.
#[cfg(target_arch = "wasm32")]
mod imp {
    use std::io;
    use std::path::{Path, PathBuf};

    fn unsupported() -> io::Error {
        io::Error::from(io::ErrorKind::Unsupported)
    }

    pub(super) fn read(_path: &Path, _max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
        Err(unsupported())
    }

    pub(super) fn list(_root: &Path, _extension: Option<&str>) -> io::Result<Vec<PathBuf>> {
        Err(unsupported())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::test_util::TempDir;
    use std::fs;

    #[test]
    fn reads_bytes_and_names_the_source() {
        let dir = TempDir::new();
        let path = dir.path().join("a.ron");
        fs::write(&path, b"(x: 1)").unwrap();

        let by_path = read_file(&path).unwrap();
        assert_eq!(by_path.bytes(), b"(x: 1)");
        assert!(by_path.name().ends_with("a.ron"));
        assert!(!by_path.name().contains('\\'));

        let named = read_file_as(&path, "data/a.ron").unwrap();
        assert_eq!(named.name(), "data/a.ron");
        assert_eq!(named.as_str(), Ok("(x: 1)"));
    }

    #[test]
    fn non_utf8_bytes_are_readable_but_not_text() {
        let dir = TempDir::new();
        let path = dir.path().join("bin");
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        let source = read_file(&path).unwrap();
        assert_eq!(source.bytes(), [0xff, 0xfe, 0x00]);
        assert!(source.as_str().is_err());
    }

    #[test]
    fn missing_file_is_not_found() {
        let dir = TempDir::new();
        let err = read_file_as(dir.path().join("nope.ron"), "nope.ron").unwrap_err();
        assert_eq!(err.kind(), FsErrorKind::NotFound);
        assert_eq!(err.source_name(), "nope.ron");
        assert_eq!(err.to_string(), "cannot read `nope.ron`: not found");
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn directory_is_an_error_not_a_panic() {
        let dir = TempDir::new();
        assert!(read_file(dir.path()).is_err());
    }

    #[test]
    fn limit_is_inclusive() {
        let dir = TempDir::new();
        let path = dir.path().join("ten");
        fs::write(&path, [7u8; 10]).unwrap();
        assert!(read_file_limited(&path, "ten", 10).is_ok());
        let err = read_file_limited(&path, "ten", 9).unwrap_err();
        assert_eq!(err.kind(), FsErrorKind::TooLarge { limit: 9 });
        assert_eq!(err.to_string(), "cannot read `ten`: larger than 9 bytes");
        assert_eq!(
            read_file_limited(&path, "ten", 0).unwrap_err().kind(),
            FsErrorKind::TooLarge { limit: 0 }
        );
        assert!(read_file_limited(&path, "ten", u64::MAX).is_ok());
    }

    #[test]
    fn lists_sorted_and_filtered() {
        let dir = TempDir::new();
        fs::create_dir_all(dir.path().join("b/deep")).unwrap();
        fs::create_dir_all(dir.path().join("a")).unwrap();
        for f in ["z.ron", "b/y.ron", "b/deep/x.ron", "a/w.ron", "a/skip.txt"] {
            fs::write(dir.path().join(f), "").unwrap();
        }
        let ron = list_files(dir.path(), Some("ron")).unwrap();
        let mut sorted = ron.clone();
        sorted.sort();
        assert_eq!(ron, sorted);
        assert_eq!(ron.len(), 4);
        assert_eq!(list_files(dir.path(), None).unwrap().len(), 5);
        assert_eq!(list_files(dir.path(), Some("none")).unwrap().len(), 0);
    }

    #[test]
    fn listing_a_missing_root_is_an_error() {
        let dir = TempDir::new();
        let err = list_files(dir.path().join("gone"), None).unwrap_err();
        assert_eq!(err.kind(), FsErrorKind::NotFound);
    }
}
