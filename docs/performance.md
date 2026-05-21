# Performance

Disk Cutter exposes about a dozen runtime knobs under **Prefs →
Performance**. They are all persisted in the SQLite `config` table and
read fresh by the helper subprocess on the next burn, so no restart is
required. The defaults are chosen to maximise throughput on a typical
macOS + USB-MSC card reader combination. This doc explains what each
knob does, why the default is what it is, and when it makes sense to
deviate.

If you only read one section: leave everything alone. The defaults are
the fastest combination we've measured.

## Architecture quick-reference

The burn path is a two-stage pipeline with a buffer pool between
producer and writer. A producer thread reads the source image
(decompressing on the fly for `.gz` / `.xz` / `.qcow2` / …), borrows
an empty `Vec<u8>` from a pre-allocated free pool of `queue.depth`
buffers, copies the chunk in, and ships it to a **single** writer
thread via a bounded `mpsc::sync_channel`. The writer issues
`pwrite(fd, buf, off)` against the raw character device
(`/dev/rdiskN`) opened with `O_EXLOCK | F_NOCACHE` and then returns
the now-flushed `Vec` to the free pool so the next chunk can reuse
it — there is no heap allocation per chunk, only the one-time
fill-the-pool allocation. When the producer hits EOF and the writer
drains, the burn is done; a read-back pass then re-hashes the device
for verification.

This is the same shape as balena Etcher's `BlockWriteStream`: one
in-flight `pwrite` per destination, a ~16-buffer ring for
backpressure, no parallel writers. An earlier diskcutter version used
N worker threads serialised through an `Arc<Mutex<mpsc::Receiver>>`,
which made the "workers" effectively single-file under one lock; the
mutex is gone, and the `workers.count` knob is preserved purely for
config compatibility (it is no longer interpreted at runtime).

See `docs/architecture.md` for the full tour — process model,
DiskArbitration session, verify fast/slow split, DB schema, frontend
reducers.

## The knobs

### `writer.impl`

The writer backend. One of `raw`, `block`, `pipelined`, `parallel`.

- **Default:** `pipelined`.
- **What it does:** picks which `DeviceIo` impl the helper uses. `raw`
  is a single-threaded `write_all` loop against `/dev/rdiskN`.
  `block` writes to the buffered block device `/dev/diskN`.
  `pipelined` is the single-worker + buffer-pool described above.
  `parallel` is the same buffer-pool model but with N worker threads
  pulling from a lock-free MPMC channel so the kernel can have up to
  N pwrite syscalls in flight against the same FD.
- **Why the default:** `pipelined` keeps the USB driver queue full at
  all times. On the dev hardware (SanDisk Ultra SD card via a USB-C
  reader) we measure roughly:
  - `raw` — ~15 MB/s
  - `block` — ~8.5 MB/s
  - `pipelined` — ~71 MB/s
  That's ~5× the throughput of `raw` for the same image and device.
- **When to change it:** if you're debugging a suspected pipelined-
  writer bug, switch to `raw` to get a known-correct single-threaded
  reference. `block` is included as an experiment toggle — it goes
  through the kernel's buffered block path, which sounds like it
  should be faster but is slower in practice because the cache fills
  RAM and then has to be flushed before the burn is "done."
  `parallel` is an experimental opt-in for hardware that supports
  overlapping I/O (Thunderbolt NVMe enclosures, high-end card
  readers with tagged command queueing). **On every consumer SD/USB
  adapter we have benchmarked, `parallel` is slower than `pipelined`
  — the macOS USB-MSC layer serialises concurrent pwrites against
  the same FD and adds queueing overhead on top, so per-pwrite
  latency rises roughly in proportion to `workers.count` and net
  throughput drops.** Only enable it if you have a target you know
  supports queued IO and you have measured a real gain.

### `chunk.bytes`

Size of each I/O chunk handed from the producer to the workers.

- **Default:** `1048576` (1 MiB).
- **What it does:** every `pwrite` is exactly this many bytes (the
  final chunk may be shorter). Smaller chunks mean more syscalls;
  bigger chunks mean fewer.
- **Why the default:** 1 MiB is the maximum transfer length that
  macOS's USB Mass Storage Class driver will accept in a single
  request. Hand it anything bigger and the kernel splits the request
  internally — you pay the syscall once and the USB stack pays the
  per-transaction overhead N times anyway. 1 MiB is the sweet spot
  where one chunk == one USB transaction.
