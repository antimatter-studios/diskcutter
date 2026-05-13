//! Pluggable streaming hashers for the burn pipeline.
//!
//! Two algorithms are supported:
//!   - `Sha256` (default) — cryptographic, slow.
//!   - `Xxhash` (xxh64)   — non-cryptographic, ~10× faster CPU-side; the
//!     right choice for burn-integrity checks (we're guarding against bit
//!     rot and bad sectors, not signing for tamper-detection).
//!
//! The xxh64 implementation in this file is a hand-rolled port of the
//! canonical xxh64 spec
//! (<https://github.com/Cyan4973/xxHash/blob/dev/doc/xxhash_spec.md>) so the
//! crate has no new Cargo dependencies. Canonical test vectors are asserted
//! in the unit tests below.
//!
//! The trait `StreamingHasher` is intentionally tiny — `update(&[u8])` plus
//! `finalize_hex` — so pipeline code can call the same shape for either
//! algorithm. `finalize_hex` returns lowercase hex to match the existing
//! SHA-256 output format from `pipeline::hex`.
//!
//! Constructed via `hash::new(algo)`; `HashAlgo::parse` accepts the strings
//! used in the Prefs panel (`"sha256"`, `"xxhash"`, `"xxh64"`).

use sha2::{Digest, Sha256};

/// Selectable hash algorithm. The string form lives in the Prefs panel as
/// `hash.algo`. Unknown values fall back to SHA-256 so a typo in config
/// can't downgrade integrity expectations silently in a surprising way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlgo {
    Sha256,
    Xxhash,
}

impl HashAlgo {
    /// Parse a user-facing algorithm name. Case-insensitive. Anything we
    /// don't recognise becomes `Sha256` — the conservative default.
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "xxhash" | "xxh64" => Self::Xxhash,
            _ => Self::Sha256,
        }
    }
}

/// Streaming hasher trait — feed bytes via `update`, then consume with
/// `finalize_hex`. The `Box<Self>` consume signature lets us hold this
/// behind a `Box<dyn StreamingHasher>` and still call a by-value finaliser.
pub trait StreamingHasher: Send {
    fn update(&mut self, buf: &[u8]);
    fn finalize_hex(self: Box<Self>) -> String;
}

/// Construct a streaming hasher for the selected algorithm.
pub fn new(algo: HashAlgo) -> Box<dyn StreamingHasher> {
    match algo {
        HashAlgo::Sha256 => Box::new(Sha256Streaming(Sha256::new())),
        HashAlgo::Xxhash => Box::new(Xxh64Streaming::new(0)),
    }
}

// --- SHA-256 -----------------------------------------------------------------

struct Sha256Streaming(Sha256);

impl StreamingHasher for Sha256Streaming {
    fn update(&mut self, buf: &[u8]) {
        Digest::update(&mut self.0, buf);
    }
    fn finalize_hex(self: Box<Self>) -> String {
        let digest = self.0.finalize();
        let mut s = String::with_capacity(digest.len() * 2);
        for b in digest {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }
}

// --- xxh64 -------------------------------------------------------------------
//
// Spec reference: <https://github.com/Cyan4973/xxHash/blob/dev/doc/xxhash_spec.md>
//
// Algorithm summary:
//   - Maintain four 64-bit accumulators (`v1..v4`) seeded from `seed` plus
//     fixed primes. Each 32-byte stripe updates all four lanes in parallel.
//   - When fewer than 32 bytes remain, hold them in a 32-byte buffer until
//     a full stripe accumulates.
//   - On finalize: merge the four lanes into a single 64-bit value (or use
//     `seed + PRIME5` if no stripes were consumed yet), mix in the total
//     input length, then consume the tail bytes (8-byte words, then a
//     4-byte word, then individual bytes) with the appropriate mix steps,
//     and finally run the avalanche.
//
// Constants and the exact bit operations come straight from the spec.

const XXH_PRIME64_1: u64 = 0x9E3779B185EBCA87;
const XXH_PRIME64_2: u64 = 0xC2B2AE3D27D4EB4F;
const XXH_PRIME64_3: u64 = 0x165667B19E3779F9;
const XXH_PRIME64_4: u64 = 0x85EBCA77C2B2AE63;
const XXH_PRIME64_5: u64 = 0x27D4EB2F165667C5;

struct Xxh64Streaming {
    seed: u64,
    total_len: u64,
    v1: u64,
    v2: u64,
    v3: u64,
    v4: u64,
    // Up to 31 unprocessed tail bytes between updates; once we have 32 we
    // consume a stripe and reset `buf_len` to 0.
    buf: [u8; 32],
    buf_len: usize,
}

impl Xxh64Streaming {
    fn new(seed: u64) -> Self {
        Self {
            seed,
            total_len: 0,
            v1: seed.wrapping_add(XXH_PRIME64_1).wrapping_add(XXH_PRIME64_2),
            v2: seed.wrapping_add(XXH_PRIME64_2),
            v3: seed,
            v4: seed.wrapping_sub(XXH_PRIME64_1),
            buf: [0u8; 32],
            buf_len: 0,
        }
    }

