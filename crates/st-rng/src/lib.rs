//! Seedable, stateless, reproducible random numbers.
//!
//! `st-rng` is a *counter-based* generator. A random value is a pure function
//! of three things: a stream, a counter, and nothing else. There is no
//! generator state to advance, no global, and no thread-local, so any draw can
//! be reproduced directly without replaying the draws before it, and adding or
//! removing one consumer of randomness never shifts the values seen by
//! another.
//!
//! # Model
//!
//! * One `u64` seed makes the root [`Stream`] ([`Stream::from_seed`]).
//! * A stream derives *child* streams explicitly, one per decision:
//!   [`Stream::derive`] takes an integer (a station id, a tick) and
//!   [`Stream::derive_label`] takes a short text label (`"economy"`).
//! * A stream yields values by counter: [`Stream::u64_at`],
//!   [`Stream::below_at`], [`Stream::range_at`], [`Stream::f64_at`] and
//!   [`Stream::bool_at`].
//! * [`Cursor`] is an optional convenience that holds a stream and a counter
//!   and hands out successive values. It is plain `Copy` data; it never hides
//!   how a value was derived.
//!
//! ```
//! use st_rng::Stream;
//!
//! // seed -> "economy" -> station 7 -> tick 1200
//! let root = Stream::from_seed(0xC0FFEE);
//! let decision = root
//!     .derive_label("economy")
//!     .derive(7)
//!     .derive(1200);
//!
//! // The same path always gives the same values, in any order.
//! let price_roll = decision.f64_at(0);
//! let again = Stream::from_seed(0xC0FFEE)
//!     .derive_label("economy")
//!     .derive(7)
//!     .derive(1200)
//!     .f64_at(0);
//! assert_eq!(price_roll.to_bits(), again.to_bits());
//! assert!((0.0..1.0).contains(&price_roll));
//! ```
//!
//! # Algorithms (frozen)
//!
//! The algorithms below are part of the contract. Changing any of them, any
//! constant, a label's spelling, or the *order* in which streams are derived
//! changes every outcome that depends on it. Once replays or saves exist this
//! is a simulation-version change (see ADR 0008).
//!
//! * **Mixing:** the SplitMix64 output finaliser, which is Stafford's "Mix13"
//!   variant of the MurmurHash3 64-bit finaliser (David Stafford, "Better Bit
//!   Mixing", 2011) as used in Vigna's reference `splitmix64.c`
//!   (<https://prng.di.unimi.it/splitmix64.c>). The generator it finalises
//!   is from Steele, Lea and Flood, "Fast splittable pseudorandom number
//!   generators", OOPSLA 2014.
//!   Draw `n` of a stream with key `k` is `mix(k + (n + 1) * GAMMA)`, which is
//!   exactly the `n`-th output of SplitMix64 seeded with `k`.
//! * **Label hashing:** FNV-1a 64 over the label's UTF-8 bytes (offset basis
//!   `0xcbf29ce484222325`, prime `0x100000001b3`), the same fixed algorithm
//!   ADR 0008 uses for state hashing. See [`label_hash`].
//! * **Child derivation:** `child = mix(mix(parent ^ DOMAIN) + mix(word + GAMMA))`
//!   with a different `DOMAIN` constant for integers and labels, so
//!   `derive(x)` and `derive_label(l)` never collide just because
//!   `x == label_hash(l)`. Derivation is not commutative:
//!   `s.derive(1).derive(2)` differs from `s.derive(2).derive(1)`.
//! * **Bounded integers:** Lemire's multiply-shift method with rejection
//!   ("Fast Random Integer Generation in an Interval", ACM TOMACS 2019). The
//!   result is exactly uniform; there is no modulo bias. Because the crate is
//!   stateless, a rejected candidate is replaced by the next candidate of a
//!   dedicated sub-stream of the same counter, so the result is still a pure
//!   function of `(stream, counter, bound)`.
//! * **Floats:** `(x >> 11) as f64 * 2^-53`. The top 53 bits fit the `f64`
//!   mantissa exactly and the multiplication by a power of two is exact, so
//!   results are uniform on the multiples of 2^-53 in `[0, 1)` and identical
//!   on every platform. No platform float function is used anywhere.
//!
//! # Quality: why this is good enough for gameplay
//!
//! The SplitMix64 finaliser is a bijective avalanche function: flipping any
//! input bit flips about half of the output bits. Counter-based generators
//! built on such mixers pass the usual statistical batteries (SplitMix64
//! passes BigCrush), and consecutive counters or sibling streams are
//! uncorrelated for any practical purpose: loot rolls, spawn jitter, event
//! timing. It is **not** cryptographically secure: it is invertible and the
//! key can be recovered from outputs. Never use it for secrets, tokens or
//! anything an adversary must not predict.
//!
//! # Platform behaviour
//!
//! The core is integer-only (wrapping `u64` arithmetic, one `u128` multiply
//! for bounded draws) plus the exact float conversion above, so output does
//! not depend on pointer width or endianness. The golden vectors run in CI on
//! Linux, macOS and Windows; execution on `wasm32` will come with the
//! determinism harness (wasmtime on `wasm32-wasip1`, ADR 0008), and until then
//! the `wasm32` job only builds the crate. The crate is `#![no_std]`, contains no `unsafe`, no allocation,
//! and no panics on any input.

