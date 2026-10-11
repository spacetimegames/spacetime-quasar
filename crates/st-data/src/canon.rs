//! Canonical encoding v1: the byte string that content hashes are taken over.
//!
//! The encoding is a `serde::Serializer` with no further dependency. It
//! identifies struct fields and enum variants by name and sorts what has no
//! inherent order, so neither Rust field order, nor the order of map entries
//! in a file, nor the syntax of the file reaches the bytes. See the
//! crate-level documentation for the table of encodings.

use core::fmt;

use serde::ser::{
    self, Serialize, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant,
    SerializeTuple, SerializeTupleStruct, SerializeTupleVariant,
};

use crate::diagnostic::Code;
use crate::path::{DataPath, Segment};

/// Why a value cannot be canonically encoded.
///
/// The code is [`Code::NonFinite`] for NaN and infinities, [`Code::DuplicateKey`]
/// for two map entries whose encoded keys are equal, and [`Code::Type`] for
/// anything else the encoding does not allow (128-bit integers, skipped
/// struct fields, errors raised by a `Serialize` implementation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodeError {
    /// The diagnostic code for this failure.
    pub code: Code,
    /// What is wrong.
    pub message: String,
    /// Where in the value the problem is.
    pub path: DataPath,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}: {}", self.code, self.message)
        } else {
            write!(f, "{}: {}: {}", self.code, self.path, self.message)
        }
    }
}

impl std::error::Error for EncodeError {}

impl ser::Error for EncodeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self {
            code: Code::Type,
            message: msg.to_string(),
            path: DataPath::new(),
        }
    }
}

/// Encodes `value` with canonical encoding v1.
///
/// # Errors
///
/// Fails with an [`EncodeError`] on NaN or infinite floats, 128-bit integers,
/// skipped struct fields and duplicate map keys.
///
/// # Examples
///
/// ```
/// use st_data::canonical_bytes;
///
/// // Integers are little-endian; a string is its length as a `u64`, then UTF-8.
/// assert_eq!(canonical_bytes(&0x0102_u16).unwrap(), [2, 1]);
/// let s = canonical_bytes("ab").unwrap();
/// assert_eq!(s, [2, 0, 0, 0, 0, 0, 0, 0, b'a', b'b']);
///
/// // NaN and infinities have no canonical encoding.
/// assert!(canonical_bytes(&f32::NAN).is_err());
/// ```
pub fn canonical_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::new();
    encode_into(&mut out, value)?;
    Ok(out)
}

/// Appends the canonical encoding of `value` to `out`.
pub(crate) fn encode_into<T: Serialize + ?Sized>(
    out: &mut Vec<u8>,
    value: &T,
) -> Result<(), EncodeError> {
    let mut ctx = Ctx::default();
    value.serialize(Ser { out, ctx: &mut ctx })
}

/// Appends a length or count: a `u64`, little-endian.
pub(crate) fn put_len(out: &mut Vec<u8>, len: usize) {
    // `usize` is at most 64 bits on every supported target.
    out.extend_from_slice(&(len as u64).to_le_bytes());
}

