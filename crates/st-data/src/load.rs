//! The loader: the envelope, the two passes and the migration chain.

use std::cell::Cell;

use ron::extensions::Extensions;
use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::canon::EncodeError;
use crate::diagnostic::{Code, Diagnostic, Position, Span};
use crate::hash::{ContentHash, content_hash};
use crate::locator::locate;
use crate::path::{DataPath, Segment};
use crate::schema::{MigrationError, Schema};
use crate::unique_map::DUPLICATE_KEY_TAG;
use crate::validate::{Validate, Validator};

/// The default limit on the size of one input: 16 MiB.
pub const DEFAULT_MAX_BYTES: usize = 16 * 1024 * 1024;

/// The recursion limit that the loader sets on `ron`. It bounds the nesting
/// of values, so no input can overflow the stack.
pub const RECURSION_LIMIT: usize = 128;

/// The most diagnostics per load that get a span from the locator, so that a
/// file with thousands of failures costs a bounded amount of rescanning.
const MAX_LOCATED: usize = 64;

/// The kind and version that open every data file.
///
/// Returned by [`Loader::peek_header`], for callers that dispatch on the
/// kind before they know the payload type (and that report
/// [`Code::UnknownKind`] when no schema is registered for it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// The schema family of the file.
    pub kind: String,
    /// The schema version of the file, at least 1.
    pub version: u32,
}

/// A successfully loaded, migrated and validated file.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded<T> {
    /// The value, at the current version of its schema.
    pub value: T,
    /// The content hash of the value at the current version.
    pub hash: ContentHash,
    /// The version the file was written at. It differs from the schema's
    /// current version when the value was migrated.
    pub file_version: u32,
}

/// Loads data files from bytes plus a source name. It does no I/O.
///
/// # Examples
///
/// ```
/// use st_data::Loader;
///
/// // Inputs above the limit are refused before they are parsed.
/// let loader = Loader::new().with_max_bytes(8);
/// let err = loader.peek_header("a.ron", b"(kind: \"x\", version: 1, data: ())").unwrap_err();
/// assert_eq!(err[0].code.as_str(), "too-large");
/// ```
#[derive(Clone, Debug)]
pub struct Loader {
    max_bytes: usize,
}

impl Default for Loader {
    fn default() -> Self {
        Self::new()
    }
}

impl Loader {
    /// A loader with the default input limit of 16 MiB.
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    /// Sets the largest input, in bytes, that the loader will parse.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Reads only the envelope's `kind` and `version`.
    ///
    /// This is the header pass of [`Loader::load`]: lenient about the
    /// payload and about unknown header fields, strict about syntax.
    ///
    /// # Errors
    ///
    /// Returns diagnostics for oversized or non-UTF-8 input, syntax errors,
    /// and a missing or mistyped `kind` or `version`.
    pub fn peek_header(&self, source: &str, bytes: &[u8]) -> Result<Header, Vec<Diagnostic>> {
        let text = self.prepare(source, bytes)?;
        parse_header(text).map_err(|raw| vec![raw.into_diagnostic(source)])
    }