#![no_std]

/// Odd 64-bit "golden gamma" increment of SplitMix64: `floor(2^64 / phi)`
/// rounded to odd. It spreads consecutive counters across the mixer input.
const GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;

/// Domain-separation constant mixed into the root key so that seed 0 does not
/// map to key 0 (the mix function maps 0 to 0).
const DOMAIN_ROOT: u64 = 0x5354_5f52_4e47_5f31; // "ST_RNG_1"

/// Domain tag for integer-keyed derivation.
const DOMAIN_INT: u64 = 0x696e_7465_6765_7231; // "integer1"

/// Domain tag for label-keyed derivation.
const DOMAIN_LABEL: u64 = 0x6c61_6265_6c5f_3031; // "label_01"

/// Domain tag for the rejection-retry sub-stream of bounded draws.
const DOMAIN_RETRY: u64 = 0x7265_7472_795f_3031; // "retry_01"

/// FNV-1a 64 offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64 prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Maximum extra candidates tried by a bounded draw before falling back to
/// the unrejected multiply-shift result. Each candidate is rejected with
/// probability below one half, so reaching the cap has probability under
/// 2^-64 and the fallback's bias is unmeasurable. The cap only exists to keep
/// the function total and loop-bounded.
const MAX_RETRIES: u64 = 64;

/// The SplitMix64 output finaliser (a 64-bit bijective avalanche mixer).
///
/// Why these steps: xor-shifts spread high bits downward, and the two odd
/// multipliers spread low bits upward, so after three rounds every input bit
/// influences every output bit with close to 50% probability. The constants
/// are the published ones; they were chosen by search for avalanche quality,
/// so do not "tidy" them. A wrapping multiply is exact on every target, which
/// is why the result is platform-independent.
#[inline]
const fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Hashes a text label to a `u64` with FNV-1a 64 over its UTF-8 bytes.
///
/// This is the function [`Stream::derive_label`] uses. It is fixed forever:
/// the empty string hashes to `0xcbf29ce484222325`, `"a"` to
/// `0xaf63dc4c8601ec8c` and `"foobar"` to `0x85944171f73967e8`.
///
/// ```
/// assert_eq!(st_rng::label_hash(""), 0xcbf2_9ce4_8422_2325);
/// assert_eq!(st_rng::label_hash("a"), 0xaf63_dc4c_8601_ec8c);
/// assert_eq!(st_rng::label_hash("foobar"), 0x8594_4171_f739_67e8);
/// ```
#[must_use]
pub const fn label_hash(label: &str) -> u64 {
    let bytes = label.as_bytes();
    let mut hash = FNV_OFFSET;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    hash
}

