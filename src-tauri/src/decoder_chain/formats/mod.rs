//! Format implementations for the decoder chain.
//!
//! Each submodule contributes one [`super::FormatTryOpen`] (and the
//! streaming decoder it wraps the source in on match). The static
//! references are re-exported and wired into [`super::READERS`] in
//! `decoder_chain/mod.rs`.
//!
//! Shared plumbing — currently just [`adapter::ReadAdapter`], the
//! `ReaderInterface` → `std::io::Read` bridge — lives here so every
//! format implementation reaches for the same shape.

pub(crate) mod adapter;
pub mod bzip2;
pub mod gzip;
pub mod xz;
pub mod zstd;
