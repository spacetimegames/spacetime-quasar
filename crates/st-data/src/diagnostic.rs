//! Located, coded, actionable errors.

use core::fmt;

use crate::path::DataPath;

/// A 1-based line and column, counted in Unicode scalar values.
///
/// This is the `ron` convention: `\r` and tabs count as one column each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number, counted in Unicode scalar values.
    pub col: u32,
}

/// A start and an end [`Position`] in a source text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// Where the offending text starts.
    pub start: Position,
    /// Where the offending text ends.
    pub end: Position,
}

impl Position {
    /// The position at the end of `prefix`, i.e. where the next character
    /// after `prefix` would sit. Mirrors `ron`'s own computation so that
    /// spans we compute agree with spans `ron` reports.
    pub(crate) fn after(prefix: &str) -> Self {
        let mut line = 1u32;
        let mut col = 1u32;
        for c in prefix.chars() {
            if c == '\n' {
                line = line.saturating_add(1);
                col = 1;
            } else {
                col = col.saturating_add(1);
            }
        }
        Self { line, col }
    }

    pub(crate) fn from_ron(p: ron::error::Position) -> Self {
        Self {
            line: u32::try_from(p.line).unwrap_or(u32::MAX),
            col: u32::try_from(p.col).unwrap_or(u32::MAX),
        }
    }
}

impl Span {
    pub(crate) fn from_ron(s: ron::error::Span) -> Self {
        Self {
            start: Position::from_ron(s.start),
            end: Position::from_ron(s.end),
        }
    }

    /// The span of the byte range `start..end` of `text`.
    pub(crate) fn of_range(text: &str, start: usize, end: usize) -> Option<Self> {
        let before = text.get(..start)?;
        let through = text.get(..end)?;
        Some(Self {
            start: Position::after(before),
            end: Position::after(through),
        })
    }
}

/// The stable, kebab-case code of a [`Diagnostic`].
///
/// The code is the contract with tools and tests: messages may be reworded,
/// codes are not. The enum is `#[non_exhaustive]`; match with a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Code {
    /// The input is larger than the loader's limit (`too-large`).
    TooLarge,
    /// The input is not valid UTF-8 (`not-utf8`).
    NotUtf8,
    /// The text is not well-formed RON (`syntax`).
    Syntax,
    /// A value has the wrong type or is out of range (`type`).
    Type,
    /// A field that the schema does not have (`unknown-field`).
    UnknownField,
    /// A required field is missing (`missing-field`).
    MissingField,
    /// A field is written twice (`duplicate-field`).
    DuplicateField,
    /// A map key is written twice (`duplicate-key`).
    DuplicateKey,
    /// An enum variant that the schema does not have (`unknown-variant`).
    UnknownVariant,
    /// The file's kind is not the kind that was asked for (`wrong-kind`).
    WrongKind,
    /// No schema is registered for the file's kind (`unknown-kind`).
    UnknownKind,
    /// The file is newer than this build reads (`version-too-new`).
    VersionTooNew,
    /// The file is older than any version with a migration path
    /// (`version-too-old`).
    VersionTooOld,
    /// A schema type ignored a field instead of rejecting it
    /// (`schema-not-strict`).
    SchemaNotStrict,
    /// A float is NaN or infinite (`non-finite`).
    NonFinite,
    /// A migration step failed (`migration`).
    Migration,
    /// A validation rule failed (`validation`).
    Validation,
    /// A shipped asset is not at its kind's current version
    /// (`outdated-asset`).
    OutdatedAsset,
    /// A previously loaded file is gone (`missing-source`).
    MissingSource,
}

impl Code {
    /// The stable kebab-case string for this code.
    ///
    /// # Examples
    ///
    /// ```
    /// assert_eq!(st_data::Code::VersionTooNew.as_str(), "version-too-new");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TooLarge => "too-large",
            Self::NotUtf8 => "not-utf8",
            Self::Syntax => "syntax",
            Self::Type => "type",
            Self::UnknownField => "unknown-field",
            Self::MissingField => "missing-field",
            Self::DuplicateField => "duplicate-field",
            Self::DuplicateKey => "duplicate-key",
            Self::UnknownVariant => "unknown-variant",
            Self::WrongKind => "wrong-kind",
            Self::UnknownKind => "unknown-kind",
            Self::VersionTooNew => "version-too-new",
            Self::VersionTooOld => "version-too-old",
            Self::SchemaNotStrict => "schema-not-strict",
            Self::NonFinite => "non-finite",
            Self::Migration => "migration",
            Self::Validation => "validation",
            Self::OutdatedAsset => "outdated-asset",
            Self::MissingSource => "missing-source",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One located, coded error. The library never prints a diagnostic.
///
/// [`Display`](fmt::Display) renders one line, for logs and test output:
///
/// ```text
/// error[unknown-field]: assets/ships/shuttle.ron:7:9: sections[0].mas: unknown field `mas`; expected one of `name`, `mass`, `armor`
/// ```
///
/// The line has the code, the source name, the `line:col` of the start of
/// the span when there is one, the path when there is one, and the message;
/// a validation rule identifier is appended in brackets. The alternate form
/// (`{:#}`) adds a second line with the help text, when there is one.
///
/// # Examples
///
/// ```
/// use st_data::{Code, DataPath, Diagnostic};
///
/// let d = Diagnostic::new("designs/hauler.ron", Code::Validation, "mass is negative")
///     .with_path(DataPath::new().key("mass"))
///     .with_rule("ship_design.mass_budget");
/// assert_eq!(
///     d.to_string(),
///     "error[validation]: designs/hauler.ron: mass: mass is negative [ship_design.mass_budget]"
/// );
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The source name the caller passed in. Names use `/` as the separator
    /// on every platform and are relative, so logs never carry absolute
    /// paths.
    pub source: String,
    /// Where in the source the problem is, when known.
    pub span: Option<Span>,
    /// Where in the payload the problem is. Empty when not applicable.
    pub path: DataPath,
    /// The stable code.
    pub code: Code,
    /// For [`Code::Validation`], the rule identifier chosen by the owning
    /// crate (for example `ship_design.mass_budget`).
    pub rule: Option<String>,
    /// What is wrong.
    pub message: String,
    /// What to do about it: the nearest field name, the supported version
    /// range, and so on.
    pub help: Option<String>,
}

impl Diagnostic {
    /// A diagnostic with a source, a code and a message; everything else is
    /// empty.
    #[must_use]
    pub fn new(source: impl Into<String>, code: Code, message: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            span: None,
            path: DataPath::new(),
            code,
            rule: None,
            message: message.into(),
            help: None,
        }
    }

    /// Sets the span.
    #[must_use]
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// Sets the payload path.
    #[must_use]
    pub fn with_path(mut self, path: DataPath) -> Self {
        self.path = path;
        self
    }

    /// Sets the validation rule identifier.
    #[must_use]
    pub fn with_rule(mut self, rule: impl Into<String>) -> Self {
        self.rule = Some(rule.into());
        self
    }

    /// Sets the help text.
    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[{}]: {}", self.code, self.source)?;
        if let Some(span) = &self.span {
            write!(f, ":{}:{}", span.start.line, span.start.col)?;
        }
        f.write_str(": ")?;
        if !self.path.is_empty() {
            write!(f, "{}: ", self.path)?;
        }
        f.write_str(&self.message)?;
        if let Some(rule) = &self.rule {
            write!(f, " [{rule}]")?;
        }
        if f.alternate()
            && let Some(help) = &self.help
        {
            write!(f, "\n  help: {help}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostic {}