/// An independent, stateless stream of random values.
///
/// A `Stream` is just a 64-bit key. Values are read by counter, so reading
/// does not change the stream and takes `&self`. Child streams are derived
/// explicitly with [`derive`](Self::derive) and
/// [`derive_label`](Self::derive_label).
///
/// ```
/// use st_rng::Stream;
///
/// let world = Stream::from_seed(42);
/// let loot = world.derive_label("loot");
/// // Counters can be read in any order and any number of times.
/// assert_eq!(loot.u64_at(5), loot.u64_at(5));
/// assert_ne!(loot.u64_at(5), loot.u64_at(6));
/// // Different decisions are independent streams.
/// assert_ne!(loot.u64_at(0), world.derive_label("spawn").u64_at(0));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Stream {
    key: u64,
}

impl Stream {
    /// Creates the root stream for a world seed.
    ///
    /// Every `u64` is a valid seed, including 0. The same seed always gives
    /// the same stream.
    ///
    /// ```
    /// use st_rng::Stream;
    /// assert_eq!(Stream::from_seed(0), Stream::from_seed(0));
    /// assert_ne!(Stream::from_seed(0), Stream::from_seed(1));
    /// ```
    #[must_use]
    pub const fn from_seed(seed: u64) -> Self {
        Self {
            key: mix(seed ^ DOMAIN_ROOT),
        }
    }

    /// Derives a child stream keyed by an integer, such as an entity id or a
    /// tick number.
    ///
    /// The order of derivation matters: `s.derive(1).derive(2)` and
    /// `s.derive(2).derive(1)` are different streams.
    ///
    /// ```
    /// use st_rng::Stream;
    /// let s = Stream::from_seed(1);
    /// assert_ne!(s.derive(7), s.derive(8));
    /// assert_ne!(s.derive(1).derive(2), s.derive(2).derive(1));
    /// ```
    #[must_use]
    pub const fn derive(&self, word: u64) -> Self {
        self.child(DOMAIN_INT, word)
    }

    /// Derives a child stream keyed by a short text label, such as
    /// `"economy"`.
    ///
    /// The label is hashed with [`label_hash`] (FNV-1a 64). Renaming a label
    /// changes every outcome drawn from it, so treat labels as part of the
    /// frozen format. Two different labels collide only if their FNV-1a
    /// hashes are equal, which is possible in principle for a 64-bit hash;
    /// keep the set of labels small and distinct.
    ///
    /// ```
    /// use st_rng::Stream;
    /// let s = Stream::from_seed(1);
    /// assert_ne!(s.derive_label("economy"), s.derive_label("combat"));
    /// // A label never equals the integer derivation of its hash.
    /// assert_ne!(
    ///     s.derive_label("economy"),
    ///     s.derive(st_rng::label_hash("economy")),
    /// );
    /// ```
    #[must_use]
    pub const fn derive_label(&self, label: &str) -> Self {
        self.child(DOMAIN_LABEL, label_hash(label))
    }

    /// Shared child-key derivation. Mixing the parent first and the word
    /// second makes derivation order-sensitive; the domain tag separates
    /// integer words from label hashes.
    const fn child(&self, domain: u64, word: u64) -> Self {
        Self {
            key: mix(mix(self.key ^ domain).wrapping_add(mix(word.wrapping_add(GAMMA)))),
        }
    }

    /// Returns the raw 64-bit value at `counter`.
    ///
    /// This is the `counter`-th output of SplitMix64 seeded with the stream
    /// key. Every `u64` counter is valid.
    ///
    /// ```
    /// use st_rng::Stream;
    /// let s = Stream::from_seed(9);
    /// assert_eq!(s.u64_at(u64::MAX), s.u64_at(u64::MAX));
    /// ```
    #[must_use]
    pub const fn u64_at(&self, counter: u64) -> u64 {
        mix(self
            .key
            .wrapping_add(counter.wrapping_add(1).wrapping_mul(GAMMA)))
    }