/// Appends a string: its length in bytes, then the UTF-8 bytes.
pub(crate) fn put_str(out: &mut Vec<u8>, s: &str) {
    put_len(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

/// State shared by every serializer of one encoding run.
#[derive(Default)]
struct Ctx {
    /// The location being encoded, for error paths.
    path: Vec<Segment>,
    /// Set while a map key is encoded, so that scalar keys leave their text
    /// in `scalar` for the path of the entry's value.
    capture: bool,
    scalar: Option<String>,
}

impl Ctx {
    fn error(&self, code: Code, message: impl Into<String>) -> EncodeError {
        EncodeError {
            code,
            message: message.into(),
            path: DataPath::from_segments(self.path.clone()),
        }
    }

    /// Fills in the current path on an error that does not have one, such as
    /// a custom error raised by a `Serialize` implementation.
    fn locate(&self, mut e: EncodeError) -> EncodeError {
        if e.path.is_empty() {
            e.path = DataPath::from_segments(self.path.clone());
        }
        e
    }

    fn note(&mut self, text: impl FnOnce() -> String) {
        if self.capture {
            self.scalar = Some(text());
        }
    }
}

struct Ser<'a> {
    out: &'a mut Vec<u8>,
    ctx: &'a mut Ctx,
}

macro_rules! int_methods {
    ($($method:ident: $t:ty),* $(,)?) => {$(
        fn $method(self, v: $t) -> Result<(), EncodeError> {
            self.ctx.note(|| v.to_string());
            self.out.extend_from_slice(&v.to_le_bytes());
            Ok(())
        }
    )*};
}

impl<'a> ser::Serializer for Ser<'a> {
    type Ok = ();
    type Error = EncodeError;
    type SerializeSeq = Seq<'a>;
    type SerializeTuple = Seq<'a>;
    type SerializeTupleStruct = Seq<'a>;
    type SerializeTupleVariant = Seq<'a>;
    type SerializeMap = Map<'a>;
    type SerializeStruct = Fields<'a>;
    type SerializeStructVariant = Fields<'a>;

    // Hashes must not depend on how a type chooses to serialise itself for
    // humans versus machines; one form, the compact one.
    fn is_human_readable(&self) -> bool {
        false
    }

    fn serialize_bool(self, v: bool) -> Result<(), EncodeError> {
        self.ctx.note(|| v.to_string());
        self.out.push(u8::from(v));
        Ok(())
    }

    int_methods! {
        serialize_i8: i8, serialize_i16: i16, serialize_i32: i32, serialize_i64: i64,
        serialize_u8: u8, serialize_u16: u16, serialize_u32: u32, serialize_u64: u64,
    }

    fn serialize_i128(self, _: i128) -> Result<(), EncodeError> {
        Err(self.ctx.error(
            Code::Type,
            "128-bit integers are not allowed in schema types",
        ))
    }

    fn serialize_u128(self, _: u128) -> Result<(), EncodeError> {
        Err(self.ctx.error(
            Code::Type,
            "128-bit integers are not allowed in schema types",
        ))
    }

    fn serialize_f32(self, v: f32) -> Result<(), EncodeError> {
        if !v.is_finite() {
            return Err(non_finite(self.ctx, v.is_nan()));
        }
        // The IEEE 754 bits, so `-0.0` stays distinct from `0.0`.
        self.out.extend_from_slice(&v.to_bits().to_le_bytes());
        Ok(())
    }

    fn serialize_f64(self, v: f64) -> Result<(), EncodeError> {
        if !v.is_finite() {
            return Err(non_finite(self.ctx, v.is_nan()));
        }
        self.out.extend_from_slice(&v.to_bits().to_le_bytes());
        Ok(())
    }

    fn serialize_char(self, v: char) -> Result<(), EncodeError> {
        self.ctx.note(|| v.to_string());
        self.out.extend_from_slice(&u32::from(v).to_le_bytes());
        Ok(())
    }

    fn serialize_str(self, v: &str) -> Result<(), EncodeError> {
        self.ctx.note(|| v.to_owned());
        put_str(self.out, v);
        Ok(())
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<(), EncodeError> {
        put_len(self.out, v.len());
        self.out.extend_from_slice(v);
        Ok(())
    }

    fn serialize_none(self) -> Result<(), EncodeError> {
        self.out.push(0);
        Ok(())
    }

    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<(), EncodeError> {
        self.out.push(1);
        v.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), EncodeError> {
        Ok(())
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<(), EncodeError> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<(), EncodeError> {
        put_str(self.out, variant);
        Ok(())
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<(), EncodeError> {
        v.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        v: &T,
    ) -> Result<(), EncodeError> {
        put_str(self.out, variant);
        self.ctx.path.push(Segment::Variant(variant.to_owned()));
        v.serialize(Ser {
            out: self.out,
            ctx: self.ctx,
        })
        .map_err(|e| self.ctx.locate(e))?;
        self.ctx.path.pop();
        self.ctx.scalar = None;
        Ok(())
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Seq<'a>, EncodeError> {
        Ok(Seq::new(self, true, None))
    }

    fn serialize_tuple(self, _: usize) -> Result<Seq<'a>, EncodeError> {
        Ok(Seq::new(self, false, None))
    }

    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Seq<'a>, EncodeError> {
        Ok(Seq::new(self, false, None))
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Seq<'a>, EncodeError> {
        put_str(self.out, variant);
        Ok(Seq::new(self, false, Some(variant)))
    }

    fn serialize_map(self, _: Option<usize>) -> Result<Map<'a>, EncodeError> {
        Ok(Map {
            out: self.out,
            ctx: self.ctx,
            entries: Vec::new(),
            pending: None,
        })
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Fields<'a>, EncodeError> {
        Ok(Fields::new(self, None))
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Fields<'a>, EncodeError> {
        put_str(self.out, variant);
        Ok(Fields::new(self, Some(variant)))
    }
}

fn non_finite(ctx: &Ctx, nan: bool) -> EncodeError {
    let what = if nan { "NaN" } else { "an infinity" };
    ctx.error(
        Code::NonFinite,
        format!("{what} is not allowed; floats must be finite"),
    )
}

/// Sequences, tuples and tuple variants: elements in order.
struct Seq<'a> {
    out: &'a mut Vec<u8>,
    ctx: &'a mut Ctx,
    body: Vec<u8>,
    count: usize,
    /// Sequences carry their length; tuples do not (the type fixes it).
    with_len: bool,
    /// Whether a variant segment was pushed on the path.
    variant: bool,
}

