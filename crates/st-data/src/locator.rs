//! A small RON span locator: finds the text of the value at a path.
//!
//! Errors raised after parsing (`non-finite`, `migration`, `validation`, and
//! the header checks) carry a path but no position, because the parser has
//! finished by then. This module tokenizes the same text again, just far
//! enough to walk a path: comments (including nested block comments),
//! strings, raw strings, characters, numbers, identifiers and the three
//! bracket kinds. It does not validate anything; the text has already been
//! parsed by `ron`. It is best-effort: when it cannot resolve a path it
//! returns `None` and the diagnostic keeps the path without a span.
//!
//! The span locator depends on the shape of the syntax that `ron` 0.12
//! accepts, and the `ron_contract` tests depend on `ron`'s error and
//! position format. Re-check both whenever `ron` is upgraded.

use crate::path::Segment;

/// The deepest nesting the locator follows. `ron` itself stops at 128, so a
/// text that parsed never gets near this; the cap only protects the stack
/// against text that did not go through `ron`.
const MAX_DEPTH: usize = 256;

/// The byte range of a value.
#[derive(Clone, Copy, Debug)]
struct Val {
    /// Where descent continues (the body of a variant, otherwise the start).
    start: usize,
    end: usize,
    /// Where the reported span starts (the variant name, when there is one).
    report: usize,
}

struct Child {
    key: Option<(usize, usize)>,
    value: (usize, usize),
}

struct Scan<'a> {
    src: &'a str,
    b: &'a [u8],
}