    /// Returns a uniformly distributed integer in `0..bound` at `counter`,
    /// with no modulo bias.
    ///
    /// Uses Lemire's multiply-shift method with rejection (see the crate
    /// docs). A `bound` of 0 has no valid result and returns 0; this never
    /// panics.
    ///
    /// ```
    /// use st_rng::Stream;
    /// let s = Stream::from_seed(3);
    /// for counter in 0..100 {
    ///     assert!(s.below_at(counter, 6) < 6);
    /// }
    /// assert_eq!(s.below_at(0, 1), 0);
    /// assert_eq!(s.below_at(0, 0), 0);
    /// ```
    #[must_use]
    pub const fn below_at(&self, counter: u64, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        // Threshold below which a low half is rejected: 2^64 mod bound.
        let threshold = bound.wrapping_neg() % bound;
        let mut candidate = self.u64_at(counter);
        let mut product = (candidate as u128) * (bound as u128);
        if (product as u64) >= threshold {
            return (product >> 64) as u64;
        }
        // Rejected: take further candidates from a retry sub-stream of this
        // counter, keeping the result a pure function of the inputs.
        let retry = self.child(DOMAIN_RETRY, counter);
        let mut attempt = 0;
        while attempt < MAX_RETRIES {
            candidate = retry.u64_at(attempt);
            product = (candidate as u128) * (bound as u128);
            if (product as u64) >= threshold {
                break;
            }
            attempt += 1;
        }
        (product >> 64) as u64
    }

    /// Returns a uniformly distributed integer in `low..high` at `counter`.
    ///
    /// If the range is empty (`low >= high`) the result is `low`; this never
    /// panics or overflows.
    ///
    /// ```
    /// use st_rng::Stream;
    /// let s = Stream::from_seed(3);
    /// let v = s.range_at(0, 10, 20);
    /// assert!((10..20).contains(&v));
    /// assert_eq!(s.range_at(0, 5, 5), 5);
    /// assert_eq!(s.range_at(0, 9, 2), 9);
    /// ```
    #[must_use]
    pub const fn range_at(&self, counter: u64, low: u64, high: u64) -> u64 {
        if low >= high {
            return low;
        }
        low + self.below_at(counter, high - low)
    }

    /// Returns an `f64` in `[0, 1)` at `counter`, built from integer bits.
    ///
    /// The result is `(x >> 11) as f64 * 2^-53`: 53 random bits, an exact
    /// conversion and an exact power-of-two scale, so it is bit-identical on
    /// every platform. It can be 0.0 but never 1.0.
    ///
    /// ```
    /// use st_rng::Stream;
    /// let s = Stream::from_seed(3);
    /// let x = s.f64_at(0);
    /// assert!((0.0..1.0).contains(&x));
    /// ```
    #[must_use]
    pub const fn f64_at(&self, counter: u64) -> f64 {
        (self.u64_at(counter) >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }

    /// Returns a fair coin flip at `counter` (the top bit of the raw value).
    ///
    /// ```
    /// use st_rng::Stream;
    /// let s = Stream::from_seed(3);
    /// assert_eq!(s.bool_at(0), s.bool_at(0));
    /// ```
    #[must_use]
    pub const fn bool_at(&self, counter: u64) -> bool {
        self.u64_at(counter) >> 63 == 1
    }

    /// Returns a [`Cursor`] over this stream, starting at counter 0.
    ///
    /// ```
    /// use st_rng::Stream;
    /// let s = Stream::from_seed(3);
    /// let mut cursor = s.cursor();
    /// assert_eq!(cursor.next_u64(), s.u64_at(0));
    /// assert_eq!(cursor.next_u64(), s.u64_at(1));
    /// ```
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        Cursor {
            stream: *self,
            counter: 0,
        }
    }
}

