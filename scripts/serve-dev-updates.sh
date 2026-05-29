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

echo "→ waiting for Caddy to initialize..."
for i in $(seq 1 15); do
    if docker exec "$CONTAINER" test -f /data/caddy/pki/authorities/local/root.crt 2>/dev/null; then
        break
    fi
    sleep 1
done

CA_CERT=$(mktemp /tmp/caddy-ca-XXXXXX.crt)
trap 'rm -f "$CA_CERT"' EXIT
docker exec "$CONTAINER" cat /data/caddy/pki/authorities/local/root.crt > "$CA_CERT"

case "$(uname -s)" in
    Darwin)
        if security find-certificate -c "Caddy Local Authority" /Library/Keychains/System.keychain &>/dev/null; then
            echo "→ Caddy local CA already trusted"
        else
            echo "→ Installing Caddy local CA to system keychain (requires sudo)..."
            sudo security add-trusted-cert -d -r trustRoot \
                -k /Library/Keychains/System.keychain "$CA_CERT"
            echo "→ CA installed and trusted"
        fi
        ;;
    Linux)
        echo "→ to trust the CA: sudo cp $CA_CERT /usr/local/share/ca-certificates/caddy-local.crt && sudo update-ca-certificates"
        ;;
esac

echo "→ serving https://localhost:$PORT/"
echo "→ stop with: docker rm -f $CONTAINER"
docker logs -f "$CONTAINER"
