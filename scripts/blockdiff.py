#!/usr/bin/env python3
"""Block-level differ for diagnosing burn corruption.

Reads two sources in lockstep (typically the source .img and a raw disk
readback, e.g. /dev/rdisk8) and reports every region where they differ,
coalesced into ranges and mapped to MBR partitions. One streaming pass each
— no full-file hashing, no temp files.

Usage:
    sudo python3 blockdiff.py <imageA> <deviceB> [--bs 1048576] [--limit BYTES]

Notes:
  * Read the RAW device node (/dev/rdiskN, not /dev/diskN) for speed.
  * --limit defaults to the size of imageA (compare only the meaningful span;
    the card is usually larger than the image).
  * Block size must be a multiple of 512 for raw char devices.
"""
import argparse
import sys

SECTOR = 512


def parse_mbr(first_block: bytes):
    """Return list of (idx, type_byte, start_lba, num_sectors) for non-empty
    primary partitions, or [] if no MBR signature."""
    if len(first_block) < 512 or first_block[510:512] != b"\x55\xaa":
        return []
    parts = []
    for i in range(4):
        e = first_block[446 + i * 16: 446 + i * 16 + 16]
        ptype = e[4]
        start = int.from_bytes(e[8:12], "little")
        count = int.from_bytes(e[12:16], "little")
        if ptype != 0 and count != 0:
            parts.append((i + 1, ptype, start, count))
    return parts


def which_partition(off: int, parts):
    sec = off // SECTOR
    hits = []
    for (idx, ptype, start, count) in parts:
        if start <= sec < start + count:
            hits.append(f"p{idx}(type=0x{ptype:02x})")
    if off < parts[0][2] * SECTOR if parts else False:
        return "MBR/gap-before-p1"
    return ",".join(hits) if hits else "unpartitioned/gap"


def human(n: int) -> str:
    for unit in ("B", "KiB", "MiB", "GiB"):
        if n < 1024 or unit == "GiB":
            return f"{n:.2f}{unit}" if unit != "B" else f"{n}B"
        n /= 1024


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("a")
    ap.add_argument("b")
    ap.add_argument("--bs", type=int, default=1 << 20)
    ap.add_argument("--limit", type=int, default=None)
    args = ap.parse_args()

    if args.bs % SECTOR:
        sys.exit(f"block size {args.bs} not a multiple of {SECTOR}")

    fa = open(args.a, "rb")
    fb = open(args.b, "rb")

    import os
    limit = args.limit
    if limit is None:
        fa.seek(0, os.SEEK_END)
        limit = fa.tell()
        fa.seek(0)

    first = fb.read(SECTOR)
    fb.seek(0)
    parts = parse_mbr(first)
    if parts:
        print("MBR partitions (from device B):", file=sys.stderr)
        for (idx, ptype, start, count) in parts:
            print(f"  p{idx}: type=0x{ptype:02x} start_lba={start} "
                  f"sectors={count} [{human(start*SECTOR)} .. {human((start+count)*SECTOR)}]",
                  file=sys.stderr)
    else:
        print("device B: no MBR signature in sector 0", file=sys.stderr)

    off = 0
    diff_ranges = []  # (start_off, end_off, bytes_differing)
    cur = None  # [start, end, ndiff]
    total_diff_bytes = 0
    next_progress = 1 << 30

    while off < limit:
        n = min(args.bs, limit - off)
        ba = fa.read(n)
        bb = fb.read(n)
        if not ba:
            break
        # Compare the overlapping length; short read on either side counts as diff.
        m = min(len(ba), len(bb))
        block_differs = ba[:m] != bb[:m] or len(ba) != len(bb)
        if block_differs:
            ndiff = sum(1 for x, y in zip(ba, bb) if x != y) + abs(len(ba) - len(bb))
            total_diff_bytes += ndiff
            if cur and cur[1] == off:
                cur[1] = off + len(ba)
                cur[2] += ndiff
            else:
                if cur:
                    diff_ranges.append(cur)
                cur = [off, off + len(ba), ndiff]
        off += len(ba)
        if len(ba) < n:  # EOF on A
            break
        if off >= next_progress:
            print(f"  ...scanned {human(off)} / {human(limit)}, "
                  f"{len(diff_ranges) + (1 if cur else 0)} diff ranges so far",
                  file=sys.stderr)
            next_progress += 1 << 30
    if cur:
        diff_ranges.append(cur)

    print(f"\n=== scanned {human(off)} ===")
    if not diff_ranges:
        print("IDENTICAL — every block of A matches B over the compared span.")
        print(">>> The on-disk bytes equal the image. The bug is NOT the written")
        print(">>> data. Suspect partition-table-vs-disk-geometry or the boot path.")
        return
    print(f"DIFFERENCES: {len(diff_ranges)} region(s), "
          f"{human(total_diff_bytes)} differing bytes total\n")
    for (s, e, nd) in diff_ranges[:200]:
        loc = which_partition(s, parts) if parts else "?"
        print(f"  offset {s:>14} .. {e:<14} ({human(e-s):>9})  "
              f"diffbytes={nd:<12}  -> {loc}")
    if len(diff_ranges) > 200:
        print(f"  ... and {len(diff_ranges)-200} more ranges")
    first_off = diff_ranges[0][0]
    print(f"\nFirst divergence at offset {first_off} "
          f"(sector {first_off // SECTOR}).")


if __name__ == "__main__":
    main()