- **When to change it:** going smaller (256 / 512 KiB) is only useful
  if you suspect the device has a small internal write window and is
  rejecting larger transactions; you'll pay more syscalls for no
  obvious gain. Going larger (2 / 4 / 8 / 16 MiB) is sometimes worth
  trying on non-USB targets (Thunderbolt enclosures, internal NVMe)
  where the per-transaction overhead is higher than the syscall cost.

### `workers.count`

**No longer interpreted at runtime — kept for prefs / CLI compatibility.**

- **Default (config):** `4`.
- **What it actually does today:** nothing. The pipelined writer was
  rewritten to use a single writer thread fed by a buffer pool — the
  same shape Etcher uses. The `workers.count` value is parsed and
  passed through to `PipelinedRawDeviceIo::new(workers, ...)` so the
  helper CLI surface stays stable, but the worker count is ignored
  inside `open_write`.
- **Why the change:** the previous N-worker implementation serialised
  every `recv()` through one `Arc<Mutex<mpsc::Receiver>>`, so workers
  were never really running in parallel. Inspecting Etcher's
  `BlockWriteStream` confirmed that one pwrite at a time is enough to
  saturate USB-MSC targets; the parallelism that matters is between
  the *producer* (decode + hash) and the *writer*, not among multiple
  writers. Collapsing to a single worker also let us drop the
  per-chunk `Vec::to_vec()` allocation and replace it with a pooled
  buffer recycled through a free-list channel.
- **When to change it:** don't bother. The knob will be removed once
  the corresponding UI selector is dropped.

### `queue.depth`

Size of the recyclable buffer pool between producer and writer.
**Pipelined only.**

- **Default:** `16`.
- **What it does:** the producer borrows an empty `Vec<u8>` from the
  pool, fills it, and ships it to the writer thread; the writer
  pwrites and returns the (still-allocated) `Vec` to the pool. The
  pool holds this many buffers total, so the producer can have at
  most this many chunks in flight before it blocks on
  `free_rx.recv()` waiting for the writer to recycle one.
- **Why the default:** 16 matches Etcher's `numBuffers=16` ring —
  enough headroom that the producer almost never stalls waiting for
  the writer, not so large that we waste tens of MiB of RAM on
  buffered data we haven't written yet. At 1 MiB per chunk this caps
  the in-flight working set at ~16 MiB.
- **When to change it:** lower (4, 8) makes the pipeline stall-prone
  if any single `pwrite` is unusually slow — useful only for
  diagnostic runs. Higher (32, 64) burns more RAM but rarely helps;
  once the device's internal queue is saturated, deeper buffering
  doesn't speed it up.

### `verify.skip`

Skip the post-burn read-back pass entirely.

- **Default:** `false` (verification runs).
- **What it does:** after the write finishes, normally the helper
  re-reads the entire device and hashes it, comparing against the
  burn-side source hash. With this on, the burn returns success the
  moment the last byte is written.
- **Why the default:** silent corruption is the worst failure mode for
  bootable media — the burn says success, the user takes the stick
  off, the machine won't boot, and the cause is invisible.
- **When to change it:** flip it on during repeated dev/test cycles
  where you trust the device and want the iteration time. Verification
  is roughly a 1× read-back over the same medium that was just
  written, so skipping it roughly halves total burn time. Leave it
  off for any production / "real" flash.

### `hash.algo`

Hash algorithm used for source and read-back integrity check.

- **Default:** `xxh3`.
- **Options:** `xxh3`, `sha256`.
- **What it does:** picks the function used to fingerprint the source
  stream during the burn and the device stream during verify. The
  helper compares the two; matching hashes ⇒ verified.
- **Why the default:** xxh3 is what balena Etcher uses for the same
  purpose. It runs at ~10-20 GB/s on Apple silicon NEON / AVX2 x86,
  so it is no longer the producer-thread bottleneck — SHA-256 at
  ~400 MB/s on the same producer thread was capping throughput well
  before the device did. For our use case — detecting accidental
  flips, truncated writes, partial transfers — a non-cryptographic
  hash is more than sufficient; we are not signing for tamper-
  detection. Legacy stored config values (`xxhash`, `xxh64`) parse
  to the same Xxh3 variant so upgraded installs do not silently
  downgrade to SHA-256.
- **When to change it:** flip to `sha256` if you specifically want a
  cryptographic digest in `burn_jobs` for audit / forensic purposes.
  Expect a meaningful throughput hit on fast devices.
