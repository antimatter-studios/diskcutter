//! Legacy `readers` namespace, slimmed in Phase 3.
//!
//! Once held an `ImageReaderRegistry` plus per-format factories
//! (raw / gz / xz / bz2 / zstd / qcow2 / vhd / vhdx / vmdk). After the
//! decoder-chain migration, only the magic-number sniff helpers
//! remain — `crate::source` is the new single-front-door for every
//! "open an image" question.
//!
//! The container-format upstream crates (`qcow2`, `vhd`, `vhdx`,
//! `vmdk`) are now called directly from `crate::source::probe` /
//! `open_streaming`. The previous wrapper-factory layer is gone — it
//! was redundant once consumers stopped going through a trait-object
//! registry.

pub(crate) mod magic;
