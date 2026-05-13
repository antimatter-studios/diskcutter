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

The burn path is a three-stage pipeline. A producer thread reads the
source image (decompressing on the fly for `.gz` / `.xz` / `.qcow2` /
…) and pushes 1 MiB chunks into a bounded `mpsc::sync_channel`. N
worker threads pop chunks and issue `pwrite(fd, buf, off)` against a
shared file descriptor opened on the raw character device
(`/dev/rdiskN`) with `O_EXLOCK | F_NOCACHE`. Each worker's offset is
the chunk's logical position in the image, so the kernel never sees a
shared file cursor and the writes can proceed concurrently. When the
producer hits EOF and every worker has drained, the burn is done; a
read-back pass then re-hashes the device for verification.

See `docs/architecture.md` for the full tour — process model,
DiskArbitration session, verify fast/slow split, DB schema, frontend
reducers.

## The knobs

### `writer.impl`

The writer backend. One of `raw`, `block`, `pipelined`.

- **Default:** `pipelined`.
- **What it does:** picks which `DeviceIo` impl the helper uses. `raw`
  is a single-threaded `write_all` loop against `/dev/rdiskN`.
  `block` writes to the buffered block device `/dev/diskN`.
  `pipelined` is the worker-pool described above.
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

Number of OS threads issuing `pwrite` in parallel. **Pipelined only.**

- **Default:** `4`.
- **What it does:** each worker runs a loop of `channel.recv() →
  pwrite()`. They all share one FD; concurrency comes from issuing
  many in-flight writes so the USB stack can pipeline them.
- **Why the default:** matches Etcher's libuv threadpool default
  (`UV_THREADPOOL_SIZE=4`). Four workers keeps the USB driver queue
  saturated on the hardware we tested without over-contending on
  internal locks.
- **When to change it:** going to 8 or 16 mostly just adds contention
  for the channel mutex with no throughput win; the bottleneck is
  the device, not the CPU. Going down to 1 reproduces a slow `raw`-
  style write and is useful for A/B'ing whether the pipelining is
  actually helping on a given device.

### `queue.depth`

Capacity of the producer→workers channel. **Pipelined only.**

- **Default:** `15`.
- **What it does:** the producer can have up to this many chunks
  in-flight (buffered in the channel + actively being written) before
  it blocks waiting for a worker to free a slot.
- **Why the default:** 15 is Etcher's choice — enough headroom that
  the producer almost never stalls waiting for slow workers, not so
  large that we waste tens of MiB of RAM on buffered data we haven't
  written yet. At 1 MiB per chunk this caps the in-flight working set
  at ~15 MiB.
- **When to change it:** lower (4, 8) makes the pipeline stall-prone
  if any single `pwrite` is unusually slow — useful only for diagnostic
  runs. Higher (32, 64) burns more RAM but rarely helps; once the
  device queue is saturated, deeper buffering doesn't speed it up.

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

- **Default:** `sha256`.
- **Options:** `sha256`, `xxhash`.
- **What it does:** picks the function used to fingerprint the source
  stream during the burn and the device stream during verify. The
  helper compares the two; matching hashes ⇒ verified.
- **Why the default:** SHA-256 is the cryptographic-grade option and
  the value we record in `burn_jobs` for the audit log. xxhash is
  roughly 10× faster on CPU but is not collision-resistant against
  an adversary. For our use case — detecting accidental flips,
  truncated writes, partial transfers — a non-cryptographic hash is
  more than sufficient.
- **When to change it:** if the burn is CPU-bound (rare, but possible
  on slow ARM machines burning fast NVMe), xxhash will close the gap.
- **Caveat:** xxhash plumbing is wired through the pipeline (vendored
  xxh64, selectable via `--hash-algo`). See [CHANGELOG.md](../CHANGELOG.md)
  for the rollout note.

### `max.mismatches`

Upper bound on mismatch records collected by the slow-path verifier.

- **Default:** `256`.
- **What it does:** the fast verify path only compares two hashes; if
  they disagree, the slow path re-opens source and device and walks
  them byte-by-byte, recording the first N mismatches (LBA, byte
  offset, expected, actual) into `burn_mismatches`. This bound is N.
- **Why the default:** 256 is enough mismatches to spot a pattern (a
  truncated write at offset X, a single flipped bit, a stuck sector)
  without ballooning the row count or RAM use if the device is
  utterly corrupt and every block disagrees.
- **When to change it:** raise to 1024 if you're forensically
  diagnosing a problematic card and want a broader sample. Drop to 16
  / 64 if you just want a yes/no signal and don't care about the
  specifics.

## Suggested presets

| Goal                             | writer.impl | chunk.bytes | workers | queue | verify.skip |
| -------------------------------- | ----------- | ----------- | ------- | ----- | ----------- |
| Default (production flash)       | pipelined   | 1 MiB       | 4       | 15    | false       |
| Maximum iteration speed (dev)    | pipelined   | 1 MiB       | 4       | 15    | true        |
| Single-threaded reference        | raw         | 1 MiB       | —       | —     | false       |
| Buffered-cache comparison        | block       | 1 MiB       | —       | —     | false       |

If a tweak doesn't appear in that table, it didn't move the needle in
our testing.

## Benchmarking

To reproduce or extend the numbers above, run:

```
cargo run --release --example benchmark
```

This is being added in a separate change and may not be present in
your checkout yet; check `src-tauri/examples/` first. The example
loops over `writer.impl` × `chunk.bytes` × `workers` × `queue.depth`
permutations against a configurable target and emits a CSV of bytes,
elapsed, and MB/s for each run.

Numbers in this doc were measured on:

- Host: macOS, Apple silicon dev machine.
- Reader: USB-C SD card reader.
- Card: SanDisk Ultra (full-size SD, USB-MSC class).
- Image: standard Linux installer ISO sized in the low gigabytes.

Your device will differ. The relative ordering (`pipelined > raw >
block`) is consistent across the USB-MSC targets we've tested; the
absolute numbers track the device's published sustained-write spec.
