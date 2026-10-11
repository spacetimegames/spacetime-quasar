//! Data loading, schema versioning, content hashing and validation.
//!
//! `st-data` turns the bytes of a `.ron` data file into a typed value, or
//! into a list of actionable [`Diagnostic`]s. It is generic: it has no game
//! types, no file system access and no threads, so it compiles for every
//! target including `wasm32-unknown-unknown`. A caller reads the bytes
//! (through `st-platform`, asynchronously where the platform needs it) and
//! hands them over together with a *source name*.
//!
//! # The envelope
//!
//! Every file is a RON struct with exactly three fields, written without a
//! struct name:
//!
//! ```text
//! (
//!     kind: "ship_design",   // the schema family
//!     version: 3,            // the schema version, increasing by 1 per change
//!     data: ( name: "Shuttle" /* ... */ ),
//! )
//! ```
//!
//! The loader enables `ron`'s `implicit_some` extension, so an `Option`
//! field is written as a bare value or omitted (with `#[serde(default)]`).
//!
//! Loading takes two passes over the same bytes. The *header pass* is
//! lenient: it reads `kind` and `version`, skips the payload and ignores
//! unknown header fields, so a file from a newer build always yields
//! `version-too-new`. The *strict pass* deserialises the envelope with the
//! payload type for the file's version, denying unknown fields everywhere.
//! A file at an older version is loaded as that version's frozen type and
//! migrated up the [`Schema`] chain, one typed step per version.
//!
//! # Compatibility policy
//!
//! Older files open in newer builds (through migrations); older builds
//! refuse newer files with `version-too-new` and never read one partially.
//! Unknown fields are always errors, missing fields are errors unless the
//! field has `#[serde(default)]`, and every schema change bumps the version,
//! additive ones included.
//!
//! # Rules for schema types
//!
//! Every type reachable from a payload follows these rules:
//!
//! * `#[derive(Serialize, Deserialize)]` and `#[serde(deny_unknown_fields)]`
//!   on every struct and enum; no hand-written impls.
//! * Forbidden serde attributes: `flatten`, `untagged`, `tag`, `content`
//!   (they buffer through serde's internal content type, where `ron` loses
//!   error positions and parses floats by guessing), and `skip`,
//!   `skip_serializing`, `skip_serializing_if` (they would hide fields from
//!   the content hash; the encoder rejects them). `#[serde(default)]` is
//!   allowed.
//! * Maps are [`UniqueMap`]; `HashMap` and `HashSet` are not allowed. Sets
//!   are written as lists.
//! * Floats are finite (`non-finite` otherwise); `-0.0` is kept as written.
//!   No `i128` or `u128`.
//!
//! As a backstop for the first rule the loader wraps the deserialiser in
//! `serde_ignored`; a field that was ignored anyway is a `schema-not-strict`
//! error with its path.
//!
//! # Content hash
//!
//! [`content_hash`] is BLAKE3 in key-derivation mode (context
//! `"st-data 2026-10-11 canonical content hash v1"`, 32-byte output), over
//! the canonical encoding of the triple (`kind`, current `version`, value
//! after migration). It hashes the value, not the file: whitespace,
//! comments, field order, `Some(x)` versus `x` and an omitted field versus
//! its default written out all give the same hash. The text form is `b3:`
//! and 64 lowercase hex digits. A hash is stable only within a schema
//! version: whoever stores one stores the version with it.
//!
//! ## Canonical encoding v1
//!
//! Integers are little-endian; *len* is a `u64`, little-endian.
//!
//! | serde data model | Encoding |
//! |---|---|
//! | `bool` | one byte, `00` or `01` |
//! | `i8` to `i64`, `u8` to `u64` | two's complement or unsigned, 1, 2, 4 or 8 bytes |
//! | `f32`, `f64` | IEEE 754 bits, 4 or 8 bytes; NaN and infinities are an error (`non-finite`); `-0.0` kept |
//! | `i128`, `u128` | error (`type`) |
//! | `char` | Unicode scalar value as `u32` |
//! | string | len in bytes, then UTF-8 bytes, no normalisation |
//! | bytes | len, then the bytes |
//! | `None` / `Some(v)` | `00` / `01` then `v` |
//! | unit, unit struct | nothing |
//! | newtype struct | the inner value |
//! | sequence | len, then each element in order |
//! | tuple, tuple struct | each element in order |
//! | map | len, then each entry (key, value) sorted by the bytes of the encoded key; equal encoded keys are an error (`duplicate-key`) |
//! | struct | field count, then each field as (name as string, value), sorted by the bytes of the name |
//! | enum variant | variant name as string, then the payload as for unit, newtype, tuple or struct |
//!
//! Struct fields and enum variants are identified by name, not by position,
//! so reordering them in Rust source does not change any hash. If the
//! encoding ever changes it becomes v2 with a new context string; hashes of
//! different encoding versions are never compared.
//!
//! # Data-set hash
//!
//! [`data_set_hash`] hashes a set of loaded files: BLAKE3 in
//! key-derivation mode (context `"st-data 2026-10-11 data set hash v1"`)
//! over the canonical encoding of the list of (`kind`, source name, content
//! hash), sorted by source name. Replays record this value to name the exact
//! data they ran on.
//!
//! The **source-name sort order** is plain byte-wise lexicographic order of
//! the UTF-8 source names, i.e. Rust's `Ord` for `str`: no locale, no case
//! folding, no normalisation, and `/` sorts as an ordinary byte. Two entries
//! with the same source name are an error. The content hash inside each
//! triple is encoded as a byte string (its length, then 32 raw bytes).
//!
//! # Diagnostics
//!
//! A [`Diagnostic`] has a `source` (the name the caller passed in, with `/`
//! separators and relative to an asset or data root, so logs never contain
//! absolute paths), an optional [`Span`] (1-based line and column, counted
//! in Unicode scalar values), a [`DataPath`] such as `sections[2].mass`, a
//! stable [`Code`], an optional validation `rule`, a message and optional
//! help. Nothing is printed by the library.
//!
//! Parse errors take their span from `ron` and their path from
//! `serde_path_to_error`. Errors found after parsing (`non-finite`,
//! `migration`, `validation`, `version-too-new`, ...) carry a path, and a
//! small RON span locator walks the text again to find the value. The
//! locator is best-effort: if it cannot resolve a path, the diagnostic keeps
//! the path without a span, and for a migrated file it says which version
//! the file is at. At most 64 diagnostics per load get a locator span.
//!
//! **Upgrading `ron`.** The span locator and the error mapping depend on
//! `ron`'s error variants, its position format (`Deserializer::span_error`,
//! 1-based line and column in characters) and the syntax it accepts. They
//! must be re-checked whenever `ron` is upgraded. The tests in
//! `tests/ron_contract.rs` pin that behaviour and fail if it changes.
//!
//! # Determinism
//!
//! This is a simulation crate (ADR 0008): it uses no `HashMap` or `HashSet`,
//! no `std::hash::Hash`-based decision, and no `usize` in anything hashed;
//! lengths are `u64`. Files are never enumerated here.
//!
//! # Not in this crate
//!
//! File access, directory watching and hot reload live elsewhere
//! (`st-platform` reads bytes and watches files). Schema types live in the
//! crates that own their meaning.
//!
//! # Example
//!
//! A kind with two versions: version 2 renamed `mass` to `mass_kg`.
//!
//! ```
//! use serde::{Deserialize, Serialize};
//! use st_data::{Genesis, Loader, MigrationError, Schema, Validate, Validator};
//!
//! #[derive(Serialize, Deserialize)]
//! #[serde(deny_unknown_fields)]
//! struct CrateV1 { mass: f32 }
//!
//! impl Schema for CrateV1 {
//!     const KIND: &'static str = "cargo_crate";
//!     const VERSION: u32 = 1;
//!     type Previous = Genesis;
//!     fn migrate(p: Genesis) -> Result<Self, MigrationError> { match p {} }
//! }
//!
//! #[derive(Serialize, Deserialize)]
//! #[serde(deny_unknown_fields)]
//! struct Crate { mass_kg: f32 }
//!
//! impl Schema for Crate {
//!     const KIND: &'static str = "cargo_crate";
//!     const VERSION: u32 = 2;
//!     type Previous = CrateV1;
//!     fn migrate(previous: CrateV1) -> Result<Self, MigrationError> {
//!         Ok(Crate { mass_kg: previous.mass })
//!     }
//! }
//!
//! impl Validate for Crate {
//!     fn validate(&self, v: &mut Validator) {
//!         if self.mass_kg < 0.0 {
//!             v.field("mass_kg", |v| v.error("cargo_crate.mass", "must not be negative"));
//!         }
//!     }
//! }
//!
//! let loader = Loader::new();
//! let old = loader
//!     .load::<Crate>("crates/a.ron", br#"(kind: "cargo_crate", version: 1, data: (mass: 2.5))"#)
//!     .unwrap();
//! let new = loader
//!     .load::<Crate>("crates/b.ron", br#"(kind: "cargo_crate", version: 2, data: (mass_kg: 2.5))"#)
//!     .unwrap();
//! assert_eq!(old.file_version, 1);
//! // An old file hashes as the value it migrates to.
//! assert_eq!(old.hash, new.hash);
//! ```

mod canon;
mod diagnostic;
mod hash;
mod load;
mod locator;
mod path;
mod schema;
mod unique_map;
mod validate;

pub use canon::{EncodeError, canonical_bytes};
pub use diagnostic::{Code, Diagnostic, Position, Span};
pub use hash::{
    ContentHash, DataSetEntry, DataSetHash, ParseHashError, content_hash, data_set_hash,
};
pub use load::{DEFAULT_MAX_BYTES, Header, Loaded, Loader, RECURSION_LIMIT};
pub use path::{DataPath, Segment};
pub use schema::{ChainError, Genesis, MigrationError, Schema, check_chain, is_valid_kind};
pub use unique_map::UniqueMap;
pub use validate::{Validate, Validator, Violation};
