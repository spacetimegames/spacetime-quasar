//! The envelope writer: the inverse of the loader for one value.

use core::fmt;

use serde::Serialize;

use crate::canon::{EncodeError, canonical_bytes};
use crate::schema::Schema;

/// The envelope could not be written.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// The value has no canonical encoding (a NaN, an infinity, a 128-bit
    /// integer, a skipped field, ...), so the loader would refuse it.
    Encode(EncodeError),
    /// `ron` could not serialise the value.
    Ron(String),
    /// The schema's version is 0, or its kind is empty
    /// ([`Genesis`](crate::Genesis) is such a schema).
    NotASchema,
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Encode(e) => write!(f, "the value cannot be written: {e}"),
            WriteError::Ron(e) => write!(f, "ron could not serialise the value: {e}"),
            WriteError::NotASchema => f.write_str("the schema has no kind or version"),
        }
    }
}

impl core::error::Error for WriteError {}

/// The envelope as it is serialised: three fields, in this order.
#[derive(Serialize)]
struct Envelope<'a, T> {
    kind: &'a str,
    version: u32,
    data: &'a T,
}

/// Writes `value` as an envelope of its schema's kind and current version,
/// as RON text with LF line ends and a final LF.
///
/// The output is what a person would write by hand: a struct without its
/// name, fields in declaration order, indented by four spaces. It loads back
/// through [`Loader`](crate::Loader) to the same value with the same
/// [`content_hash`](crate::content_hash). The text is a pure function of the
/// value, on every platform.
///
/// # Errors
///
/// [`WriteError::Encode`] for a value the loader would refuse (the check is
/// the canonical encoding of the content hash), [`WriteError::Ron`] if `ron`
/// fails, and [`WriteError::NotASchema`] for `Genesis`.
///
/// # Examples
///
/// ```
/// use serde::{Deserialize, Serialize};
/// use st_data::{Genesis, Loader, MigrationError, Schema, content_hash, write_envelope};
///
/// #[derive(Debug, PartialEq, Serialize, Deserialize)]
/// #[serde(deny_unknown_fields)]
/// struct Crate { weight: u32, label: String }
///
/// impl Schema for Crate {
///     const KIND: &'static str = "cargo_crate";
///     const VERSION: u32 = 1;
///     type Previous = Genesis;
///     fn migrate(p: Genesis) -> Result<Self, MigrationError> { match p {} }
/// }
///
/// let value = Crate { weight: 3, label: "spare parts".into() };
/// let text = write_envelope(&value)?;
/// assert!(text.starts_with("(\n    kind: \"cargo_crate\",\n    version: 1,\n"));
///
/// let loaded = Loader::new().load_unvalidated::<Crate>("crate.ron", text.as_bytes()).unwrap();
/// assert_eq!(loaded.value, value);
/// assert_eq!(loaded.hash, content_hash(&value)?);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn write_envelope<T: Schema>(value: &T) -> Result<String, WriteError> {
    if T::VERSION == 0 || T::KIND.is_empty() {
        return Err(WriteError::NotASchema);
    }
    // The canonical encoding rejects exactly what the loader would reject, so
    // an envelope that was written always loads.
    canonical_bytes(value).map_err(WriteError::Encode)?;
    let envelope = Envelope {
        kind: T::KIND,
        version: T::VERSION,
        data: value,
    };
    // `new_line` is set because the default is CR LF on Windows, and the text
    // must be the same on every platform.
    let config = ron::ser::PrettyConfig::new()
        .new_line("\n")
        .indentor("    ")
        .struct_names(false);
    let mut text = ron::Options::default()
        .to_string_pretty(&envelope, config)
        .map_err(|e| WriteError::Ron(e.to_string()))?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::{Genesis, Loader, MigrationError, content_hash};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Probe {
        n: u64,
        x: f64,
        name: String,
        list: Vec<u64>,
        maybe: Option<u32>,
    }

    impl Schema for Probe {
        const KIND: &'static str = "probe";
        const VERSION: u32 = 1;
        type Previous = Genesis;
        fn migrate(p: Genesis) -> Result<Self, MigrationError> {
            match p {}
        }
    }

    #[derive(Serialize, Deserialize)]
    struct Unversioned {}

    impl Schema for Unversioned {
        const KIND: &'static str = "unversioned";
        const VERSION: u32 = 0;
        type Previous = Genesis;
        fn migrate(p: Genesis) -> Result<Self, MigrationError> {
            match p {}
        }
    }

    fn probe() -> Probe {
        Probe {
            n: u64::MAX,
            x: -0.0,
            name: "a \"quoted\"\nname".into(),
            list: vec![1, 2, 3],
            maybe: Some(7),
        }
    }

    #[test]
    fn round_trips_with_the_same_hash() {
        let value = probe();
        let text = write_envelope(&value).unwrap();
        let loaded = Loader::new()
            .load_unvalidated::<Probe>("probe.ron", text.as_bytes())
            .unwrap();
        assert_eq!(loaded.value, value);
        assert_eq!(loaded.hash, content_hash(&value).unwrap());
        assert!(loaded.value.x.is_sign_negative());
    }

    #[test]
    fn has_lf_line_ends_only_and_is_deterministic() {
        let text = write_envelope(&probe()).unwrap();
        assert!(!text.contains('\r'));
        assert!(text.ends_with(")\n"));
        assert_eq!(text, write_envelope(&probe()).unwrap());
    }

    #[test]
    fn refuses_a_value_the_loader_would_refuse() {
        let mut value = probe();
        value.x = f64::NAN;
        assert!(matches!(write_envelope(&value), Err(WriteError::Encode(_))));
        value.x = f64::INFINITY;
        assert!(matches!(write_envelope(&value), Err(WriteError::Encode(_))));
        assert_eq!(write_envelope(&Unversioned {}), Err(WriteError::NotASchema));
    }
}
