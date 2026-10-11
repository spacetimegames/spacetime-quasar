//! The `Schema` trait and the typed migration chain.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::path::DataPath;

/// A payload type of one kind at one version, with its way up from the
/// version before.
///
/// Each supported version of a kind is its own Rust type. The current
/// version is the live type; every earlier supported version is a frozen
/// copy that is never edited again. `Previous` links a type to the version
/// before it, so the chain from the oldest supported version to the current
/// one is checked by the compiler, and the loader walks it by recursion.
///
/// Rules for schema types (see the crate documentation): derive
/// `Serialize` and `Deserialize`, put `#[serde(deny_unknown_fields)]` on
/// every struct and enum, use [`UniqueMap`](crate::UniqueMap) instead of
/// hash maps, and avoid the forbidden serde attributes.
///
/// # Examples
///
/// ```
/// use serde::{Deserialize, Serialize};
/// use st_data::{Genesis, MigrationError, Schema};
///
/// #[derive(Serialize, Deserialize)]
/// #[serde(deny_unknown_fields)]
/// struct Crate { weight: u32 }
///
/// impl Schema for Crate {
///     const KIND: &'static str = "cargo_crate";
///     const VERSION: u32 = 1;
///     type Previous = Genesis; // the oldest supported version
///     fn migrate(previous: Genesis) -> Result<Self, MigrationError> {
///         match previous {} // `Genesis` has no values
///     }
/// }
/// ```
pub trait Schema: Serialize + serde::de::DeserializeOwned + 'static {
    /// The `kind` string of the envelope. It matches
    /// `[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*`, is chosen by the owning crate
    /// and never changes meaning.
    const KIND: &'static str;
    /// The schema version this type represents, at least 1.
    const VERSION: u32;
    /// The type of version `VERSION - 1`, or [`Genesis`] for the oldest
    /// supported version.
    type Previous: Schema;
    /// Converts a value of the previous version into this version.
    ///
    /// A migration is a pure function of the previous value: no I/O, clock,
    /// RNG, other files, global state or hash-map iteration, and no float
    /// arithmetic where it can be avoided. It never "fixes" balance or
    /// validity; the migrated value is validated like a freshly authored one.
    ///
    /// # Errors
    ///
    /// Returns a [`MigrationError`] with the path of the offending value when
    /// the previous value cannot be converted.
    fn migrate(previous: Self::Previous) -> Result<Self, MigrationError>;
}

/// The uninhabited type before the oldest supported version of a kind.
///
/// `Genesis` has [`VERSION`](Schema::VERSION) 0. The loader stops with
/// `version-too-old` when a chain reaches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Genesis {}

impl Schema for Genesis {
    const KIND: &'static str = "";
    const VERSION: u32 = 0;
    type Previous = Genesis;

    fn migrate(previous: Genesis) -> Result<Self, MigrationError> {
        // `Genesis` has no values, so there is nothing to match.
        match previous {}
    }
}

/// Why a [`Schema::migrate`] step failed.
///
/// # Examples
///
/// ```
/// use st_data::{DataPath, MigrationError};
///
/// let e = MigrationError::new("mass does not fit in 16 bits")
///     .at(DataPath::new().key("sections").index(1).key("mass"));
/// assert_eq!(e.to_string(), "sections[1].mass: mass does not fit in 16 bits");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationError {
    /// What went wrong.
    pub message: String,
    /// The location of the offending value in the previous version's value.
    pub path: DataPath,
}

impl MigrationError {
    /// An error with a message and no path.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            path: DataPath::new(),
        }
    }

    /// Sets the path of the offending value.
    #[must_use]
    pub fn at(mut self, path: DataPath) -> Self {
        self.path = path;
        self
    }
}

impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

impl std::error::Error for MigrationError {}

/// Whether `kind` matches `[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*`.
///
/// # Examples
///
/// ```
/// assert!(st_data::is_valid_kind("ship_design"));
/// assert!(st_data::is_valid_kind("save.universe"));
/// assert!(!st_data::is_valid_kind("Ship"));
/// assert!(!st_data::is_valid_kind("a..b"));
/// assert!(!st_data::is_valid_kind(""));
/// ```
#[must_use]
pub fn is_valid_kind(kind: &str) -> bool {
    kind.split('.').all(|part| {
        let mut chars = part.chars();
        matches!(chars.next(), Some('a'..='z'))
            && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
    })
}

/// A flaw in a [`Schema`] chain, found by [`check_chain`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainError {
    /// The version of the type where the flaw was found.
    pub version: u32,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "version {}: {}", self.version, self.message)
    }
}

impl std::error::Error for ChainError {}

/// Checks the shape of the chain that ends in `T`, for use in the owning
/// crate's unit tests.
///
/// Every type in the chain must have the same [`KIND`](Schema::KIND), a
/// valid kind string, and a version exactly one above its predecessor's, and
/// the chain must end in [`Genesis`] at a version of at least 1. (The
/// version arithmetic is also asserted at compile time when the loader is
/// instantiated for `T`.)
///
/// # Errors
///
/// Returns the first flaw found, oldest version last.
///
/// # Examples
///
/// ```
/// use serde::{Deserialize, Serialize};
/// use st_data::{check_chain, Genesis, MigrationError, Schema};
///
/// #[derive(Serialize, Deserialize)]
/// struct V1 { a: u8 }
/// impl Schema for V1 {
///     const KIND: &'static str = "demo";
///     const VERSION: u32 = 1;
///     type Previous = Genesis;
///     fn migrate(p: Genesis) -> Result<Self, MigrationError> { match p {} }
/// }
///
/// assert!(check_chain::<V1>().is_ok());
/// ```
pub fn check_chain<T: Schema>() -> Result<(), ChainError> {
    check_link::<T>(T::KIND, None)
}

fn check_link<S: Schema>(kind: &str, above: Option<u32>) -> Result<(), ChainError> {
    let fail = |message: String| {
        Err(ChainError {
            version: S::VERSION,
            message,
        })
    };
    if S::VERSION == 0 {
        // Reached the end of the chain; an empty chain is not a chain.
        return match above {
            Some(_) => Ok(()),
            None => fail("a schema must have a version of at least 1".into()),
        };
    }
    if S::KIND != kind {
        return fail(format!("kind `{}` differs from `{kind}`", S::KIND));
    }
    if !is_valid_kind(S::KIND) {
        return fail(format!("`{}` is not a valid kind string", S::KIND));
    }
    if let Some(above) = above
        && S::VERSION.checked_add(1) != Some(above)
    {
        return fail(format!(
            "version {} is followed by version {above}, expected {}",
            S::VERSION,
            S::VERSION.saturating_add(1)
        ));
    }
    let previous = <S::Previous as Schema>::VERSION;
    if previous != 0 && previous.checked_add(1) != Some(S::VERSION) {
        return fail(format!(
            "`Previous` has version {previous}, expected {}",
            S::VERSION.saturating_sub(1)
        ));
    }
    // Strictly decreasing versions guarantee that this recursion ends.
    check_link::<S::Previous>(kind, Some(S::VERSION))
}