/// Finds the byte range of the value at `path`, counted from the root value
/// of `src` (the envelope). Returns `None` when the path cannot be resolved.
pub(crate) fn locate(src: &str, path: &[Segment]) -> Option<(usize, usize)> {
    let sc = Scan {
        src,
        b: src.as_bytes(),
    };
    let start = sc.root_start()?;
    let end = sc.value_end(start, 0)?;
    let mut cur = Val {
        start,
        end,
        report: start,
    };
    for seg in path {
        cur = sc.descend(cur, seg, 0)?;
    }
    Some((cur.report, cur.end))
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

impl Scan<'_> {
    fn at(&self, i: usize) -> Option<u8> {
        self.b.get(i).copied()
    }

    /// Skips whitespace, line comments and (nested) block comments.
    fn trivia(&self, mut i: usize) -> usize {
        loop {
            match self.at(i) {
                Some(c) if c.is_ascii_whitespace() => i += 1,
                Some(b'/') if self.at(i + 1) == Some(b'/') => {
                    while !matches!(self.at(i), Some(b'\n') | None) {
                        i += 1;
                    }
                }
                Some(b'/') if self.at(i + 1) == Some(b'*') => {
                    let mut depth = 1usize;
                    i += 2;
                    while depth > 0 {
                        match (self.at(i), self.at(i + 1)) {
                            (None, _) => return i,
                            (Some(b'*'), Some(b'/')) => {
                                depth -= 1;
                                i += 2;
                            }
                            (Some(b'/'), Some(b'*')) => {
                                depth += 1;
                                i += 2;
                            }
                            _ => i += 1,
                        }
                    }
                }
                _ => return i,
            }
        }
    }

    /// Start of the root value: after comments and `#![enable(..)]`
    /// attributes.
    fn root_start(&self) -> Option<usize> {
        let mut i = self.trivia(0);
        while self.at(i) == Some(b'#') {
            while self.at(i)? != b']' {
                i += 1;
            }
            i = self.trivia(i + 1);
        }
        Some(i)
    }

    /// End (exclusive) of the value that starts at `i`.
    fn value_end(&self, i: usize, depth: usize) -> Option<usize> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.at(i)? {
            b'"' => self.string_end(i),
            b'\'' => self.char_end(i),
            b'(' | b'[' | b'{' => self.children(i, depth).map(|(_, end)| end),
            c if c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.') => Some(self.number_end(i)),
            b'r' if matches!(self.at(i + 1), Some(b'"' | b'#'))
                && self.raw_string_end(i + 1).is_some() =>
            {
                self.raw_string_end(i + 1)
            }
            b'b' if self.at(i + 1) == Some(b'"') => self.string_end(i + 1),
            b'b' if self.at(i + 1) == Some(b'\'') => self.char_end(i + 1),
            b'b' if self.at(i + 1) == Some(b'r') && self.raw_string_end(i + 2).is_some() => {
                self.raw_string_end(i + 2)
            }
            c if is_ident_start(c) => {
                let end = self.ident_end(i);
                // `Name(..)`: a struct name, a variant or `Some` in front of
                // its parenthesised body.
                let next = self.trivia(end);
                if self.at(next) == Some(b'(') {
                    self.children(next, depth).map(|(_, e)| e)
                } else {
                    Some(end)
                }
            }
            _ => None,
        }
    }

    fn ident_end(&self, mut i: usize) -> usize {
        // Raw identifiers: `r#name`.
        if self.at(i) == Some(b'r') && self.at(i + 1) == Some(b'#') {
            i += 2;
        }
        while self.at(i).is_some_and(is_ident_char) {
            i += 1;
        }
        i
    }

    fn number_end(&self, mut i: usize) -> usize {
        // Covers signs, digits, `_`, `.`, exponents and `0x`/`0b`/`0o`
        // prefixes, and `-inf`; a number never contains a delimiter.
        while self
            .at(i)
            .is_some_and(|c| is_ident_char(c) || matches!(c, b'+' | b'-' | b'.'))
        {
            i += 1;
        }
        i
    }

    fn string_end(&self, open: usize) -> Option<usize> {
        let mut i = open + 1;
        loop {
            match self.at(i)? {
                b'\\' => i += 2,
                b'"' => return Some(i + 1),
                _ => i += 1,
            }
        }
    }

    fn char_end(&self, open: usize) -> Option<usize> {
        let mut i = open + 1;
        loop {
            match self.at(i)? {
                b'\\' => i += 2,
                b'\'' => return Some(i + 1),
                _ => i += 1,
            }
        }
    }

    /// `at` is the first byte after the `r` (or `br`): `#`* then `"`.
    fn raw_string_end(&self, at: usize) -> Option<usize> {
        let mut i = at;
        let mut hashes = 0usize;
        while self.at(i) == Some(b'#') {
            hashes += 1;
            i += 1;
        }
        if self.at(i) != Some(b'"') {
            return None;
        }
        i += 1;
        loop {
            if self.at(i)? == b'"' && (0..hashes).all(|k| self.at(i + 1 + k) == Some(b'#')) {
                return Some(i + 1 + hashes);
            }
            i += 1;
        }
    }

    /// Parses the elements of the bracketed group that opens at `open`.
    /// Returns them with the end of the group.
    fn children(&self, open: usize, depth: usize) -> Option<(Vec<Child>, usize)> {
        let close = match self.at(open)? {
            b'(' => b')',
            b'[' => b']',
            b'{' => b'}',
            _ => return None,
        };
        let mut out = Vec::new();
        let mut i = open + 1;
        loop {
            i = self.trivia(i);
            if self.at(i)? == close {
                return Some((out, i + 1));
            }
            let first_end = self.value_end(i, depth + 1)?;
            let after = self.trivia(first_end);
            if self.at(after) == Some(b':') {
                let value_start = self.trivia(after + 1);
                let value_end = self.value_end(value_start, depth + 1)?;
                out.push(Child {
                    key: Some((i, first_end)),
                    value: (value_start, value_end),
                });
                i = value_end;
            } else {
                out.push(Child {
                    key: None,
                    value: (i, first_end),
                });
                i = first_end;
            }
            i = self.trivia(i);
            match self.at(i)? {
                b',' => i += 1,
                c if c == close => {}
                _ => return None,
            }
        }
    }

    /// Whether the key text in `start..end` names `want`.
    fn key_matches(&self, start: usize, end: usize, want: &str) -> bool {
        let Some(text) = self.src.get(start..end) else {
            return false;
        };
        if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
            // A quoted key: compare after the two escapes that matter.
            return inner.replace("\\\"", "\"").replace("\\\\", "\\") == want;
        }
        text.strip_prefix("r#").unwrap_or(text) == want
    }

    /// Splits a value into an optional leading name (a struct name, a
    /// variant name or `Some`) and the bracketed body, if it has one.
    fn shape(&self, v: Val) -> Shape {
        match self.at(v.start) {
            Some(b'(' | b'[' | b'{') => Shape::Body,
            Some(c) if is_ident_start(c) => {
                let name_end = self.ident_end(v.start);
                let next = self.trivia(name_end);
                if self.at(next) == Some(b'(') {
                    Shape::Named {
                        name: (v.start, name_end),
                        body: next,
                    }
                } else {
                    Shape::Bare {
                        name: (v.start, name_end),
                    }
                }
            }
            _ => Shape::Scalar,
        }
    }

    fn descend(&self, v: Val, seg: &Segment, depth: usize) -> Option<Val> {
        if depth > MAX_DEPTH {
            return None;
        }
        match (self.shape(v), seg) {
            (Shape::Named { name, body }, Segment::Variant(want)) => {
                if self.src.get(name.0..name.1)? != want {
                    return None;
                }
                Some(Val {
                    start: body,
                    end: v.end,
                    report: v.report,
                })
            }
            (Shape::Bare { name }, Segment::Variant(want)) => {
                if self.src.get(name.0..name.1)? != want {
                    return None;
                }
                Some(v)
            }
            (Shape::Named { body, .. }, _) => {
                let inner = Val {
                    start: body,
                    end: v.end,
                    report: body,
                };
                self.descend_body(inner, seg, depth)
            }
            (Shape::Body, _) => self.descend_body(v, seg, depth),
            (Shape::Bare { .. } | Shape::Scalar, _) => None,
        }
    }

    fn descend_body(&self, v: Val, seg: &Segment, depth: usize) -> Option<Val> {
        let (children, _) = self.children(v.start, depth)?;
        let pick = |child: &Child| Val {
            start: child.value.0,
            end: child.value.1,
            report: child.value.0,
        };
        match seg {
            Segment::Key(want) => {
                for child in &children {
                    if let Some((ks, ke)) = child.key
                        && self.key_matches(ks, ke, want)
                    {
                        return Some(pick(child));
                    }
                }
            }
            Segment::Index(want) => {
                return children
                    .iter()
                    .filter(|c| c.key.is_none())
                    .nth(*want)
                    .map(pick);
            }
            Segment::Variant(_) => {}
        }
        // A newtype wrapper `(inner)` is transparent in paths.
        let mut unkeyed = children.iter().filter(|c| c.key.is_none());
        match (unkeyed.next(), unkeyed.next()) {
            (Some(only), None) => self.descend(pick(only), seg, depth + 1),
            _ => None,
        }
    }
}