    /// Loads `bytes` as the schema type `T`: both passes, the migration
    /// chain, the content hash, and `T`'s [`Validate`] rules.
    ///
    /// `source` is the name that diagnostics carry: use `/` as the separator
    /// and a path relative to the asset or player-data root, never an
    /// absolute path.
    ///
    /// # Errors
    ///
    /// Returns every diagnostic found. Parsing stops at the first error (a
    /// `ron` limitation); validation reports every failed rule at once.
    ///
    /// # Examples
    ///
    /// ```
    /// use serde::{Deserialize, Serialize};
    /// use st_data::{Genesis, Loader, MigrationError, Schema, Validate, Validator};
    ///
    /// #[derive(Debug, Serialize, Deserialize)]
    /// #[serde(deny_unknown_fields)]
    /// struct Crate { weight: u32 }
    ///
    /// impl Schema for Crate {
    ///     const KIND: &'static str = "cargo_crate";
    ///     const VERSION: u32 = 1;
    ///     type Previous = Genesis;
    ///     fn migrate(p: Genesis) -> Result<Self, MigrationError> { match p {} }
    /// }
    ///
    /// impl Validate for Crate {
    ///     fn validate(&self, v: &mut Validator) {
    ///         if self.weight == 0 {
    ///             v.field("weight", |v| v.error("cargo_crate.weight", "must not be zero"));
    ///         }
    ///     }
    /// }
    ///
    /// let loader = Loader::new();
    /// let ok = loader
    ///     .load::<Crate>("crates/a.ron", br#"(kind: "cargo_crate", version: 1, data: (weight: 3))"#)
    ///     .unwrap();
    /// assert_eq!(ok.value.weight, 3);
    ///
    /// // A typo is an error with a position, never a silent default.
    /// let err = loader
    ///     .load::<Crate>("crates/b.ron", b"(\n kind: \"cargo_crate\",\n version: 1,\n data: (wieght: 3),\n)")
    ///     .unwrap_err();
    /// assert_eq!(err[0].code.as_str(), "unknown-field");
    /// assert_eq!(err[0].span.unwrap().start.line, 4);
    /// ```
    pub fn load<T: Schema + Validate>(
        &self,
        source: &str,
        bytes: &[u8],
    ) -> Result<Loaded<T>, Vec<Diagnostic>> {
        self.load_with::<T>(source, bytes, |value, v| value.validate(v))
    }

    /// Like [`Loader::load`], without the validation step.
    ///
    /// For types that have no rules, and for tooling that wants the parsed
    /// value of a file that does not pass validation.
    ///
    /// # Errors
    ///
    /// As [`Loader::load`], except for validation failures.
    pub fn load_unvalidated<T: Schema>(
        &self,
        source: &str,
        bytes: &[u8],
    ) -> Result<Loaded<T>, Vec<Diagnostic>> {
        self.load_with::<T>(source, bytes, |_, _| {})
    }

