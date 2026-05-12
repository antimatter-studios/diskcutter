# rust-img-qcow2 — improvements wishlist

Notes captured while integrating `am-img-qcow2` (repo: https://github.com/antimatter-studios/rust-img-qcow2)
as the qcow2 read backend for Disk Cutter. The crate is usable, but a few items would smooth adoption for
downstream consumers.

## Resolved

### ~~1. Sibling `am-fs-core` is an unpublished path-dep~~ — fixed 2026-05-08

`am-fs-core` v0.1.0 was published to crates.io on 2026-05-08
(https://crates.io/crates/am-fs-core). A consumer can now `cargo add am-img-qcow2`
and Cargo will resolve `am-fs-core` from the registry — no vendoring or side-by-side
clone required.

## Blockers for direct adoption

### 2. Crates.io is stale — published v0.1.0 lags HEAD v0.2.0; no auto-publish pipeline

The current published release on crates.io is `am-img-qcow2` v0.1.0
(https://crates.io/crates/am-img-qcow2), and inspecting that tarball confirms it
includes only a subset of the feature set on `main`:

| Feature                                          | v0.1.0 (crates.io) | HEAD / v0.2.0 tag |
| ------------------------------------------------ | ------------------ | ----------------- |
| Uncompressed + zlib (deflate) clusters           | ✅                  | ✅                 |
| zstd-compressed clusters (v3 `compression_type`) | ❌ `Unsupported`    | ✅ (via `ruzstd`)  |
| Backing-file chain (path-based, depth 16)        | ✅                  | ✅                 |
| Path-based open (`open`, `open_rw`)              | ✅                  | ✅                 |
| Device-backed open (`open_on_device`)            | ❌ doesn't exist    | ✅                 |
| `fs_core::BlockRead` bridge                      | ✅                  | ✅                 |
| `fs_core::BlockDevice` impl                      | ❌                  | ✅                 |

The git tag `v0.2.0` exists and `Cargo.toml` on `main` declares `version = "0.2.0"`,
but the release was never pushed to crates.io because the CI workflow
(`.github/workflows/ci.yml`) only runs test/clippy/fmt — there is no publish job.

Consumers wanting the v0.2.0 feature set today must depend via
`git = "https://github.com/antimatter-studios/rust-img-qcow2"`, which reintroduces
the original path-dep problem for `am-fs-core` (since git deps don't compose with
crates.io deps cleanly when the downstream graph mixes registry- and git-sourced
versions of the same crate).

**Suggested fix:** add a `release.yml` workflow triggered on `v*.*.*` tag pushes
that runs `cargo publish` for `am-fs-core` first (it has no dependency on the
qcow2 crate) and then `am-img-qcow2`.

**Update 2026-05-12:** `am-fs-core` v0.2.0 is also pending (the `BlockReadStreamer`
work from item #3). When the publish pipeline lands it needs to handle both
crates' minor bumps, not just qcow2.

Pieces still not on HEAD that will surface as future blockers:

- Encryption support, external data file, extended L2 — all rejected with
  `Error::Unsupported`. Out of scope for Disk Cutter's burn use case but worth
  noting for the wishlist.

## High-leverage improvements

These aren't strict blockers but every consumer will hit them.

### 3. No `std::io::Read` impl on `Qcow2Reader` — landed in `am-fs-core` working tree (2026-05-12)

The crate exposes `read_at(offset, &mut buf)` (from `fs_core::BlockRead`). Every consumer
that wants to stream the virtual disk top-to-bottom (e.g. for hashing, copying, or
burning) ends up writing the same ~30-line `Read` adapter: track current offset, call
`read_at`, advance offset, signal EOF at `virtual_size`.

**Resolution direction:** rather than ship a qcow2-specific newtype, the adapter
landed one layer down in `am-fs-core` as `BlockReadStreamer<T: BlockRead>` —
generic over the parent so it serves *every* `BlockRead` consumer (partition
probes, slice readers, future image formats), not just qcow2. Implements `Read`
+ `Seek`; works through `Arc<dyn BlockRead>`, `&dyn BlockRead`, or owned-by-value.
16 unit tests; downstream qcow2 crate rebuilds and tests green against the
modified fs-core.

No `std-io` feature flag — `am-fs-core` is std-only already (so is qcow2), so a
flag would gate nothing.

**Still to do before this closes:**

1. Land `Qcow2Reader::reader(&self) -> fs_core::BlockReadStreamer<&Qcow2Reader>`
   on the qcow2 crate as a one-liner convenience so consumers write
   `r.reader()` instead of `BlockReadStreamer::new(&r)`.
2. Release `am-fs-core` v0.2.0 (additive — new struct + `impl BlockRead for &T`
   forwarding impl). Pushes Disk Cutter's dependency floor up by one minor.
3. Bump `am-img-qcow2` to `am-fs-core = "0.2"`, cut v0.3.0.
4. Commit + push the fs-core branch, open PR.

The fs-core change is uncommitted as of writing — local clone only.

### 4. No path-less / device-only open — already done on HEAD, blocked on publish

v0.1.0 only exposes `open(path)` / `open_rw(path)` / `open_best_effort(path)` —
every entry point takes a filesystem `Path`. Consumers operating on non-filesystem
sources (in-memory buffers, network streams, sandboxed VFS, an already-open
`BlockDevice`) have no way in on the published version.

HEAD / v0.2.0 already adds `Qcow2Reader::open_on_device(Arc<dyn BlockDevice>)` and
`open_rw_on_device(...)` ([reader.rs:145-157][on-device]), so this closes as soon
as v0.2.0 lands on crates.io. Verified locally — 24/24 qcow2 tests pass and the
synthetic-image test exercises this path.

The remaining gap is the backing-resolver: on-device opens still can't follow a
backing chain because the resolver is filesystem-relative, and the on-device
constructor explicitly rejects backed images with
`Error::Unsupported("image references a backing file; open by path to resolve
the chain")` ([reader.rs:199-206][on-device-backing]). Accepting a
`Fn(&str) -> io::Result<Box<dyn BlockDevice>>` callback would unblock the rare
case where a consumer wants both a non-filesystem source *and* a backing chain.
Not a blocker for Disk Cutter — `.qcow2` files we burn always live on disk.

[on-device]: https://github.com/antimatter-studios/rust-img-qcow2/blob/main/src/reader.rs#L145-L157
[on-device-backing]: https://github.com/antimatter-studios/rust-img-qcow2/blob/main/src/reader.rs#L199-L206

## Moved out of this wishlist (2026-05-12)

Former items #5 (progress callbacks), #6 (async/tokio), #7 (inspection without
full open), and #8 (typed `is_encrypted` accessors) all turned out to be
*Disk-Cutter-side* concerns — implementable today against the existing crate
API without any upstream change. Tracked in [status.md](status.md) under "UI
features moved here from qcow2 wishlist".

## Out of scope for v1

- Image creation / write support — Disk Cutter only reads qcow2.
- Mounting the contained filesystem — orthogonal to burning the raw virtual disk.