/// A convenience that holds a [`Stream`] and a counter and returns successive
/// values.
///
/// A cursor is plain `Copy` data with no hidden state: its `n`-th call
/// returns exactly `stream.u64_at(n)` (or the matching bounded or float
/// form). It exists only to save writing `counter += 1` by hand. Each draw
/// advances the counter by exactly one, including bounded draws that needed
/// rejection, so the position after `n` calls is always `n`. The counter
/// wraps after 2^64 draws.
///
/// ```
/// use st_rng::Stream;
///
/// let stream = Stream::from_seed(7).derive_label("spawn").derive(3);
/// let mut cursor = stream.cursor();
/// let a = cursor.next_below(100);
/// let b = cursor.next_f64();
/// assert_eq!(a, stream.below_at(0, 100));
/// assert_eq!(b.to_bits(), stream.f64_at(1).to_bits());
/// assert_eq!(cursor.counter(), 2);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cursor {
    stream: Stream,
    counter: u64,
}

impl Cursor {
    /// Creates a cursor over `stream` positioned at `counter`.
    ///
    /// ```
    /// use st_rng::{Cursor, Stream};
    /// let s = Stream::from_seed(1);
    /// let mut c = Cursor::at(s, 10);
    /// assert_eq!(c.next_u64(), s.u64_at(10));
    /// ```
    #[must_use]
    pub const fn at(stream: Stream, counter: u64) -> Self {
        Self { stream, counter }
    }

    /// Returns the stream this cursor reads.
    ///
    /// ```
    /// use st_rng::Stream;
    /// let s = Stream::from_seed(1);
    /// assert_eq!(s.cursor().stream(), s);
    /// ```
    #[must_use]
    pub const fn stream(&self) -> Stream {
        self.stream
    }

    /// Returns the counter the next draw will use.
    ///
    /// ```
    /// use st_rng::Stream;
    /// let mut c = Stream::from_seed(1).cursor();
    /// assert_eq!(c.counter(), 0);
    /// let _ = c.next_u64();
    /// assert_eq!(c.counter(), 1);
    /// ```
    #[must_use]
    pub const fn counter(&self) -> u64 {
        self.counter
    }

    /// Takes the current counter and advances by one.
    const fn take(&mut self) -> u64 {
        let counter = self.counter;
        self.counter = counter.wrapping_add(1);
        counter
    }

    /// Returns the next raw `u64`; see [`Stream::u64_at`].
    ///
    /// ```
    /// use st_rng::Stream;
    /// let s = Stream::from_seed(1);
    /// let mut c = s.cursor();
    /// assert_eq!(c.next_u64(), s.u64_at(0));
    /// ```
    pub const fn next_u64(&mut self) -> u64 {
        let counter = self.take();
        self.stream.u64_at(counter)
    }

    /// Returns the next integer in `0..bound`; see [`Stream::below_at`].
    /// A `bound` of 0 returns 0 (and still advances the counter).
    ///
    /// ```
    /// use st_rng::Stream;
    /// let mut c = Stream::from_seed(1).cursor();
    /// assert!(c.next_below(10) < 10);
    /// assert_eq!(c.next_below(0), 0);
    /// ```
    pub const fn next_below(&mut self, bound: u64) -> u64 {
        let counter = self.take();
        self.stream.below_at(counter, bound)
    }

    /// Returns the next integer in `low..high`; see [`Stream::range_at`].
    ///
    /// ```
    /// use st_rng::Stream;
    /// let mut c = Stream::from_seed(1).cursor();
    /// assert!((5..8).contains(&c.next_range(5, 8)));
    /// ```
    pub const fn next_range(&mut self, low: u64, high: u64) -> u64 {
        let counter = self.take();
        self.stream.range_at(counter, low, high)
    }

    /// Returns the next `f64` in `[0, 1)`; see [`Stream::f64_at`].
    ///
    /// ```
    /// use st_rng::Stream;
    /// let mut c = Stream::from_seed(1).cursor();
    /// assert!((0.0..1.0).contains(&c.next_f64()));
    /// ```
    pub const fn next_f64(&mut self) -> f64 {
        let counter = self.take();
        self.stream.f64_at(counter)
    }

