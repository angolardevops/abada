#!/usr/bin/env bash
# Regenerates conformance/vectors/*.json by running grpc-gateway itself over
# conformance/cases/*.json. With --check, fails if the committed vectors differ
# from what grpc-gateway answers today.
set -euo pipefail

GATEWAY_TAG="${GATEWAY_TAG:-v2.27.3}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${ABADA_REF_DIR:-$HOME/.cache/abada-ref}/grpc-gateway-$GATEWAY_TAG"

if [ ! -d "$WORK" ]; then
  git clone --quiet --depth 1 --branch "$GATEWAY_TAG" \
    https://github.com/grpc-ecosystem/grpc-gateway.git "$WORK"
fi
mkdir -p "$WORK/internal/abadaoracle"
cp "$ROOT/conformance/oracle/main.go" "$WORK/internal/abadaoracle/main.go"

out="$ROOT/conformance/vectors/path.json"
tmp="$(mktemp "$ROOT/conformance/vectors/.path.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
(cd "$WORK" && ABADA_GENERATOR="grpc-gateway $GATEWAY_TAG, $(go version | cut -d' ' -f3)" \
  go run ./internal/abadaoracle) < "$ROOT/conformance/cases/path.json" > "$tmp"

if [ "${1:-}" = "--check" ]; then
  # The Go version is recorded, not compared: the net/url rules it produced are.
  if ! diff <(grep -v '"generator"' "$out") <(grep -v '"generator"' "$tmp") > /dev/null; then
    echo "conformance/vectors/path.json is stale: run scripts/regen-vectors.sh" >&2
    exit 1
  fi
  echo "vectors match grpc-gateway $GATEWAY_TAG"
else
  mv "$tmp" "$out"
  echo "wrote $out"
fi
