//! Pluggable streaming hashers for the burn pipeline.
//!
//! Two algorithms are supported:
//!   - `Xxh3` (default) — non-cryptographic, ~10-20 GB/s on Apple silicon NEON
//!     and AVX2 x86. This is what balena Etcher uses and is the right choice
//!     for burn-integrity checks: we are guarding against bit rot, truncated
//!     transfers and bad sectors, not signing for tamper-detection.
//!   - `Sha256` — cryptographic, slow (~400 MB/s). Kept as an opt-in for
//!     anyone who specifically wants the cryptographic digest in their
//!     burn_history audit log.
//!
//! Xxh3 is provided by `twox-hash` and goes through its native vector path
//! (NEON on aarch64, AVX2 on x86_64). Output is the 64-bit hex digest, same
//! 16-char shape as the previous xxh64 implementation so the column width
//! constraint in burn_history stays valid.
//!
//! The trait `StreamingHasher` is intentionally tiny — `update(&[u8])` plus
//! `finalize_hex` — so pipeline code can call the same shape for either
//! algorithm. `finalize_hex` returns lowercase hex to match the existing
//! SHA-256 output format from `pipeline::hex`.
//!
//! Constructed via `hash::new(algo)`; `HashAlgo::parse` accepts every string
//! the Prefs panel has ever written (`"sha256"`, `"xxh3"`, plus the legacy
//! `"xxhash"` / `"xxh64"` values from old stored configs, which now alias to
//! Xxh3 since the standalone xxh64 implementation has been retired).

use sha2::{Digest, Sha256};
use std::hash::Hasher as _;
use twox_hash::XxHash3_64;

/// Selectable hash algorithm. The string form lives in the Prefs panel as
/// `hash.algo`. Unknown values fall back to Xxh3 — the new performance-
/// oriented default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlgo {
    Sha256,
    Xxh3,
}

impl HashAlgo {
    /// Parse a user-facing algorithm name. Case-insensitive. Legacy values
    /// (`xxhash`, `xxh64`) that older versions wrote to config map to Xxh3
    /// so upgraded installs don't silently fall back to SHA-256. Unknown
    /// strings also resolve to Xxh3 — the burn defaults to the fast hash.
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "sha256" => Self::Sha256,
            _ => Self::Xxh3,
        }
    }

    /// Canonical short label used in burn_history rows and helper info
    /// logs. Distinct from `parse` because `parse` is lenient (accepts
    /// legacy aliases) but the label must be a single canonical form.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Xxh3 => "xxh3",
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
        HashAlgo::Xxh3 => Box::new(Xxh3Streaming(XxHash3_64::new())),
    }
}