enum Shape {
    Scalar,
    /// An identifier on its own: a unit variant, `None`, `true`, `inf`.
    Bare {
        name: (usize, usize),
    },
    /// A bracketed group.
    Body,
    /// `Name(..)`.
    Named {
        name: (usize, usize),
        body: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(src: &str, path: &[Segment]) -> Option<String> {
        locate(src, path).map(|(s, e)| src[s..e].to_owned())
    }

    fn key(s: &str) -> Segment {
        Segment::Key(s.to_owned())
    }

    #[test]
    fn walks_fields_sequences_and_variants() {
        let src = "#![enable(implicit_some)]\n// c\n(kind: \"k\", /* a /* b */ c */ version: 2,\n\
                   data: (name: \"x\", sections: [Cargo(capacity: 1.5), Engine(0.5), Core],\n\
                   tags: {\"a\": 1, \"b\": 2}))";
        let data = [key("data")];
        assert_eq!(text(src, &[key("version")]).as_deref(), Some("2"));
        assert_eq!(text(src, &[key("kind")]).as_deref(), Some("\"k\""));
        assert!(text(src, &data).unwrap().starts_with("(name"));
        let sections = [key("data"), key("sections")];
        let cap = [
            key("data"),
            key("sections"),
            Segment::Index(0),
            Segment::Variant("Cargo".into()),
            key("capacity"),
        ];
        assert_eq!(text(src, &cap).as_deref(), Some("1.5"));
        let whole = [
            key("data"),
            key("sections"),
            Segment::Index(0),
            Segment::Variant("Cargo".into()),
        ];
        assert_eq!(text(src, &whole).as_deref(), Some("Cargo(capacity: 1.5)"));
        let engine = [
            key("data"),
            key("sections"),
            Segment::Index(1),
            Segment::Variant("Engine".into()),
        ];
        assert_eq!(text(src, &engine).as_deref(), Some("Engine(0.5)"));
        let core = [
            key("data"),
            key("sections"),
            Segment::Index(2),
            Segment::Variant("Core".into()),
        ];
        assert_eq!(text(src, &core).as_deref(), Some("Core"));
        assert!(text(src, &sections).unwrap().ends_with("Core]"));
        let tag = [key("data"), key("tags"), key("b")];
        assert_eq!(text(src, &tag).as_deref(), Some("2"));
    }

    #[test]
    fn skips_struct_names_and_some() {
        let src = "Envelope(kind: \"k\", version: 1, data: Design(mass: Some(2.0)))";
        let path = [key("data"), key("mass")];
        assert_eq!(text(src, &path).as_deref(), Some("Some(2.0)"));
        let inner = [key("data"), key("mass"), Segment::Index(0)];
        assert_eq!(text(src, &inner).as_deref(), Some("2.0"));
    }

    #[test]
    fn handles_strings_with_delimiters() {
        let src = "(kind: \"a)\\\"(\", version: r#\"x\"y)\"#, data: (c: ')', d: 4))";
        assert_eq!(text(src, &[key("data"), key("d")]).as_deref(), Some("4"));
    }

    #[test]
    fn unresolvable_paths_give_none() {
        let src = "(kind: \"k\", version: 1, data: (a: 1))";
        assert_eq!(text(src, &[key("data"), key("zzz")]), None);
        assert_eq!(text(src, &[key("data"), Segment::Index(3)]), None);
        assert_eq!(locate("", &[]), None);
        assert_eq!(locate("(((", &[key("a")]), None);
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let deep = "(".repeat(100_000);
        assert_eq!(locate(&deep, &[key("a")]), None);
        let deep = "[".repeat(100_000);
        assert_eq!(locate(&deep, &[Segment::Index(0)]), None);
    }
}