    fn prepare<'b>(&self, source: &str, bytes: &'b [u8]) -> Result<&'b str, Vec<Diagnostic>> {
        if bytes.len() > self.max_bytes {
            return Err(vec![
                Diagnostic::new(
                    source,
                    Code::TooLarge,
                    format!(
                        "the file has {} bytes, more than the limit of {}",
                        bytes.len(),
                        self.max_bytes
                    ),
                )
                .with_help("split the data into several files, or raise the loader's limit"),
            ]);
        }
        core::str::from_utf8(bytes).map_err(|e| {
            // Point at the first invalid byte; the text before it is valid.
            let prefix = bytes
                .get(..e.valid_up_to())
                .and_then(|b| core::str::from_utf8(b).ok())
                .unwrap_or("");
            let at = Position::after(prefix);
            vec![
                Diagnostic::new(source, Code::NotUtf8, "the file is not valid UTF-8")
                    .with_span(Span { start: at, end: at })
                    .with_help("save the file as UTF-8"),
            ]
        })
    }

    fn load_with<T: Schema>(
        &self,
        source: &str,
        bytes: &[u8],
        validate: impl FnOnce(&T, &mut Validator),
    ) -> Result<Loaded<T>, Vec<Diagnostic>> {
        let text = self.prepare(source, bytes)?;
        let header = parse_header(text).map_err(|raw| vec![raw.into_diagnostic(source)])?;
        let ctx = Src {
            name: source,
            text,
            located: Cell::new(0),
            file_version: header.version,
            current_version: T::VERSION,
        };

        if T::VERSION == 0 {
            return Err(vec![Diagnostic::new(
                source,
                Code::Type,
                format!("schema `{}` has version 0; versions start at 1", T::KIND),
            )]);
        }
        if header.kind != T::KIND {
            return Err(vec![ctx.header_diag(
                Code::WrongKind,
                &["kind"],
                format!("expected kind `{}`, found `{}`", T::KIND, header.kind),
                Some("check that the file is loaded as the right kind of data".to_owned()),
            )]);
        }
        if header.version == 0 {
            return Err(vec![ctx.header_diag(
                Code::Type,
                &["version"],
                "`version` must be at least 1".to_owned(),
                None,
            )]);
        }
        if header.version > T::VERSION {
            return Err(vec![
                ctx.header_diag(
                    Code::VersionTooNew,
                    &["version"],
                    format!(
                        "`{}` file is version {}, but this build reads up to version {}",
                        T::KIND,
                        header.version,
                        T::VERSION
                    ),
                    Some(
                        "the file was written by a newer build; update the game. \
                     An older build never reads a newer file partially"
                            .to_owned(),
                    ),
                ),
            ]);
        }
        let oldest = oldest_version::<T>();
        if header.version < oldest {
            return Err(vec![ctx.header_diag(
                Code::VersionTooOld,
                &["version"],
                format!(
                    "`{}` file is version {}, but the oldest supported version is {}",
                    T::KIND,
                    header.version,
                    oldest
                ),
                Some(format!(
                    "supported versions are {oldest} to {}; this file is too old to migrate",
                    T::VERSION
                )),
            )]);
        }

        let value = match load_chain::<T>(text, header.version) {
            Ok(value) => value,
            Err(Fail::Parse(raw)) => return Err(vec![ctx.finish(raw.into_diagnostic(source))]),
            Err(Fail::NotStrict(paths)) => {
                return Err(paths
                    .into_iter()
                    .map(|path| {
                        let name = path.to_string();
                        ctx.finish(
                            Diagnostic::new(
                                source,
                                Code::SchemaNotStrict,
                                format!(
                                    "`{name}` was ignored by the schema instead of being rejected"
                                ),
                            )
                            .with_path(path)
                            .with_help(
                                "a schema type is missing `#[serde(deny_unknown_fields)]`; \
                                 every struct and enum in a schema must have it",
                            ),
                        )
                    })
                    .collect());
            }
            Err(Fail::Migration(e)) => {
                return Err(vec![ctx.finish(
                    Diagnostic::new(source, Code::Migration, e.message).with_path(e.path),
                )]);
            }
        };

        let hash = match content_hash(&value) {
            Ok(hash) => hash,
            Err(e) => return Err(vec![ctx.finish(encode_diag(source, e))]),
        };

        let mut validator = Validator::new();
        validate(&value, &mut validator);
        if !validator.is_ok() {
            return Err(validator
                .into_diagnostics(source)
                .into_iter()
                .map(|d| ctx.finish(d))
                .collect());
        }

        Ok(Loaded {
            value,
            hash,
            file_version: header.version,
        })
    }
}

fn encode_diag(source: &str, e: EncodeError) -> Diagnostic {
    let help = match e.code {
        Code::NonFinite => Some("write a finite number"),
        Code::DuplicateKey => Some("each map key may appear only once"),
        _ => None,
    };
    let mut d = Diagnostic::new(source, e.code, e.message).with_path(e.path);
    d.help = help.map(str::to_owned);
    d
}

/// The text being loaded and what is needed to put spans on diagnostics.
struct Src<'a> {
    name: &'a str,
    text: &'a str,
    located: Cell<usize>,
    file_version: u32,
    current_version: u32,
}

impl Src<'_> {
    /// A diagnostic about an envelope field (`kind` or `version`).
    fn header_diag(
        &self,
        code: Code,
        field: &[&str],
        message: String,
        help: Option<String>,
    ) -> Diagnostic {
        let path: Vec<Segment> = field
            .iter()
            .map(|f| Segment::Key((*f).to_owned()))
            .collect();
        let mut d = Diagnostic::new(self.name, code, message);
        d.span = self.span_of(&path);
        d.help = help;
        d
    }

    fn span_of(&self, path: &[Segment]) -> Option<Span> {
        let used = self.located.get();
        if used >= MAX_LOCATED {
            return None;
        }
        self.located.set(used + 1);
        let (start, end) = locate(self.text, path)?;
        Span::of_range(self.text, start, end)
    }

    /// Gives a diagnostic that has only a payload path a span, with a note
    /// when the file is older than the schema and the path cannot be found.
    fn finish(&self, mut d: Diagnostic) -> Diagnostic {
        if d.span.is_none() {
            let mut full = Vec::with_capacity(d.path.segments().len() + 1);
            full.push(Segment::Key("data".to_owned()));
            full.extend(d.path.segments().iter().cloned());
            d.span = self.span_of(&full);
        }
        if d.span.is_none() && self.file_version != self.current_version {
            let note = format!(
                "the file is version {} and is migrated to version {} before this check; \
                 the path may exist only in version {}",
                self.file_version, self.current_version, self.current_version
            );
            d.help = Some(match d.help.take() {
                Some(help) => format!("{help}; {note}"),
                None => note,
            });
        }
        d
    }
}

