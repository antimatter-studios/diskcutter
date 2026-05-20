# rust-img-qcow2 — upstream wishlist

Disk Cutter currently uses `am-img-qcow2 = "0.3"` + `am-fs-core = "0.2"`
from crates.io. These items would smooth adoption further but none are
blockers; file upstream at
https://github.com/antimatter-studios/rust-img-qcow2 if pursuing.

## Ergonomic

- **`Qcow2Reader::reader(&self) -> BlockReadStreamer<&Qcow2Reader>`** —
  one-liner so consumers write `r.reader()` instead of
  `BlockReadStreamer::new(&r)`. Every site in [source.rs](../src-tauri/src/source.rs)
  currently spells the long form.

## On-device + backing chain

`open_on_device` rejects backed images with `Error::Unsupported` because
the backing resolver is filesystem-relative. Accepting a
`Fn(&str) -> io::Result<Box<dyn BlockDevice>>` callback would unblock
non-filesystem sources that also need backing-chain resolution. Not
relevant for Disk Cutter — `.qcow2` files we burn always live on disk.

## Out of scope for Disk Cutter

- Encryption, external data file, extended L2 — rejected upstream with
  `Error::Unsupported`. We only burn raw virtual disks, so none of these
  matter for the burn path.
- Image creation / write support — we only read qcow2.
- Mounting the contained filesystem — orthogonal to burning.