/// One-shot Xxh3-64 fingerprint of a single chunk. The burn pipeline records
/// one of these per fixed-size chunk so read-back can pinpoint *which* chunks
/// differ (the repair work-list) without re-reading the source.
///
/// Deliberately always Xxh3, independent of the burn's configured `HashAlgo`:
/// this is fixed-width integrity localisation for the repair path, not the
/// user-facing audit digest (which still honours the SHA-256 opt-in). Equal
/// chunk bytes always yield equal digests, so a per-chunk comparison is exact
/// up to the ~2^-64 collision probability that the whole-image hash also
/// accepts.
pub fn chunk_digest(buf: &[u8]) -> u64 {
    let mut h = XxHash3_64::new();
    h.write(buf);
    h.finish()
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

// --- Xxh3 (64-bit) -----------------------------------------------------------
//
// Thin wrapper around `twox_hash::XxHash3_64`. The crate exposes the
// `core::hash::Hasher` trait: `write(&[u8])` feeds bytes, `finish()` returns
// the 64-bit digest. Internally it dispatches to AVX2 / NEON / SSE2 / portable
// fallbacks at runtime, which is what gives the ~10-20 GB/s on modern CPUs.

struct Xxh3Streaming(XxHash3_64);

impl StreamingHasher for Xxh3Streaming {
    fn update(&mut self, buf: &[u8]) {
        self.0.write(buf);
    }
    fn finalize_hex(self: Box<Self>) -> String {
        format!("{:016x}", self.0.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity: xxh3 of the empty input matches the documented constant
    /// from the xxHash spec (0x2d06800538d394c2). If twox-hash ever
    /// changes its empty-input output we want this test to catch it.
    #[test]
    fn xxh3_empty_input_matches_spec() {
        let h: Box<dyn StreamingHasher> = Box::new(Xxh3Streaming(XxHash3_64::new()));
        assert_eq!(h.finalize_hex(), "2d06800538d394c2");
    }

    /// Sanity: streaming xxh3 across awkward chunk boundaries must yield
    /// the same digest as a single-shot update. This is what the burn
    /// pipeline relies on when feeding decompressed chunks of varying
    /// sizes.
    #[test]
    fn xxh3_streaming_matches_single_shot() {
        let payload: Vec<u8> = (0..200u32).map(|i| (i & 0xff) as u8).collect();

        let mut single = Xxh3Streaming(XxHash3_64::new());
        single.update(&payload);
        let single_hex = Box::new(single).finalize_hex();

        let mut chunked = Xxh3Streaming(XxHash3_64::new());
        chunked.update(&payload[..1]);
        chunked.update(&payload[1..7]);
        chunked.update(&payload[7..32]);
        chunked.update(&payload[32..64]);
        chunked.update(&payload[64..99]);
        chunked.update(&payload[99..]);
        let chunked_hex = Box::new(chunked).finalize_hex();

        assert_eq!(
            single_hex, chunked_hex,
            "streaming xxh3 must match single-shot regardless of chunk boundaries"
        );
    }

    #[test]
    fn xxh3_single_byte_inputs_avalanche() {
        let mut a = Xxh3Streaming(XxHash3_64::new());
        a.update(&[0u8]);
        let mut b = Xxh3Streaming(XxHash3_64::new());
        b.update(&[1u8]);
        assert_ne!(
            Box::new(a).finalize_hex(),
            Box::new(b).finalize_hex(),
            "single-byte inputs should hash differently"
        );
    }

    #[test]
    fn xxh3_dispatch_via_new() {
        let h = new(HashAlgo::Xxh3);
        assert_eq!(h.finalize_hex(), "2d06800538d394c2");
    }

    #[test]
    fn sha256_via_new_matches_known_empty_digest() {
        let h = new(HashAlgo::Sha256);
        assert_eq!(
            h.finalize_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hashalgo_parse_recognises_xxh3_and_legacy_aliases() {
        assert_eq!(HashAlgo::parse("xxh3"), HashAlgo::Xxh3);
        assert_eq!(HashAlgo::parse("XXH3"), HashAlgo::Xxh3);
        // Legacy values older configs may still hold:
        assert_eq!(HashAlgo::parse("xxhash"), HashAlgo::Xxh3);
        assert_eq!(HashAlgo::parse("xxh64"), HashAlgo::Xxh3);
        assert_eq!(HashAlgo::parse("XxHash"), HashAlgo::Xxh3);
    }

    #[test]
    fn hashalgo_parse_recognises_sha256_and_defaults_otherwise_to_xxh3() {
        assert_eq!(HashAlgo::parse("sha256"), HashAlgo::Sha256);
        assert_eq!(HashAlgo::parse("SHA256"), HashAlgo::Sha256);
        assert_eq!(HashAlgo::parse(""), HashAlgo::Xxh3);
        assert_eq!(HashAlgo::parse("md5"), HashAlgo::Xxh3);
        assert_eq!(HashAlgo::parse("anything-else"), HashAlgo::Xxh3);
    }

    #[test]
    fn hashalgo_label_returns_canonical_short_form() {
        assert_eq!(HashAlgo::Sha256.label(), "sha256");
        assert_eq!(HashAlgo::Xxh3.label(), "xxh3");
    }
}
