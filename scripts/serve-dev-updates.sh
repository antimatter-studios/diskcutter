#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${PORT:-17780}"
DATA_DIR="${DATA_DIR:-$REPO_ROOT/dev-updates}"
CONTAINER="diskcutter-updates"
VOLUME="diskcutter-caddy-data"

mkdir -p "$DATA_DIR"

docker rm -f "$CONTAINER" 2>/dev/null || true

CADDYFILE=$(mktemp)
trap 'rm -f "$CADDYFILE"' EXIT
cat > "$CADDYFILE" <<EOF
localhost:$PORT {
    root * /srv
    file_server browse
    tls internal
}
EOF

docker run -d \
    --name "$CONTAINER" \
    -p "$PORT:$PORT" \
    -v "$DATA_DIR:/srv:ro" \
    -v "$VOLUME:/data" \
    -v "$CADDYFILE:/etc/caddy/Caddyfile:ro" \
    caddy:alpine

echo "→ serving https://localhost:$PORT/ (self-signed cert — app accepts it without CA install)"
echo "→ stop with: docker rm -f $CONTAINER"
docker logs -f "$CONTAINER"