impl<'a> Seq<'a> {
    fn new(ser: Ser<'a>, with_len: bool, variant: Option<&'static str>) -> Self {
        if let Some(name) = variant {
            ser.ctx.path.push(Segment::Variant(name.to_owned()));
        }
        Self {
            out: ser.out,
            ctx: ser.ctx,
            body: Vec::new(),
            count: 0,
            with_len,
            variant: variant.is_some(),
        }
    }

    fn element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), EncodeError> {
        self.ctx.path.push(Segment::Index(self.count));
        v.serialize(Ser {
            out: &mut self.body,
            ctx: self.ctx,
        })
        .map_err(|e| self.ctx.locate(e))?;
        self.ctx.path.pop();
        self.count += 1;
        Ok(())
    }

    fn finish(self) {
        if self.with_len {
            put_len(self.out, self.count);
        }
        self.out.extend_from_slice(&self.body);
        if self.variant {
            self.ctx.path.pop();
        }
        self.ctx.scalar = None;
    }
}

impl SerializeSeq for Seq<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), EncodeError> {
        self.element(v)
    }

    fn end(self) -> Result<(), EncodeError> {
        self.finish();
        Ok(())
    }
}

impl SerializeTuple for Seq<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), EncodeError> {
        self.element(v)
    }

    fn end(self) -> Result<(), EncodeError> {
        self.finish();
        Ok(())
    }
}

impl SerializeTupleStruct for Seq<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), EncodeError> {
        self.element(v)
    }

    fn end(self) -> Result<(), EncodeError> {
        self.finish();
        Ok(())
    }
}

impl SerializeTupleVariant for Seq<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), EncodeError> {
        self.element(v)
    }

    fn end(self) -> Result<(), EncodeError> {
        self.finish();
        Ok(())
    }
}

/// Structs and struct variants: fields identified by name, sorted by the
/// bytes of the name.
struct Fields<'a> {
    out: &'a mut Vec<u8>,
    ctx: &'a mut Ctx,
    fields: Vec<(&'static str, Vec<u8>)>,
    variant: bool,
}

impl<'a> Fields<'a> {
    fn new(ser: Ser<'a>, variant: Option<&'static str>) -> Self {
        if let Some(name) = variant {
            ser.ctx.path.push(Segment::Variant(name.to_owned()));
        }
        Self {
            out: ser.out,
            ctx: ser.ctx,
            fields: Vec::new(),
            variant: variant.is_some(),
        }
    }