/// The oldest version that has a migration path to `T`.
fn oldest_version<T: Schema>() -> u32 {
    let previous = <T::Previous as Schema>::VERSION;
    // The second test only stops a malformed chain from looping.
    if previous == 0 || previous >= T::VERSION {
        T::VERSION
    } else {
        oldest_version::<T::Previous>()
    }
}

/// How a pass or a migration failed.
enum Fail {
    Parse(Raw),
    NotStrict(Vec<DataPath>),
    Migration(MigrationError),
}

/// Loads the version-`file_version` file as `T`, migrating up the chain.
fn load_chain<T: Schema>(text: &str, file_version: u32) -> Result<T, Fail> {
    const {
        assert!(
            T::VERSION == 0
                || <T::Previous as Schema>::VERSION == 0
                || <T::Previous as Schema>::VERSION + 1 == T::VERSION,
            "Schema::Previous must be the type of version VERSION - 1, or Genesis"
        );
    }
    if file_version == T::VERSION && T::VERSION != 0 {
        return strict_pass::<T>(text);
    }
    if T::VERSION == 0 || file_version > T::VERSION {
        // Unreachable for a checked chain: the caller compares the file's
        // version with the supported range first. Report rather than panic.
        return Err(Fail::Parse(Raw::new(
            Code::VersionTooOld,
            "no migration path from the file's version",
        )));
    }
    let previous = load_chain::<T::Previous>(text, file_version)?;
    T::migrate(previous).map_err(Fail::Migration)
}

/// The strict pass: the whole envelope with the payload type, unknown fields
/// denied, and `serde_ignored` as the backstop for types that forgot.
fn strict_pass<T: Schema>(text: &str) -> Result<T, Fail> {
    let (envelope, ignored): (Envelope<T>, _) = run_pass(text).map_err(Fail::Parse)?;
    if ignored.is_empty() {
        Ok(envelope.data)
    } else {
        Err(Fail::NotStrict(ignored))
    }
}

/// The envelope of a data file. Struct names are optional in RON, so files
/// are written without one.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<D> {
    // Both are checked by the header pass; the strict pass only needs them
    // to be present and well-typed so that unknown header fields are denied.
    #[allow(dead_code, reason = "read by the header pass")]
    kind: String,
    #[allow(dead_code, reason = "read by the header pass")]
    version: u32,
    data: D,
}

/// The header pass: lenient about the payload and unknown header fields, so
/// that `version-too-new` wins even when a later build adds header fields.
fn parse_header(text: &str) -> Result<Header, Raw> {
    // Named like the envelope of the strict pass: `ron` checks a struct name
    // that a file writes against the Rust name, and both passes must agree.
    #[derive(Deserialize)]
    #[serde(rename = "Envelope")]
    struct RawHeader {
        kind: String,
        version: u32,
        #[allow(dead_code, reason = "skipped on purpose")]
        data: IgnoredAny,
    }
    let (header, _): (RawHeader, _) = run_pass(text)?;
    Ok(Header {
        kind: header.kind,
        version: header.version,
    })
}

/// A failure of a pass, before it has a source name.
struct Raw {
    code: Code,
    message: String,
    help: Option<String>,
    span: Option<Span>,
    path: DataPath,
}

impl Raw {
    fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            help: None,
            span: None,
            path: DataPath::new(),
        }
    }

    fn into_diagnostic(self, source: &str) -> Diagnostic {
        Diagnostic {
            source: source.to_owned(),
            span: self.span,
            path: self.path,
            code: self.code,
            rule: None,
            message: self.message,
            help: self.help,
        }
    }
}

