# Disk Cutter — TODO

Tracking upcoming feature work. Each item should explain the *why* and the rough scope.

## In progress

## Done

### i18n / translation catalog

Shipped `react-i18next` with auto-discovered catalogs in `src/i18n/locales/` (en/de/es, 155
keys each), pluralization for disk/job counts, and a LANGUAGE picker in the Tweaks panel that
enumerates locales via `import.meta.glob` and uses each catalog's `language.name` as its
own label.

## Backlog

### Real raw-device I/O

`PlainFileDeviceIo` writes to a temp file; real burns need `/dev/diskN` access with `O_DIRECT
| O_SYNC` on Unix, `FILE_FLAG_NO_BUFFERING` on Windows. Requires elevation strategy (sudo
prompt vs privileged helper vs SMJobBless/polkit).

### Real disk enumeration

macOS: `diskutil list -plist` + `diskutil info -plist /dev/diskN`. Linux: `/sys/block`.
Windows: SetupDi APIs. Returns the existing `Disk` shape.

### SQLite history (`burn_jobs` table)

Add `tauri-plugin-sql`, schema with `burn_jobs` + `burn_mismatches` tables. Insert at write
start, update at finish. Drives Sidebar DONE/FAILED counts persistently across sessions.

### Streaming image hash during inspect

Right now `inspect_image` returns `sha256: null`. Hashing a multi-GB image takes seconds. Do
it async with progress events so the UI shows "hashing…" before the burn even starts.

### Compressed image formats

`GzipReader`, `XzReader`, `Bzip2Reader`, `ZstdReader` — wrap the file in a decoder layer,
expose decompressed bytes. Format probe walks the extension chain (`foo.img.xz` → strip `.xz`
→ probe `foo.img`).

### qcow2 reader

Use `am-img-qcow2` crate with a thin `Read` adapter around `read_at`. See
`docs/rust-img-qcow2-improvements.md` for upstream notes.

### Theme switching

Theme palette is already in CSS `:root` vars (`--bone`, `--ink`, `--accent-1..4`, etc.). To
add a dark theme, define an alternate set of var values on `:root[data-theme="dark"]` and
add a theme picker in app options. Accent palette is already CSS-sourced via
`readThemeAccent()` so it follows the active theme automatically.
