//! Validation rules and the collector that gathers every failed rule.

use core::fmt::Display;

use crate::diagnostic::{Code, Diagnostic};
use crate::path::{DataPath, Segment};
use crate::unique_map::UniqueMap;

/// A value that checks its own rules after it has been parsed and migrated.
///
/// Rules live in the crate that owns the schema type. A rule reports a
/// failure with [`Validator::error`] and keeps going, so one run reports
/// every failed rule at once. Rules that span several files are checked by
/// the owning crate over the whole data set, with the same [`Diagnostic`]
/// type.
///
/// # Examples
///
/// ```
/// use st_data::{Validate, Validator};
///
/// struct Design { mass: f32, sections: Vec<f32> }
///
/// impl Validate for Design {
///     fn validate(&self, v: &mut Validator) {
///         v.field("mass", |v| {
///             if self.mass <= 0.0 {
///                 v.error("ship_design.mass_positive", "mass must be positive");
///             }
///         });
///         v.field("sections", |v| {
///             for (i, s) in self.sections.iter().enumerate() {
///                 v.index(i, |v| {
///                     if *s <= 0.0 {
///                         v.error("ship_design.section_mass", "section mass must be positive");
///                     }
///                 });
///             }
///         });
///     }
/// }
///
/// let mut v = Validator::new();
/// Design { mass: -1.0, sections: vec![1.0, 0.0] }.validate(&mut v);
/// let diagnostics = v.into_diagnostics("designs/x.ron");
/// assert_eq!(diagnostics.len(), 2);
/// assert_eq!(diagnostics[1].path.to_string(), "sections[1]");
/// ```
pub trait Validate {
    /// Checks every rule, reporting each failure on `v`.
    fn validate(&self, v: &mut Validator);
}

/// One failed rule, before it gets a source name and a span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// Where in the payload the rule failed.
    pub path: DataPath,
    /// The rule identifier chosen by the owning crate.
    pub rule: String,
    /// What is wrong.
    pub message: String,
    /// What to do about it.
    pub help: Option<String>,
}

/// Collects failed rules and tracks the path of the value being checked.
///
/// # Examples
///
/// ```
/// use st_data::Validator;
///
/// let mut v = Validator::new();
/// v.field("armor", |v| v.index(3, |v| v.error("rule.id", "too thick")));
/// assert_eq!(v.violations()[0].path.to_string(), "armor[3]");
/// ```
#[derive(Debug, Default)]
pub struct Validator {
    path: Vec<Segment>,
    violations: Vec<Violation>,
}

impl Validator {
    /// A validator with no violations, positioned at the payload root.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn scoped<R>(&mut self, segment: Segment, f: impl FnOnce(&mut Self) -> R) -> R {
        self.path.push(segment);
        let result = f(self);
        self.path.pop();
        result
    }

    /// Runs `f` with the path extended by a struct field name.
    pub fn field<R>(&mut self, name: &str, f: impl FnOnce(&mut Self) -> R) -> R {
        self.scoped(Segment::Key(name.to_owned()), f)
    }

    /// Runs `f` with the path extended by a sequence index.
    pub fn index<R>(&mut self, index: usize, f: impl FnOnce(&mut Self) -> R) -> R {
        self.scoped(Segment::Index(index), f)
    }

    /// Runs `f` with the path extended by a map key.
    pub fn key<R>(&mut self, key: impl Display, f: impl FnOnce(&mut Self) -> R) -> R {
        self.scoped(Segment::Key(key.to_string()), f)
    }

    /// Runs `f` with the path extended by an enum variant name.
    pub fn variant<R>(&mut self, name: &str, f: impl FnOnce(&mut Self) -> R) -> R {
        self.scoped(Segment::Variant(name.to_owned()), f)
    }

    /// Reports a failed rule at the current path.
    pub fn error(&mut self, rule: &str, message: impl Into<String>) {
        self.push(rule, message.into(), None);
    }

    /// Reports a failed rule at the current path, with a hint on how to fix
    /// it.
    pub fn error_with_help(
        &mut self,
        rule: &str,
        message: impl Into<String>,
        help: impl Into<String>,
    ) {
        self.push(rule, message.into(), Some(help.into()));
    }

    fn push(&mut self, rule: &str, message: String, help: Option<String>) {
        self.violations.push(Violation {
            path: DataPath::from_segments(self.path.clone()),
            rule: rule.to_owned(),
            message,
            help,
        });
    }

    /// The failures reported so far.
    #[must_use]
    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }

    /// Whether no rule has failed.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.violations.is_empty()
    }

    /// Converts the failures to diagnostics (without spans) for `source`.
    #[must_use]
    pub fn into_diagnostics(self, source: &str) -> Vec<Diagnostic> {
        self.violations
            .into_iter()
            .map(|v| {
                let mut d = Diagnostic::new(source, Code::Validation, v.message)
                    .with_path(v.path)
                    .with_rule(v.rule);
                d.help = v.help;
                d
            })
            .collect()
    }
}

macro_rules! trivially_valid {
    ($($t:ty),* $(,)?) => {$(
        impl Validate for $t {
            fn validate(&self, _: &mut Validator) {}
        }
    )*};
}
trivially_valid!(
    bool,
    char,
    str,
    String,
    i8,
    i16,
    i32,
    i64,
    u8,
    u16,
    u32,
    u64,
    f32,
    f64,
    ()
);

impl<T: Validate + ?Sized> Validate for &T {
    fn validate(&self, v: &mut Validator) {
        (**self).validate(v);
    }
}

impl<T: Validate + ?Sized> Validate for Box<T> {
    fn validate(&self, v: &mut Validator) {
        (**self).validate(v);
    }
}

impl<T: Validate> Validate for Option<T> {
    fn validate(&self, v: &mut Validator) {
        if let Some(inner) = self {
            inner.validate(v);
        }
    }
}

impl<T: Validate> Validate for [T] {
    fn validate(&self, v: &mut Validator) {
        for (i, item) in self.iter().enumerate() {
            v.index(i, |v| item.validate(v));
        }
    }
}

impl<T: Validate> Validate for Vec<T> {
    fn validate(&self, v: &mut Validator) {
        self.as_slice().validate(v);
    }
}

impl<K: Ord + Display, V: Validate> Validate for UniqueMap<K, V> {
    fn validate(&self, v: &mut Validator) {
        for (key, value) in self.iter() {
            v.key(key, |v| value.validate(v));
        }
    }
}