/// Parses `text` as a `D`, with `ron` configured as the ADR specifies and
/// wrapped in `serde_path_to_error` (inner) and `serde_ignored` (outer).
/// Returns the value and the paths of fields that were ignored.
fn run_pass<D: serde::de::DeserializeOwned>(text: &str) -> Result<(D, Vec<DataPath>), Raw> {
    let options = ron::Options::default()
        .with_default_extension(Extensions::IMPLICIT_SOME)
        .with_recursion_limit(RECURSION_LIMIT);
    let mut de = ron::Deserializer::from_str_with_options(text, &options)
        .map_err(|e| from_ron(&e.code, Some(Span::from_ron(e.span)), DataPath::new()))?;

    let mut track = serde_path_to_error::Track::new();
    let mut ignored = Vec::new();
    let result: Result<D, ron::Error> = {
        let inner = serde_path_to_error::Deserializer::new(&mut de, &mut track);
        serde_ignored::deserialize(inner, |p| ignored.push(ignored_path(&p)))
    };
    match result {
        Ok(value) => match de.end() {
            Ok(()) => Ok((value, ignored)),
            Err(e) => {
                let e = de.span_error(e);
                Err(from_ron(
                    &e.code,
                    Some(Span::from_ron(e.span)),
                    DataPath::new(),
                ))
            }
        },
        Err(e) => {
            let path = tracked_path(&track.path());
            let class = e.clone();
            let spanned = de.span_error(e);
            Err(from_ron(&class, Some(Span::from_ron(spanned.span)), path))
        }
    }
}

/// Converts the path that `serde_path_to_error` tracked. A leading `data`
/// (the envelope field that holds the payload) is dropped.
fn tracked_path(path: &serde_path_to_error::Path) -> DataPath {
    let mut segments: Vec<Segment> = path
        .iter()
        .filter_map(|s| match s {
            serde_path_to_error::Segment::Seq { index } => Some(Segment::Index(*index)),
            serde_path_to_error::Segment::Map { key } => Some(Segment::Key(key.clone())),
            serde_path_to_error::Segment::Enum { variant } => {
                Some(Segment::Variant(variant.clone()))
            }
            _ => None,
        })
        .collect();
    drop_data_prefix(&mut segments);
    DataPath::from_segments(segments)
}

/// Converts the path of a field that `serde_ignored` saw being ignored.
fn ignored_path(path: &serde_ignored::Path<'_>) -> DataPath {
    fn walk(path: &serde_ignored::Path<'_>, out: &mut Vec<Segment>) {
        match path {
            serde_ignored::Path::Root => {}
            serde_ignored::Path::Seq { parent, index } => {
                walk(parent, out);
                out.push(Segment::Index(*index));
            }
            serde_ignored::Path::Map { parent, key } => {
                walk(parent, out);
                out.push(Segment::Key(key.clone()));
            }
            serde_ignored::Path::Some { parent }
            | serde_ignored::Path::NewtypeStruct { parent }
            | serde_ignored::Path::NewtypeVariant { parent } => walk(parent, out),
        }
    }
    let mut segments = Vec::new();
    walk(path, &mut segments);
    drop_data_prefix(&mut segments);
    DataPath::from_segments(segments)
}

fn drop_data_prefix(segments: &mut Vec<Segment>) {
    if matches!(segments.first(), Some(Segment::Key(k)) if k == "data") {
        segments.remove(0);
    }
}

