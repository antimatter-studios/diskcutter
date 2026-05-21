#!/usr/bin/env bash
#
# Build the macOS .app + updater bundle, copy artifacts into ./dev-updates/,
# and emit the latest.json manifest the in-app updater fetches when the
# DEV channel is selected.
#
# Requires TAURI_SIGNING_PRIVATE_KEY_PATH (or _KEY) pointing at the same
# minisign key whose pubkey is embedded in tauri.conf.json. The signed
# .app.tar.gz produced by `tauri build --bundles updater` carries an
# adjacent .sig file we splice into the manifest.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

DEV_DIR="$REPO_ROOT/dev-updates"
KEY_PATH="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.diskcutter-updater/updater.key}"

if [[ ! -f "$KEY_PATH" ]]; then
  echo "error: signing key not found at $KEY_PATH" >&2
  echo "       set TAURI_SIGNING_PRIVATE_KEY_PATH or generate one with:" >&2
  echo "       npx tauri signer generate -w \"$KEY_PATH\" --ci" >&2
  exit 1
fi
# Tauri CLI reads TAURI_SIGNING_PRIVATE_KEY as the literal key contents
# (the _PATH variant is documented but currently ignored by the bundler).
# Inline the file contents so the bundler picks up the key.
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_PATH")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

VERSION="$(node -p "require('./package.json').version")"

# Detect arch suffix Tauri uses in its target id.
case "$(uname -m)" in
  arm64|aarch64) ARCH="aarch64" ;;
  x86_64)        ARCH="x86_64" ;;
  *) echo "unsupported arch: $(uname -m)" >&2; exit 1 ;;
esac

case "$(uname -s)" in
  Darwin)  PLATFORM="darwin-${ARCH}" ;;
  Linux)   PLATFORM="linux-${ARCH}" ;;
  MINGW*|MSYS*|CYGWIN*) PLATFORM="windows-${ARCH}" ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

echo "→ building updater bundle for $PLATFORM @ $VERSION"
npx tauri build --bundles app

BUNDLE_ROOT="$REPO_ROOT/src-tauri/target/release/bundle"
TARBALL=""
SIGFILE=""
case "$PLATFORM" in
  darwin-*)
    TARBALL="$(ls "$BUNDLE_ROOT/macos/"*.app.tar.gz 2>/dev/null | head -1 || true)"
    SIGFILE="${TARBALL}.sig"
    ;;
  linux-*)
    TARBALL="$(ls "$BUNDLE_ROOT/appimage/"*.AppImage.tar.gz 2>/dev/null | head -1 || true)"
    SIGFILE="${TARBALL}.sig"
    ;;
  windows-*)
    TARBALL="$(ls "$BUNDLE_ROOT/nsis/"*-setup.nsis.zip 2>/dev/null | head -1 || true)"
    SIGFILE="${TARBALL}.sig"
    ;;
esac

if [[ -z "$TARBALL" || ! -f "$TARBALL" ]]; then
  echo "error: no updater artifact found under $BUNDLE_ROOT" >&2
  exit 1
fi
if [[ ! -f "$SIGFILE" ]]; then
  echo "error: signature file missing next to $TARBALL" >&2
  exit 1
fi

mkdir -p "$DEV_DIR"
# Normalize the artifact name — Tauri's default bundle name has spaces
# ("Disk Cutter.app.tar.gz") which produce ugly URLs the updater client
# tolerates but no one wants to look at.
case "$PLATFORM" in
  darwin-*)  EXT="app.tar.gz" ;;
  linux-*)   EXT="AppImage.tar.gz" ;;
  windows-*) EXT="nsis.zip" ;;
esac
TARBALL_BASENAME="diskcutter-${VERSION}-${PLATFORM}.${EXT}"
cp -f "$TARBALL" "$DEV_DIR/$TARBALL_BASENAME"
cp -f "$SIGFILE" "$DEV_DIR/$TARBALL_BASENAME.sig"
SIG="$(cat "$SIGFILE")"
PUBDATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Tauri tolerates the same artifact URL relative to whatever host serves
# the manifest; we always emit an absolute URL so a different consumer
# (e.g. another machine on the LAN) can still resolve it via the same
# server. Keep PORT in sync with serve-dev-updates default.
PORT="${PORT:-17780}"
HOST="${HOST:-localhost}"
URL="http://${HOST}:${PORT}/${TARBALL_BASENAME}"

MANIFEST="$DEV_DIR/latest.json"
cat > "$MANIFEST" <<EOF
{
  "version": "${VERSION}",
  "notes": "dev build ${VERSION}",
  "pub_date": "${PUBDATE}",
  "platforms": {
    "${PLATFORM}": {
      "signature": "${SIG}",
      "url": "${URL}"
    }
  }
}
EOF

echo "→ wrote $MANIFEST"
echo "→ serve with: npm run updates:serve"