    /// Returns the next coin flip; see [`Stream::bool_at`].
    ///
    /// ```
    /// use st_rng::Stream;
    /// let mut c = Stream::from_seed(1).cursor();
    /// let _flip: bool = c.next_bool();
    /// ```
    pub const fn next_bool(&mut self) -> bool {
        let counter = self.take();
        self.stream.bool_at(counter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference SplitMix64 outputs for state seed 0
    /// (from the published reference implementation).
    #[test]
    fn mix_matches_splitmix64_reference() {
        let expected = [
            0xe220_a839_7b1d_cdaf_u64,
            0x6e78_9e6a_a1b9_65f4,
            0x06c4_5d18_8009_454f,
            0xf88b_b8a8_724c_81ec,
        ];
        let raw = Stream { key: 0 };
        for (n, want) in expected.iter().enumerate() {
            assert_eq!(raw.u64_at(n as u64), *want);
        }
    }

    #[test]
    fn fnv_reference_vectors() {
        assert_eq!(label_hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(label_hash("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(label_hash("foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn seed_zero_is_not_degenerate() {
        assert_ne!(Stream::from_seed(0).key, 0);
    }

    #[test]
    fn bounded_is_in_range_and_covers_it() {
        let s = Stream::from_seed(11);
        let mut seen = [false; 7];
        for c in 0..1000 {
            let v = s.below_at(c, 7) as usize;
            assert!(v < 7);
            seen[v] = true;
        }
        assert!(seen.iter().all(|&b| b));
    }

    #[test]
    fn bounded_extreme_bounds_do_not_panic() {
        let s = Stream::from_seed(5);
        for c in 0..200 {
            let _ = s.below_at(c, u64::MAX);
            assert!(s.below_at(c, (1 << 63) + 1) < (1 << 63) + 1);
            assert_eq!(s.below_at(c, 1), 0);
        }
        assert!(s.range_at(0, 0, u64::MAX) < u64::MAX);
        assert_eq!(s.range_at(0, u64::MAX, 0), u64::MAX);
    }

    #[test]
    fn rejection_path_is_taken_and_uses_retry_stream() {
        // With a bound just above 2^63 about half of first candidates are
        // rejected. For each rejected counter the result must come from the
        // retry sub-stream, not from the rejected first candidate.
        let s = Stream::from_seed(21);
        let bound = (1u64 << 63) + 12345;
        let threshold = bound.wrapping_neg() % bound;
        let mut retried = 0;
        for c in 0..256 {
            let first = (s.u64_at(c) as u128) * (bound as u128);
            let got = s.below_at(c, bound);
            assert!(got < bound);
            if (first as u64) >= threshold {
                assert_eq!(got, (first >> 64) as u64, "accepted at {c}");
                continue;
            }
            retried += 1;
            let retry = s.child(DOMAIN_RETRY, c);
            let accepted = (0..MAX_RETRIES)
                .map(|a| (retry.u64_at(a) as u128) * (bound as u128))
                .find(|p| (*p as u64) >= threshold)
                .expect("a candidate is accepted within the cap");
            assert_eq!(got, (accepted >> 64) as u64, "retried at {c}");
        }
        // Roughly half of 256 draws are rejected.
        assert!((64..=192).contains(&retried), "retried {retried}");
    }

    #[test]
    fn float_is_exact_unit_interval() {
        let s = Stream::from_seed(2);
        for c in 0..1000 {
            let x = s.f64_at(c);
            assert!((0.0..1.0).contains(&x));
            // A multiple of 2^-53.
            assert_eq!((x * 9_007_199_254_740_992.0).fract(), 0.0);
        }
    }

    #[test]
    fn cursor_matches_direct_reads() {
        let s = Stream::from_seed(8).derive(1);
        let mut c = s.cursor();
        for n in 0..50 {
            assert_eq!(c.next_u64(), s.u64_at(n));
        }
        assert_eq!(c.counter(), 50);
    }

    #[test]
    fn cursor_counter_wraps() {
        let s = Stream::from_seed(8);
        let mut c = Cursor::at(s, u64::MAX);
        assert_eq!(c.next_u64(), s.u64_at(u64::MAX));
        assert_eq!(c.next_u64(), s.u64_at(0));
    }
}
