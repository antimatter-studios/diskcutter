#!/usr/bin/env bash
#
# Build the macOS .app + updater bundle and publish it to the local dev update
# server via HTTPS PUT.  The server (npm run updates:serve) can be running from
# any worktree — publishing just needs its URL.
#
# Dev versioning: sources scripts/version.sh and calls calver_next_dev().
# Version format: YYYY.M.D-N[-branch-slug]  (N is global across all branches
# for the day; branch slug is auto-derived from the current git branch).
# tauri.conf.json is patched to the dev version for the build only and
# restored afterwards — no version commits needed.
#
# Requires TAURI_SIGNING_PRIVATE_KEY_PATH pointing at the minisign key whose
# pubkey is embedded in tauri.conf.json.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# shellcheck source=scripts/version.sh
source "$REPO_ROOT/scripts/version.sh"

PORT="${PORT:-17780}"
HOST="${HOST:-localhost}"
SERVER="https://${HOST}:${PORT}"

KEY_PATH="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.diskcutter-updater/updater.key}"

if [[ ! -f "$KEY_PATH" ]]; then
  echo "error: signing key not found at $KEY_PATH" >&2
  echo "       set TAURI_SIGNING_PRIVATE_KEY_PATH or generate one with:" >&2
  echo "       npx tauri signer generate -w \"$KEY_PATH\" --ci" >&2
  exit 1
fi
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_PATH")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

# Check the update server is reachable before starting a long build.
if ! curl -sk -o /dev/null --max-time 2 "$SERVER/"; then
  echo "error: dev update server not responding at $SERVER" >&2
  echo "       start it with: npm run updates:serve" >&2
  exit 1
fi

# Fetch current updates.json from server so calver can determine next N.
TMPDIR_PUBLISH="$(mktemp -d)"
curl -sk "$SERVER/updates.json" -o "$TMPDIR_PUBLISH/updates.json" 2>/dev/null || true

VERSION="$(calver_next_dev "$TMPDIR_PUBLISH/placeholder.json")"

# Temporarily patch tauri.conf.json with the dev version; restore on exit.
TAURI_CONF="src-tauri/tauri.conf.json"
TAURI_CONF_BAK="${TAURI_CONF}.publish-bak"
cp "$TAURI_CONF" "$TAURI_CONF_BAK"
cleanup() {
  mv "$TAURI_CONF_BAK" "$TAURI_CONF"
  rm -rf "$TMPDIR_PUBLISH"
}
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
SIG="$(cat "$SIGFILE")"
PUBDATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

URL="${SERVER}/${TARBALL_BASENAME}"

echo "→ uploading $TARBALL_BASENAME"
curl -sk -X PUT "$SERVER/$TARBALL_BASENAME"     -T "$TARBALL"  -o /dev/null
curl -sk -X PUT "$SERVER/$TARBALL_BASENAME.sig" -T "$SIGFILE"  -o /dev/null

PLATFORM_JSON="{
    \"${PLATFORM}\": {
      \"signature\": \"${SIG}\",
      \"url\": \"${URL}\"
    }
  }"

VERSION_JSON="$TMPDIR_PUBLISH/${VERSION}.json"
cat > "$VERSION_JSON" <<EOF
{
  "version": "${VERSION}",
  "notes": "dev build ${VERSION}",
  "pub_date": "${PUBDATE}",
  "platforms": ${PLATFORM_JSON}
}
EOF

# Merge new entry into updates.json (already fetched into TMPDIR_PUBLISH).
node -e "
  const fs = require('fs');
  let u = { dev: [] };
  try { u = JSON.parse(fs.readFileSync('$TMPDIR_PUBLISH/updates.json', 'utf8')); } catch (_) {}
  if (!Array.isArray(u.dev)) u.dev = [];
  const entry = JSON.parse(fs.readFileSync('$VERSION_JSON', 'utf8'));
  u.dev.unshift(entry);
  fs.writeFileSync('$TMPDIR_PUBLISH/updates.json', JSON.stringify(u, null, 2) + '\n');
"

# catalog.json — legacy compat for clients on <=2026.5.29-3.
node -e "
  const fs = require('fs');
  const u = JSON.parse(fs.readFileSync('$TMPDIR_PUBLISH/updates.json', 'utf8'));
  const cat = { versions: u.dev.map(e => ({ version: e.version, notes: e.notes, pub_date: e.pub_date })) };
  fs.writeFileSync('$TMPDIR_PUBLISH/catalog.json', JSON.stringify(cat, null, 2) + '\n');
"

echo "→ uploading manifests"
curl -sk -X PUT "$SERVER/${VERSION}.json" -T "$VERSION_JSON"                   -o /dev/null
curl -sk -X PUT "$SERVER/latest.json"     -T "$VERSION_JSON"                   -o /dev/null
curl -sk -X PUT "$SERVER/updates.json"    -T "$TMPDIR_PUBLISH/updates.json"    -o /dev/null
curl -sk -X PUT "$SERVER/catalog.json"    -T "$TMPDIR_PUBLISH/catalog.json"    -o /dev/null

echo "→ published $VERSION"
echo "→ $SERVER/updates.json"
