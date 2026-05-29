#!/usr/bin/env bash
#
# Build the macOS .app + updater bundle, copy artifacts into ./dev-updates/,
# and emit the latest.json manifest the in-app updater fetches when the
# DEV channel is selected.
#
# Dev versioning: sources scripts/version.sh and calls calver_next_dev()
# which derives YYYY.M.D from the system clock and auto-increments the -N
# suffix based on dev-updates/latest.json (resets to 1 on a new day).
# tauri.conf.json is patched to the dev version for the build only and
# restored afterwards — no version commits needed for dev iterations.
#
# Requires TAURI_SIGNING_PRIVATE_KEY_PATH (or _KEY) pointing at the same
# minisign key whose pubkey is embedded in tauri.conf.json. The signed
# .app.tar.gz produced by `tauri build --bundles updater` carries an
# adjacent .sig file we splice into the manifest.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# shellcheck source=scripts/version.sh
source "$REPO_ROOT/scripts/version.sh"

DEV_DIR="$REPO_ROOT/dev-updates"
KEY_PATH="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.diskcutter-updater/updater.key}"

if [[ ! -f "$KEY_PATH" ]]; then
  echo "error: signing key not found at $KEY_PATH" >&2
  echo "       set TAURI_SIGNING_PRIVATE_KEY_PATH or generate one with:" >&2
  echo "       npx tauri signer generate -w \"$KEY_PATH\" --ci" >&2
  exit 1
fi
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_PATH")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

mkdir -p "$DEV_DIR"
VERSION="$(calver_next_dev "$DEV_DIR/latest.json")"

# Temporarily patch tauri.conf.json with the dev version; restore on exit.
TAURI_CONF="src-tauri/tauri.conf.json"
TAURI_CONF_BAK="${TAURI_CONF}.publish-bak"
cp "$TAURI_CONF" "$TAURI_CONF_BAK"
cleanup() { mv "$TAURI_CONF_BAK" "$TAURI_CONF"; }
trap cleanup EXIT

node -e "
  const fs = require('fs');
  const c = JSON.parse(fs.readFileSync('$TAURI_CONF'));
  c.version = '$VERSION';
  fs.writeFileSync('$TAURI_CONF', JSON.stringify(c, null, 2) + '\n');
"

# Detect arch/platform suffix Tauri uses in its target id.
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
case "$PLATFORM" in
  darwin-*)
    TARBALL="$(ls "$BUNDLE_ROOT/macos/"*.app.tar.gz 2>/dev/null | head -1 || true)"
    ;;
  linux-*)
    TARBALL="$(ls "$BUNDLE_ROOT/appimage/"*.AppImage.tar.gz 2>/dev/null | head -1 || true)"
    ;;
  windows-*)
    TARBALL="$(ls "$BUNDLE_ROOT/nsis/"*-setup.nsis.zip 2>/dev/null | head -1 || true)"
    ;;
esac
SIGFILE="${TARBALL}.sig"

if [[ -z "$TARBALL" || ! -f "$TARBALL" ]]; then
  echo "error: no updater artifact found under $BUNDLE_ROOT" >&2
  exit 1
fi
if [[ ! -f "$SIGFILE" ]]; then
  echo "error: signature file missing next to $TARBALL" >&2
  exit 1
fi

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
