//! Content hashes and the data-set hash.

use core::fmt;
use core::str::FromStr;

use serde::{Serialize, Serializer};

use crate::canon::{EncodeError, encode_into, put_len, put_str};
use crate::diagnostic::Code;
use crate::path::DataPath;
use crate::schema::Schema;

/// Context string of the content hash (BLAKE3 key-derivation mode). It names
/// the algorithm, the date it was fixed and the encoding version; if the
/// canonical encoding ever changes it becomes v2 with a new string.
const CONTENT_CONTEXT: &str = "st-data 2026-10-11 canonical content hash v1";

/// Context string of the data-set hash.
const DATA_SET_CONTEXT: &str = "st-data 2026-10-11 data set hash v1";

/// Why a hash string could not be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseHashError;

impl fmt::Display for ParseHashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected `b3:` followed by 64 lowercase hex digits")
    }
}

impl std::error::Error for ParseHashError {}

macro_rules! hash_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Wraps raw hash bytes.
            #[must_use]
            pub fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            /// The 32 raw bytes.
            #[must_use]
            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("b3:")?;
                for byte in self.0 {
                    write!(f, "{byte:02x}")?;
                }
                Ok(())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self)
            }
        }

        impl FromStr for $name {
            type Err = ParseHashError;

            fn from_str(s: &str) -> Result<Self, ParseHashError> {
                parse_hex(s).map(Self).ok_or(ParseHashError)
            }
        }
    };
}

hash_type! {
    /// The content hash of one loaded file: BLAKE3 over the canonical
    /// encoding of its kind, version and value.
    ///
    /// The text form is `b3:` followed by 64 lowercase hex digits.
    ///
    /// # Examples
    ///
    /// ```
    /// use st_data::ContentHash;
    ///
    /// let text = format!("b3:{}", "00".repeat(32));
    /// let hash: ContentHash = text.parse().unwrap();
    /// assert_eq!(hash.to_string(), text);
    /// assert!("b3:xyz".parse::<ContentHash>().is_err());
    /// ```
    ContentHash
}

hash_type! {
    /// The hash of a set of loaded files, see [`data_set_hash`]. This is the
    /// value that replays and determinism checks refer to.
    ///
    /// The text form is `b3:` followed by 64 lowercase hex digits.
    DataSetHash
}

fn parse_hex(s: &str) -> Option<[u8; 32]> {
    let digits = s.strip_prefix("b3:")?.as_bytes();
    if digits.len() != 64 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let hi = nibble(*digits.get(2 * i)?)?;
        let lo = nibble(*digits.get(2 * i + 1)?)?;
        *byte = hi << 4 | lo;
    }
    Some(out)
}

/// The content hash of `value` as the current version of its schema.
///
/// The hash input is the canonical encoding of `T::KIND` (as a string),
/// `T::VERSION` (as a `u32`) and the value. Whitespace, comments, field
/// order, `Some(x)` versus `x` and an omitted field versus its default
/// written out never reach it: the hash is over the value, not the file.
///
/// # Errors
///
/// Fails with an [`EncodeError`] when the value contains a NaN or infinite
/// float, a 128-bit integer, or a duplicate map key.
///
/// # Examples
///
/// ```
/// use serde::{Deserialize, Serialize};
/// use st_data::{content_hash, Genesis, MigrationError, Schema};
///
/// #[derive(Serialize, Deserialize)]
/// struct Crate { weight: u32 }
/// impl Schema for Crate {
///     const KIND: &'static str = "cargo_crate";
///     const VERSION: u32 = 1;
///     type Previous = Genesis;
///     fn migrate(p: Genesis) -> Result<Self, MigrationError> { match p {} }
/// }
///
/// let a = content_hash(&Crate { weight: 3 }).unwrap();
/// let b = content_hash(&Crate { weight: 4 }).unwrap();
/// assert_ne!(a, b);
/// assert!(a.to_string().starts_with("b3:"));
/// ```
pub fn content_hash<T: Schema>(value: &T) -> Result<ContentHash, EncodeError> {
    let mut input = Vec::new();
    put_str(&mut input, T::KIND);
    input.extend_from_slice(&T::VERSION.to_le_bytes());
    encode_into(&mut input, value)?;
    let mut hasher = blake3::Hasher::new_derive_key(CONTENT_CONTEXT);
    hasher.update(&input);
    Ok(ContentHash(*hasher.finalize().as_bytes()))
}

/// One loaded file, as the data-set hash sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataSetEntry {
    /// The file's kind.
    pub kind: String,
    /// The source name the file was loaded under, with `/` separators.
    pub source: String,
    /// The file's content hash.
    pub hash: ContentHash,
}

/// The content hash as the canonical encoding sees it: raw bytes, with a
/// length like any other byte string.
struct HashBytes<'a>(&'a ContentHash);

impl Serialize for HashBytes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0.as_bytes())
    }
}

/// The hash of a set of loaded files.
///
/// BLAKE3 in key-derivation mode with the context string
/// `"st-data 2026-10-11 data set hash v1"` over the canonical encoding of the
/// list of (`kind`, source name, content hash) triples, sorted by source
/// name. The content hash is encoded as a byte string (its length, then the
/// 32 raw bytes).
///
/// **Sort order.** The entries are sorted by plain byte-wise lexicographic
/// order of the UTF-8 source names, which is Rust's `Ord` for `str`: no
/// locale, no case folding, no Unicode normalisation, and `/` (0x2F) sorts
/// as an ordinary byte. The order of `entries` does not matter.
///
/// # Errors
///
/// Fails with [`Code::DuplicateKey`] when two entries have the same source
/// name: a source name identifies one file.
///
/// # Examples
///
/// ```
/// use st_data::{data_set_hash, ContentHash, DataSetEntry};
///
/// let entry = |source: &str, byte: u8| DataSetEntry {
///     kind: "ship_design".into(),
///     source: source.into(),
///     hash: ContentHash::from_bytes([byte; 32]),
/// };
/// let ab = data_set_hash(&[entry("a.ron", 1), entry("b.ron", 2)]).unwrap();
/// let ba = data_set_hash(&[entry("b.ron", 2), entry("a.ron", 1)]).unwrap();
/// assert_eq!(ab, ba); // the order of the input does not matter
/// assert!(data_set_hash(&[entry("a.ron", 1), entry("a.ron", 2)]).is_err());
/// ```
pub fn data_set_hash(entries: &[DataSetEntry]) -> Result<DataSetHash, EncodeError> {
    let mut sorted: Vec<&DataSetEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.source.cmp(&b.source));
    for pair in sorted.windows(2) {
        if let [a, b] = pair
            && a.source == b.source
        {
            return Err(EncodeError {
                code: Code::DuplicateKey,
                message: format!("two data-set entries have the source name `{}`", a.source),
                path: DataPath::new(),
            });
        }
    }
    let list: Vec<(&str, &str, HashBytes<'_>)> = sorted
        .iter()
        .map(|e| (e.kind.as_str(), e.source.as_str(), HashBytes(&e.hash)))
        .collect();
    let mut input = Vec::new();
    // Encoded as a sequence: the count, then each triple in order.
    put_len(&mut input, list.len());
    for item in &list {
        encode_into(&mut input, item)?;
    }
    let mut hasher = blake3::Hasher::new_derive_key(DATA_SET_CONTEXT);
    hasher.update(&input);
    Ok(DataSetHash(*hasher.finalize().as_bytes()))
}
