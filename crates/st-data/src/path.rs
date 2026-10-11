//! Locations inside a payload, in the notation used by diagnostics.

use core::fmt;

/// One step of a [`DataPath`].
///
/// A struct field and a map key are both a [`Segment::Key`]: the payload
/// text does not distinguish them either, and the span locator treats them
/// alike.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Segment {
    /// A struct field name or a map key, rendered as `.name`.
    Key(String),
    /// A position in a sequence or tuple, rendered as `[index]`.
    Index(usize),
    /// The name of an enum variant, rendered like a key (`.Cargo`).
    Variant(String),
}

/// The location of a value inside a payload.
///
/// Rendered like `sections[2].mass`. The envelope's `data.` prefix is not
/// part of a payload path; the envelope fields themselves are the paths
/// `kind` and `version`.
///
/// # Examples
///
/// ```
/// use st_data::DataPath;
///
/// let path = DataPath::new().key("sections").index(2).key("mass");
/// assert_eq!(path.to_string(), "sections[2].mass");
/// assert!(DataPath::new().is_empty());
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataPath {
    segments: Vec<Segment>,
}

impl DataPath {
    /// The empty path: the payload itself.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a path from segments.
    #[must_use]
    pub fn from_segments(segments: Vec<Segment>) -> Self {
        Self { segments }
    }

    /// Returns this path extended with a field name or map key.
    #[must_use]
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.segments.push(Segment::Key(key.into()));
        self
    }

    /// Returns this path extended with a sequence index.
    #[must_use]
    pub fn index(mut self, index: usize) -> Self {
        self.segments.push(Segment::Index(index));
        self
    }

    /// Returns this path extended with an enum variant name.
    #[must_use]
    pub fn variant(mut self, name: impl Into<String>) -> Self {
        self.segments.push(Segment::Variant(name.into()));
        self
    }

    /// The segments, outermost first.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Whether the path is empty (it names the payload itself).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

impl fmt::Display for DataPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, segment) in self.segments.iter().enumerate() {
            match segment {
                Segment::Key(name) | Segment::Variant(name) => {
                    if i > 0 {
                        f.write_str(".")?;
                    }
                    f.write_str(name)?;
                }
                Segment::Index(index) => write!(f, "[{index}]")?,
            }
        }
        Ok(())
    }
}
