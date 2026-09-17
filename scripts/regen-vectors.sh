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
cp "$ROOT"/conformance/oracle/*.go "$WORK/internal/abadaoracle/"

out="$ROOT/conformance/vectors/path.json"
tmp="$(mktemp "$ROOT/conformance/vectors/.path.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
(cd "$WORK" && ABADA_GENERATOR="grpc-gateway $GATEWAY_TAG, $(go version | cut -d' ' -f3)" \
  go run ./internal/abadaoracle) < "$ROOT/conformance/cases/path.json" > "$tmp"

# Real contracts: conformance/contracts/<name>.binpb, with <name>.source naming
# where the descriptor set came from.
for binpb in "$ROOT"/conformance/contracts/*.binpb; do
  [ -e "$binpb" ] || continue
  name="$(basename "$binpb" .binpb)"
  ctmp="$(mktemp "$ROOT/conformance/vectors/.$name.XXXXXX")"
  (cd "$WORK" && ABADA_GENERATOR="grpc-gateway $GATEWAY_TAG, $(go version | cut -d' ' -f3)" \
    go run ./internal/abadaoracle contract "$binpb" "$(cat "$ROOT/conformance/contracts/$name.source")") > "$ctmp"
  cout="$ROOT/conformance/vectors/contract-$name.json"
  if [ "${1:-}" = "--check" ]; then
    if ! diff <(grep -v '"generator"' "$cout") <(grep -v '"generator"' "$ctmp") > /dev/null; then
      rm -f "$ctmp"
      echo "$cout is stale: run scripts/regen-vectors.sh" >&2
      exit 1
    fi
    rm -f "$ctmp"
  else
    mv "$ctmp" "$cout"
    echo "wrote $cout"
  fi
done

# Errors: DefaultHTTPErrorHandler and the routing errors, over a real
# net/http server.
etmp="$(mktemp "$ROOT/conformance/vectors/.errors.XXXXXX")"
(cd "$WORK" && ABADA_GENERATOR="grpc-gateway $GATEWAY_TAG, $(go version | cut -d' ' -f3)" \
  go run ./internal/abadaoracle errors) < "$ROOT/conformance/cases/errors.json" > "$etmp"
eout="$ROOT/conformance/vectors/errors.json"
if [ "${1:-}" = "--check" ]; then
  if ! diff <(grep -v '"generator"' "$eout") <(grep -v '"generator"' "$etmp") > /dev/null; then
    rm -f "$etmp"
    echo "$eout is stale: run scripts/regen-vectors.sh" >&2
    exit 1
  fi
  rm -f "$etmp"
else
  mv "$etmp" "$eout"
  echo "wrote $eout"
fi

# JSON transcoding: conformance/cases/json-<name>.json holds bodies for the
# messages of contracts/<name>.binpb; the vectors are what grpc-gateway's default
# marshaler accepts and answers for each.
for jcases in "$ROOT"/conformance/cases/json-*.json; do
  [ -e "$jcases" ] || continue
  name="$(basename "$jcases" .json)"
  name="${name#json-}"
  jtmp="$(mktemp "$ROOT/conformance/vectors/.json-$name.XXXXXX")"
  (cd "$WORK" && ABADA_GENERATOR="grpc-gateway $GATEWAY_TAG, $(go version | cut -d' ' -f3)" \
    go run ./internal/abadaoracle json "$ROOT/conformance/contracts/$name.binpb" "$(cat "$ROOT/conformance/contracts/$name.source")") \
    < "$jcases" > "$jtmp"
  jout="$ROOT/conformance/vectors/json-$name.json"
  if [ "${1:-}" = "--check" ]; then
    if ! diff <(grep -v '"generator"' "$jout") <(grep -v '"generator"' "$jtmp") > /dev/null; then
      rm -f "$jtmp"
      echo "$jout is stale: run scripts/regen-vectors.sh" >&2
      exit 1
    fi
    rm -f "$jtmp"
  else
    mv "$jtmp" "$jout"
    echo "wrote $jout"
  fi
done

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
