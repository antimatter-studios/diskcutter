# Contributing to Disk Cutter

Thanks for poking at the codebase. This guide is for people who want to land
a change — it covers the project layout, the local dev loop, and the bar a
PR has to clear before it can merge. It is deliberately opinionated; for the
architectural deep dive see [`docs/architecture.md`](docs/architecture.md).

## Project layout

Disk Cutter is a Tauri 2 desktop app: a React frontend talks to a Rust
backend via Tauri's IPC, and the Rust side owns all of the dangerous bits
(elevated burn helper, raw device I/O, sqlite history, macOS DiskArbitration
FFI). Tests live next to the code they cover for Rust, plus a small
frontend Vitest suite at the repo root.

```
/
├── src/                 — React frontend (Vite + react-i18next)
├── src-tauri/           — Rust backend (Tauri 2 + sqlite + DiskArbitration FFI)
│   ├── src/             — modules: pipeline, readers, writers, helper, disks, db, disk_arb...
│   ├── tests/           — integration tests
│   ├── examples/        — cargo example binaries (benchmark.rs)
│   └── migrations/      — SQL migrations applied on startup
├── tests/               — frontend Vitest suite
├── docs/                — architecture, performance, CHANGELOG, ROADMAP
├── scripts/             — dev tooling (i18n parity, install hooks)
└── .githooks/           — pre-commit (cargo fmt, clippy, i18n parity)
```

## Local dev setup

One-time per clone:

```sh
npm install                         # also runs scripts/install-hooks.sh via "prepare"
```

If you ever need to re-register the hook manually:

```sh
bash scripts/install-hooks.sh       # registers .githooks/pre-commit
```

Day-to-day:

```sh
npx tauri build                                        # release .app/.dmg
cargo check --manifest-path src-tauri/Cargo.toml       # fast Rust iteration
cargo test  --manifest-path src-tauri/Cargo.toml       # backend tests
npm test                                               # frontend Vitest
npm run build                                          # frontend production bundle
```

The release `.app` bundle is the only supported run-mode. There is no
dev server — `tauri dev` / `vite` have been removed from the project
configuration on purpose; iterate by rebuilding.

## Coding standards

### Rust

- `cargo fmt` is non-negotiable. The pre-commit hook runs `cargo fmt --check`.
- `cargo clippy --all-targets -- -D warnings` must be clean. The hook
  enforces this too.
- Modules stay small and single-purpose. If a `*.rs` file grows past ~500
  lines or starts mixing concerns (e.g. IPC + business logic), split it.
- Prefer `?` over `.unwrap()` outside tests. The helper binary in
  particular must never panic — it runs as root.

### Frontend

- Minimal comment density. Extract pure helpers (see `app-derive.js`,
  `format.js`, `job-reducers.js`) so logic is unit-testable without
  spinning up a DOM.
- Brutalist palette: all colours and most spacings live in `:root` CSS
  variables in `src/styles.css`. Don't hardcode hex values in components.
- Keep components in `src/components.jsx` colocated unless they grow large
  enough to justify their own file.

### i18n

Every user-facing English string lives in `src/i18n/locales/en.json` and
**must** have a counterpart in `de.json` and `es.json`. The pre-commit
hook runs `scripts/check-i18n.mjs`, which fails the commit on any drift
(missing keys in one locale, orphan keys in another). If you're adding a
new string, add it to all three locales in the same commit.

## Commit style

- Conventional commits: `feat:`, `fix:`, `refactor:`, `test:`, `chore:`,
  `docs:`, `perf:`. Scopes are optional and lowercase, e.g.
  `feat(sparse): hole-punching writer`.
- One topic per commit. If your diff touches three unrelated things, that
  is three commits.
- The pre-commit hook will block fmt/clippy/i18n regressions. Don't bypass
  it with `--no-verify` unless you're committing a deliberate WIP on a
  feature branch.
- **No AI attribution lines** (`Co-Authored-By: Claude` etc). House style.

## Test contract before opening a PR

A PR is ready when:

- `cargo test --manifest-path src-tauri/Cargo.toml` passes
- `npm test` passes
- `npm run build` succeeds
- `node scripts/check-i18n.mjs` is clean
- `cargo clippy --all-targets -- -D warnings` is clean (at minimum for
  files you touched)

CI runs the same gate — see [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

## Architecture pointer

For the deep dive — process model, the elevated helper handshake,
DiskArbitration usage, writer implementations, pipeline buffer
discipline — read [`docs/architecture.md`](docs/architecture.md). Don't
skip it before making structural changes; the constraints aren't
obvious from the code alone.

## Adding a new image-reader format

Image readers live under `src-tauri/src/readers/` and plug into a registry
in `readers/mod.rs`.

1. Create `src-tauri/src/readers/<format>.rs` with a struct that
   implements `ImageReader` (a `Read + Send` newtype around your decoder)
   and a `<Format>ReaderFactory` that implements `ImageReaderFactory`.
2. `ImageReaderFactory::probe(&self, path: &Path) -> Option<ImageInfo>`
   is the discriminator. The convention is: cheap extension check first,
   then read the first ~16 bytes and confirm the magic. Return `None` if
   either is inconclusive — never panic, never read the whole file.
3. Register the factory in `readers/mod.rs` inside the `Registry::new`
   constructor. Order matters only for ambiguous cases; the first
   factory whose `probe()` returns `Some` wins.
4. Add a test in `src-tauri/tests/burn_integration.rs` that builds a
   tiny fixture image of your format and runs it through the full
   pipeline against a tempfile target. Pattern-match on the existing
   gzip/bzip2 cases for shape.
5. Mirror the unit tests in `<format>.rs` itself for the `probe()` shape:
   recognises-by-extension, recognises-by-magic-on-renamed-file,
   rejects-garbage, returns-None-for-missing-file.

## Adding a new perf tuneable to Prefs

Performance knobs flow from React state through the elevated helper's CLI
into the pipeline. To add a new one:

1. Define the key and default in `src/components.jsx` under
   `PREFS_DEFAULTS`, and add the field row in the `PrefsView` form.
2. Parse the matching CLI argument in `src-tauri/src/helper.rs` (the
   `parse_args` function near the top).
3. Propagate the value from the UI through to the helper invocation in
   `src-tauri/src/disks.rs::spawn_elevated_burn` — add the new CLI flag
   to the command builder.
4. Document the knob in `docs/PERFORMANCE.md`: what it does, the default,
   the trade-off, and any measurement notes from your tuning runs.

Don't forget i18n labels for the new field — `src/i18n/locales/*.json`
under the `prefs.*` namespace.