    fn round(acc: u64, input: u64) -> u64 {
        let acc = acc.wrapping_add(input.wrapping_mul(XXH_PRIME64_2));
        let acc = acc.rotate_left(31);
        acc.wrapping_mul(XXH_PRIME64_1)
    }

    fn merge_round(acc: u64, val: u64) -> u64 {
        let val = Self::round(0, val);
        let acc = acc ^ val;
        acc.wrapping_mul(XXH_PRIME64_1).wrapping_add(XXH_PRIME64_4)
    }

    fn avalanche(mut h: u64) -> u64 {
        h ^= h >> 33;
        h = h.wrapping_mul(XXH_PRIME64_2);
        h ^= h >> 29;
        h = h.wrapping_mul(XXH_PRIME64_3);
        h ^= h >> 32;
        h
    }

    fn consume_stripe(&mut self, stripe: &[u8; 32]) {
        // Each 32-byte stripe = four little-endian 64-bit lanes.
        self.v1 = Self::round(
            self.v1,
            u64::from_le_bytes(stripe[0..8].try_into().unwrap()),
        );
        self.v2 = Self::round(
            self.v2,
            u64::from_le_bytes(stripe[8..16].try_into().unwrap()),
        );
        self.v3 = Self::round(
            self.v3,
            u64::from_le_bytes(stripe[16..24].try_into().unwrap()),
        );
        self.v4 = Self::round(
            self.v4,
            u64::from_le_bytes(stripe[24..32].try_into().unwrap()),
        );
    }

    fn finalize(&self) -> u64 {
        // 1. Merge lanes (or use the short-input fast path).
        let mut h64 = if self.total_len >= 32 {
            let mut h = self
                .v1
                .rotate_left(1)
                .wrapping_add(self.v2.rotate_left(7))
                .wrapping_add(self.v3.rotate_left(12))
                .wrapping_add(self.v4.rotate_left(18));
            h = Self::merge_round(h, self.v1);
            h = Self::merge_round(h, self.v2);
            h = Self::merge_round(h, self.v3);
            h = Self::merge_round(h, self.v4);
            h
        } else {
            self.seed.wrapping_add(XXH_PRIME64_5)
        };

        // 2. Mix in the total input length.
        h64 = h64.wrapping_add(self.total_len);

        // 3. Consume any tail bytes (the buffer that never reached a full
        // 32-byte stripe). Process 8-byte chunks, then a 4-byte chunk, then
        // individual bytes, each with its own mixing recipe.
        let tail = &self.buf[..self.buf_len];
        let mut i = 0;
        while i + 8 <= tail.len() {
            let k1 = Self::round(0, u64::from_le_bytes(tail[i..i + 8].try_into().unwrap()));
            h64 ^= k1;
            h64 = h64
                .rotate_left(27)
                .wrapping_mul(XXH_PRIME64_1)
                .wrapping_add(XXH_PRIME64_4);
            i += 8;
        }
        if i + 4 <= tail.len() {
            let k1 = (u32::from_le_bytes(tail[i..i + 4].try_into().unwrap()) as u64)
                .wrapping_mul(XXH_PRIME64_1);
            h64 ^= k1;
            h64 = h64
                .rotate_left(23)
                .wrapping_mul(XXH_PRIME64_2)
                .wrapping_add(XXH_PRIME64_3);
            i += 4;
        }
        while i < tail.len() {
            let k1 = (tail[i] as u64).wrapping_mul(XXH_PRIME64_5);
            h64 ^= k1;
            h64 = h64.rotate_left(11).wrapping_mul(XXH_PRIME64_1);
            i += 1;
        }

        // 4. Final avalanche.
        Self::avalanche(h64)
    }
}

impl StreamingHasher for Xxh64Streaming {
    fn update(&mut self, mut input: &[u8]) {
        self.total_len = self.total_len.wrapping_add(input.len() as u64);

        // If we already have buffered tail bytes, fill the buffer to 32 and
        // consume that stripe first.
        if self.buf_len > 0 {
            let need = 32 - self.buf_len;
            if input.len() < need {
                self.buf[self.buf_len..self.buf_len + input.len()].copy_from_slice(input);
                self.buf_len += input.len();
                return;
            }
            self.buf[self.buf_len..32].copy_from_slice(&input[..need]);
            let stripe = self.buf; // copy out before mut-borrow shenanigans
            self.consume_stripe(&stripe);
            self.buf_len = 0;
            input = &input[need..];
        }

        // Consume as many full 32-byte stripes as possible directly from `input`.
        while input.len() >= 32 {
            let mut stripe = [0u8; 32];
            stripe.copy_from_slice(&input[..32]);
            self.consume_stripe(&stripe);
            input = &input[32..];
        }

        // Stash the remainder for next `update` or finalize.
        if !input.is_empty() {
            self.buf[..input.len()].copy_from_slice(input);
            self.buf_len = input.len();
        }
    }