/// The nearest of `expected` to `found` by edit distance, when it is close
/// enough to be a plausible typo.
fn nearest<'a>(found: &str, expected: &[&'a str]) -> Option<&'a str> {
    let len = found.chars().count();
    expected
        .iter()
        .map(|candidate| (edit_distance(found, candidate), *candidate))
        .min_by_key(|(d, _)| *d)
        .filter(|(d, _)| *d <= 2 && *d < len)
        .map(|(_, candidate)| candidate)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row.first().copied().unwrap_or(0);
        if let Some(first) = row.first_mut() {
            *first = i + 1;
        }
        for (j, cb) in b.iter().enumerate() {
            let above = row.get(j + 1).copied().unwrap_or(0);
            let left = row.get(j).copied().unwrap_or(0);
            let cost = usize::from(ca != *cb);
            let best = (diagonal + cost).min(above + 1).min(left + 1);
            if let Some(slot) = row.get_mut(j + 1) {
                *slot = best;
            }
            diagonal = above;
        }
    }
    row.last().copied().unwrap_or(0)
}

fn names(list: &[&str]) -> String {
    list.iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Turns a `ron` error into a coded diagnostic body.
///
/// This depends on the variants and the wording of `ron` 0.12 errors; the
/// `ron_contract` tests fail when an upgrade changes them.
fn from_ron(err: &ron::Error, span: Option<Span>, path: DataPath) -> Raw {
    use ron::Error as E;
    let mut raw = Raw::new(Code::Type, err.to_string());
    raw.span = span;
    raw.path = path;
    match err {
        E::NoSuchStructField {
            expected, found, ..
        } => {
            raw.code = Code::UnknownField;
            raw.message = if expected.is_empty() {
                format!("unknown field `{found}`; this struct has no fields")
            } else {
                format!(
                    "unknown field `{found}`; expected one of {}",
                    names(expected)
                )
            };
            raw.help = nearest(found, expected).map(|n| format!("did you mean `{n}`?"));
        }
        E::NoSuchEnumVariant {
            expected, found, ..
        } => {
            raw.code = Code::UnknownVariant;
            raw.message = format!(
                "unknown variant `{found}`; expected one of {}",
                names(expected)
            );
            raw.help = nearest(found, expected).map(|n| format!("did you mean `{n}`?"));
        }
        E::MissingStructField { field, .. } => {
            raw.code = Code::MissingField;
            raw.message = format!("missing field `{field}`");
            raw.help = Some(format!("add `{field}: ...` to this struct"));
        }
        E::DuplicateStructField { field, .. } => {
            raw.code = Code::DuplicateField;
            raw.message = format!("field `{field}` is written twice");
            raw.help = Some("remove one of them".to_owned());
        }
        E::Message(m) if m == DUPLICATE_KEY_TAG => {
            raw.code = Code::DuplicateKey;
            raw.message = "this map key appears more than once".to_owned();
            raw.help = Some("each key may appear only once".to_owned());
        }
        E::ExceededRecursionLimit => {
            raw.code = Code::Syntax;
            raw.message = format!("values are nested deeper than {RECURSION_LIMIT} levels");
        }
        e if is_syntax(e) => raw.code = Code::Syntax,
        _ => {}
    }
    raw
}

/// Whether a `ron` error is about the text rather than about a value's type.
fn is_syntax(e: &ron::Error) -> bool {
    use ron::Error as E;
    matches!(
        e,
        E::Fmt
            | E::Io(_)
            | E::Eof
            | E::ExpectedArrayEnd
            | E::ExpectedAttribute
            | E::ExpectedAttributeEnd
            | E::ExpectedComma
            | E::ExpectedMapColon
            | E::ExpectedMapEnd
            | E::ExpectedOptionEnd
            | E::ExpectedStructLikeEnd
            | E::ExpectedStringEnd
            | E::ExpectedIdentifier
            | E::ExpectedByteLiteral
            | E::ExpectedChar
            | E::ExpectedRawValue
            | E::ExpectedRangeSyntax
            | E::ExpectedStructName(_)
            | E::FloatUnderscore
            | E::InvalidEscape(_)
            | E::InvalidIntegerDigit { .. }
            | E::InvalidIdentifier(_)
            | E::SuggestRawIdentifier(_)
            | E::NoSuchExtension(_)
            | E::UnclosedBlockComment
            | E::UnclosedLineComment
            | E::UnderscoreAtBeginning
            | E::UnexpectedChar(_)
            | E::Utf8Error(_)
            | E::TrailingCharacters
    )
}
