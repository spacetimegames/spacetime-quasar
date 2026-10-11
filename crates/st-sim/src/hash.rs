//! The canonical state hash (ADR 0008, "State hash", format version 1).
//!
//! FNV-1a 64 over a canonical byte stream. It is not `std::hash::Hasher` and
//! `#[derive(Hash)]` is never used for it: the byte stream is written out
//! explicitly so that it is identical on every platform.

use st_math::glam::{DQuat, DVec3};

/// FNV-1a 64 offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Version of the canonical encoding. It changes only when the encoding rules
/// change; new state changes the hashes, not the version.
pub const HASH_FORMAT_VERSION: u32 = 1;

/// A state could not be hashed.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashError {
    /// A NaN was found in the state. A NaN is an error, not a value, because
    /// its payload is not portable (wasm leaves it non-deterministic).
    NotANumber,
}

impl core::fmt::Display for HashError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HashError::NotANumber => f.write_str("a NaN was found in the simulation state"),
        }
    }
}

impl core::error::Error for HashError {}

/// Writes the canonical byte stream and folds it into an FNV-1a 64 hash.
///
/// Integers are little-endian at fixed width. An `f64` is written as its bit
/// pattern after mapping `-0.0` to `+0.0`; a NaN is a [`HashError`]. A
/// sequence is a `u64` count followed by its elements.
///
/// ```
/// use st_sim::StateHasher;
///
/// let mut h = StateHasher::new();
/// h.bytes(b"foobar");
/// assert_eq!(h.finish(), 0x85944171f73967e8);
/// ```
#[derive(Clone, Debug)]
pub struct StateHasher {
    state: u64,
}

impl Default for StateHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl StateHasher {
    /// Starts an empty stream (the FNV-1a offset basis).
    pub const fn new() -> Self {
        Self { state: FNV_OFFSET }
    }

    /// The hash of everything written so far. The hasher stays usable.
    pub const fn finish(&self) -> u64 {
        self.state
    }

    /// Writes raw bytes.
    pub fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state ^= u64::from(b);
            self.state = self.state.wrapping_mul(FNV_PRIME);
        }
    }

    /// Writes a 4-byte ASCII section tag.
    pub fn tag(&mut self, tag: &[u8; 4]) {
        self.bytes(tag);
    }

    /// Writes one byte.
    pub fn u8(&mut self, v: u8) {
        self.bytes(&[v]);
    }

    /// Writes a `bool` as one byte, 0 or 1.
    pub fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }

    /// Writes a `u32`, little-endian.
    pub fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }

    /// Writes an `i16`, little-endian.
    pub fn i16(&mut self, v: i16) {
        self.bytes(&v.to_le_bytes());
    }

    /// Writes a `u64`, little-endian.
    pub fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }

    /// Writes an `i64`, little-endian.
    pub fn i64(&mut self, v: i64) {
        self.bytes(&v.to_le_bytes());
    }

    /// Writes a sequence length as a `u64`.
    pub fn count(&mut self, n: usize) {
        // A `usize` is at most 64 bits on every supported target.
        self.u64(n as u64);
    }

    /// Writes an `Option` tag byte (0 for `None`, 1 for `Some`). The caller
    /// then writes the value if there is one.
    pub fn option_tag(&mut self, is_some: bool) {
        self.bool(is_some);
    }

    /// Writes an `f64` as its bit pattern, little-endian, with `-0.0` mapped
    /// to `+0.0`. Infinities are written as they are.
    ///
    /// # Errors
    ///
    /// [`HashError::NotANumber`] if `v` is a NaN. Nothing is written then.
    pub fn f64(&mut self, v: f64) -> Result<(), HashError> {
        if v.is_nan() {
            return Err(HashError::NotANumber);
        }
        let v = if v == 0.0 { 0.0 } else { v };
        self.u64(v.to_bits());
        Ok(())
    }

    /// Writes a `DVec3` as x, y, z.
    ///
    /// # Errors
    ///
    /// [`HashError::NotANumber`] if a component is a NaN.
    pub fn vec3(&mut self, v: DVec3) -> Result<(), HashError> {
        self.f64(v.x)?;
        self.f64(v.y)?;
        self.f64(v.z)
    }

    /// Writes a `DQuat` as x, y, z, w (glam's storage order, no sign
    /// normalisation).
    ///
    /// # Errors
    ///
    /// [`HashError::NotANumber`] if a component is a NaN.
    pub fn quat(&mut self, q: DQuat) -> Result<(), HashError> {
        self.f64(q.x)?;
        self.f64(q.y)?;
        self.f64(q.z)?;
        self.f64(q.w)
    }
}

/// A value that contributes to the state hash.
pub trait StateHash {
    /// Writes this value's canonical encoding.
    ///
    /// # Errors
    ///
    /// [`HashError::NotANumber`] if the value contains a NaN.
    fn state_hash(&self, h: &mut StateHasher) -> Result<(), HashError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv(bytes: &[u8]) -> u64 {
        let mut h = StateHasher::new();
        h.bytes(bytes);
        h.finish()
    }

    #[test]
    fn adr_0008_byte_vectors() {
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn adr_0008_f64_sequence_vector() {
        // (1.0, -0.0, 2.5) written back to back, without a count prefix.
        let mut h = StateHasher::new();
        for v in [1.0, -0.0, 2.5] {
            h.f64(v).unwrap();
        }
        assert_eq!(h.finish(), 0xbd93_cddf_232e_c51c);
    }

    #[test]
    fn negative_zero_hashes_as_zero() {
        let mut a = StateHasher::new();
        let mut b = StateHasher::new();
        a.f64(-0.0).unwrap();
        b.f64(0.0).unwrap();
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn nan_is_an_error_and_writes_nothing() {
        let mut h = StateHasher::new();
        assert_eq!(h.f64(f64::NAN), Err(HashError::NotANumber));
        assert_eq!(
            h.vec3(DVec3::new(0.0, f64::NAN, 0.0)),
            Err(HashError::NotANumber)
        );
        // The x component was written before the NaN was met; the caller
        // discards the hasher on error.
        assert_eq!(StateHasher::new().finish(), 0xcbf2_9ce4_8422_2325);
        assert_eq!(h.f64(f64::INFINITY), Ok(()));
    }
}