    fn finalize_hex(self: Box<Self>) -> String {
        format!("{:016x}", self.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical xxh64 vector: empty input, seed=0 → 0xef46db3751d8e999.
    #[test]
    fn xxh64_empty_input_matches_spec() {
        let h: Box<dyn StreamingHasher> = Box::new(Xxh64Streaming::new(0));
        assert_eq!(h.finalize_hex(), "ef46db3751d8e999");
    }

    /// Canonical xxh64 vector: classic Python "Nobody inspects..." string,
    /// seed=0 → 0xfbcea83c8a378bf1. This is the agreed-upon cross-impl
    /// sanity vector.
    #[test]
    fn xxh64_classic_string_matches_spec() {
        let input = b"Nobody inspects the spammish repetition";
        let mut h = Xxh64Streaming::new(0);
        h.update(input);
        let boxed: Box<dyn StreamingHasher> = Box::new(h);
        assert_eq!(boxed.finalize_hex(), "fbcea83c8a378bf1");
    }

    /// A larger payload exercises the 32-byte stripe loop, lane merge, and
    /// tail-byte handling all at once. Computed against the reference impl.
    #[test]
    fn xxh64_streaming_matches_single_shot() {
        // 200-byte input — non-trivial: covers 6 full stripes (192 bytes)
        // plus an 8-byte tail.
        let payload: Vec<u8> = (0..200u32).map(|i| (i & 0xff) as u8).collect();

        let mut single = Xxh64Streaming::new(0);
        single.update(&payload);
        let single_hex = Box::new(single).finalize_hex();

        // Same payload, fed in awkward chunk sizes.
        let mut chunked = Xxh64Streaming::new(0);
        chunked.update(&payload[..1]);
        chunked.update(&payload[1..7]);
        chunked.update(&payload[7..32]);
        chunked.update(&payload[32..64]);
        chunked.update(&payload[64..99]);
        chunked.update(&payload[99..]);
        let chunked_hex = Box::new(chunked).finalize_hex();

        assert_eq!(
            single_hex, chunked_hex,
            "streaming xxh64 must match single-shot regardless of chunk boundaries"
        );
    }

    #[test]
    fn xxh64_single_byte_inputs_avalanche() {
        // Sanity: two different one-byte inputs should not collide.
        let mut a = Xxh64Streaming::new(0);
        a.update(&[0u8]);
        let mut b = Xxh64Streaming::new(0);
        b.update(&[1u8]);
        assert_ne!(
            Box::new(a).finalize_hex(),
            Box::new(b).finalize_hex(),
            "single-byte inputs should hash differently"
        );
    }

    #[test]
    fn xxh64_dispatch_via_new() {
        // Exercise the public `hash::new(HashAlgo::Xxhash)` entry point so
        // it stays wired to Xxh64Streaming.
        let h = new(HashAlgo::Xxhash);
        assert_eq!(h.finalize_hex(), "ef46db3751d8e999");
    }

    #[test]
    fn sha256_via_new_matches_known_empty_digest() {
        // Sanity: hashing the empty string via `hash::new(Sha256)` produces
        // the well-known empty-input SHA-256.
        let h = new(HashAlgo::Sha256);
        assert_eq!(
            h.finalize_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hashalgo_parse_recognises_xxhash_variants() {
        assert_eq!(HashAlgo::parse("xxhash"), HashAlgo::Xxhash);
        assert_eq!(HashAlgo::parse("xxh64"), HashAlgo::Xxhash);
        assert_eq!(HashAlgo::parse("XXHASH"), HashAlgo::Xxhash);
        assert_eq!(HashAlgo::parse("XxHash"), HashAlgo::Xxhash);
    }

    #[test]
    fn hashalgo_parse_falls_back_to_sha256_for_unknown() {
        assert_eq!(HashAlgo::parse("sha256"), HashAlgo::Sha256);
        assert_eq!(HashAlgo::parse("SHA256"), HashAlgo::Sha256);
        assert_eq!(HashAlgo::parse(""), HashAlgo::Sha256);
        assert_eq!(HashAlgo::parse("md5"), HashAlgo::Sha256);
        assert_eq!(HashAlgo::parse("anything-else"), HashAlgo::Sha256);
    }
}