- **Wiring:** `disks.rs::build_helper_command` now reads `hash.algo`
  from the SQLite config and forwards it to the elevated helper as
  `--hash-algo=<value>`. The helper, the CLI, and the in-process
  burn path all honour the same value.

### Mismatch cap (not a runtime knob)

The slow-path verifier caps mismatch records at `MAX_MISMATCHES = 256`
([pipeline.rs](../src-tauri/src/pipeline.rs)). This is a compile-time
constant — there is no config key. 256 is enough to spot a pattern (a
truncated write at offset X, a single flipped bit, a stuck sector)
without ballooning row count or RAM if the device is utterly corrupt.
Edit the constant if you genuinely need a different bound.

## Suggested presets

| Goal                             | writer.impl | chunk.bytes | workers | queue | verify.skip |
| -------------------------------- | ----------- | ----------- | ------- | ----- | ----------- |
| Default (production flash)       | pipelined   | 1 MiB       | 4       | 16    | false       |
| Maximum iteration speed (dev)    | pipelined   | 1 MiB       | 4       | 16    | true        |
| Single-threaded reference        | raw         | 1 MiB       | —       | —     | false       |
| Buffered-cache comparison        | block       | 1 MiB       | —       | —     | false       |

If a tweak doesn't appear in that table, it didn't move the needle
in our testing. `hash.algo` is omitted because the new default
(xxh3) is fast enough that no one should need to swap it; the row
would just say "xxh3" in every cell.

## Real-world adapter variability (read this before filing a perf issue)

The single largest factor in observed burn speed is **the SD-to-host
adapter you plug into**, not anything in Disk Cutter or even the card.
Real numbers from one machine + one card, varying only the adapter:

| Adapter / port                                | MB/s |
| --------------------------------------------- | ---- |
| Cheap HDMI/SD/USB multi-dongle #1             |  ~7  |
| Cheap HDMI/SD/USB multi-dongle #2             | ~25  |
| Generic USB-A SD adapter #1 (USB 3 port)      | ~14  |
| Quality USB-A SD adapter #2 (USB 3 port)      | ~70  |

That is a **10× spread across adapters on the same machine**, same
card, same code, same operating system. The bottleneck on a slow
adapter is its USB-MSC bridge chipset's sustained throughput — every
`pwrite()` blocks waiting for the bridge to acknowledge the bytes have
been transferred. No amount of pipelining, hashing tweaks, or larger
chunks can move past that ceiling, because we are already extracting
the bridge's full single-pwrite serial rate.

How to tell if your adapter is the bottleneck rather than the app:

- In Prefs → Performance, set `debug.logging = true`, run a burn, and
  look at `/tmp/disk-cutter-pwrite-stats.log`. The `mbps` field there
  is the device-bound throughput inferred purely from `pwrite()`
  duration. If that is already close to the wall-clock MB/s shown in
  the UI, the kernel/device path is the cap, not the burn pipeline.
- Compare against balena Etcher writing the same card on the same
  port. If Etcher is also slow, the cause is the adapter or card. If
  Etcher is significantly faster on the same hardware, open an issue
  — there is something we are doing that Etcher is not.

The macOS USB-MSC driver also caps single-pwrite size via the
`DKIOCGETMAXBYTECOUNTWRITE` ioctl (often 4 MiB on consumer adapters).
Setting `chunk.bytes` above that value in Prefs is silently no-op —
the helper clamps to whatever the kernel reports. The current chunk
size in use appears in the helper info log on every burn:

```
helper: writer=raw-pipelined target=/dev/disk9 chunk=4194304 ...
```

If you see `chunk=4194304` even though you set `chunk.bytes=16777216`,
the device's reported max is 4 MiB and you have already hit it.

## Benchmarking

To reproduce or extend the numbers above, run:

```
cargo run --release --example benchmark
```

The example lives at `src-tauri/examples/benchmark.rs` and loops over
`writer.impl` × `chunk.bytes` × `workers` × `queue.depth` permutations
against a configurable target, emitting a CSV of bytes, elapsed, and
MB/s for each run. A companion `hash_bench` example in the same
directory benchmarks the CPU-side hash algorithms in isolation.

Numbers in this doc were measured on:

- Host: macOS, Apple silicon dev machine.
- Reader: USB-C SD card reader.
- Card: SanDisk Ultra (full-size SD, USB-MSC class).
- Image: standard Linux installer ISO sized in the low gigabytes.

Your device will differ. The relative ordering (`pipelined > raw >
block`) is consistent across the USB-MSC targets we've tested; the
absolute numbers track the device's published sustained-write spec.