    fn field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        v: &T,
    ) -> Result<(), EncodeError> {
        self.ctx.path.push(Segment::Key(name.to_owned()));
        let mut bytes = Vec::new();
        v.serialize(Ser {
            out: &mut bytes,
            ctx: self.ctx,
        })
        .map_err(|e| self.ctx.locate(e))?;
        self.ctx.path.pop();
        self.fields.push((name, bytes));
        Ok(())
    }

    fn skipped(&self, name: &'static str) -> EncodeError {
        // A skipped field would be invisible to the hash.
        self.ctx.error(
            Code::Type,
            format!(
                "field `{name}` is skipped by serde; skipped fields are not allowed in schema types"
            ),
        )
    }

    fn finish(mut self) {
        // Sorting by the name's own bytes (not its length-prefixed encoding)
        // is what the encoding specifies; equal names cannot occur in a
        // derived struct, and a stable sort keeps the result deterministic
        // if a hand-written impl repeats one.
        self.fields
            .sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        put_len(self.out, self.fields.len());
        for (name, bytes) in &self.fields {
            put_str(self.out, name);
            self.out.extend_from_slice(bytes);
        }
        if self.variant {
            self.ctx.path.pop();
        }
        self.ctx.scalar = None;
    }
}

impl SerializeStruct for Fields<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        v: &T,
    ) -> Result<(), EncodeError> {
        self.field(name, v)
    }

    fn skip_field(&mut self, name: &'static str) -> Result<(), EncodeError> {
        Err(self.skipped(name))
    }

    fn end(self) -> Result<(), EncodeError> {
        self.finish();
        Ok(())
    }
}

impl SerializeStructVariant for Fields<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        v: &T,
    ) -> Result<(), EncodeError> {
        self.field(name, v)
    }

    fn skip_field(&mut self, name: &'static str) -> Result<(), EncodeError> {
        Err(self.skipped(name))
    }

    fn end(self) -> Result<(), EncodeError> {
        self.finish();
        Ok(())
    }
}

/// Maps: entries sorted by the bytes of the encoded key.
struct Map<'a> {
    out: &'a mut Vec<u8>,
    ctx: &'a mut Ctx,
    /// (encoded key, key text for paths, encoded value)
    entries: Vec<(Vec<u8>, String, Vec<u8>)>,
    pending: Option<(Vec<u8>, String)>,
}

impl SerializeMap for Map<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), EncodeError> {
        let mut bytes = Vec::new();
        self.ctx.capture = true;
        self.ctx.scalar = None;
        let result = key.serialize(Ser {
            out: &mut bytes,
            ctx: self.ctx,
        });
        self.ctx.capture = false;
        result.map_err(|e| self.ctx.locate(e))?;
        // A key that is not a bare scalar has no text; "?" never matches a
        // span, which is the safe outcome.
        let text = self.ctx.scalar.take().unwrap_or_else(|| "?".to_owned());
        self.pending = Some((bytes, text));
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), EncodeError> {
        let Some((key_bytes, text)) = self.pending.take() else {
            return Err(self
                .ctx
                .error(Code::Type, "map value serialised without a key"));
        };
        self.ctx.path.push(Segment::Key(text.clone()));
        let mut bytes = Vec::new();
        value
            .serialize(Ser {
                out: &mut bytes,
                ctx: self.ctx,
            })
            .map_err(|e| self.ctx.locate(e))?;
        self.ctx.path.pop();
        self.entries.push((key_bytes, text, bytes));
        Ok(())
    }

    fn end(mut self) -> Result<(), EncodeError> {
        self.entries.sort_by(|a, b| a.0.cmp(&b.0));
        for pair in self.entries.windows(2) {
            if let [a, b] = pair
                && a.0 == b.0
            {
                self.ctx.path.push(Segment::Key(b.1.clone()));
                let e = self
                    .ctx
                    .error(Code::DuplicateKey, "two map entries have the same key");
                return Err(e);
            }
        }
        put_len(self.out, self.entries.len());
        for (key, _, value) in &self.entries {
            self.out.extend_from_slice(key);
            self.out.extend_from_slice(value);
        }
        self.ctx.scalar = None;
        Ok(())
    }
}
