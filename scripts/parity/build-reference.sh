#!/usr/bin/env bash
# Builds the reference side of the performance gate: conformance/oracle/parity
# linked with the Go code protoc-gen-grpc-gateway v2.27.3 generates for the
# request contract, into $ABADA_REF_DIR/bin/parity. Needs the plugins that
# scripts/regen-vectors.sh builds (run it once first). Holds the shared
# checkout's lock while it works and leaves the checkout as it found it.
set -euo pipefail

TAG="${GATEWAY_TAG:-v2.27.3}"
GRPC_GO="${PROTOC_GEN_GO_GRPC_VERSION:-v1.5.1}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REF="${ABADA_REF_DIR:-$HOME/.cache/abada-ref}"
WORK="$REF/grpc-gateway-$TAG"
BIN="$REF/bin"
for p in "protoc-gen-go-$TAG" "protoc-gen-go-grpc-$GRPC_GO" "protoc-gen-grpc-gateway-$TAG"; do
  [ -x "$BIN/$p" ] || { echo "missing $BIN/$p: run scripts/regen-vectors.sh first" >&2; exit 1; }
done

build() {
  local dir="$WORK/internal/abadaparity"
  rm -rf "$dir"
  mkdir -p "$dir/protogen"
  trap 'rm -rf "$dir"' EXIT
  cp "$ROOT"/conformance/oracle/parity/*.go "$dir/"
  cp "$ROOT"/conformance/oracle/e2e/protogen/*.go "$dir/protogen/"
  (cd "$WORK" && go run ./internal/abadaparity/protogen \
    -set "$ROOT/conformance/contracts/abada-conformance-request-v1.binpb" \
    -importpath "github.com/grpc-ecosystem/grpc-gateway/v2/internal/abadaparity/gen/abadaconformancerequestv1" \
    -module github.com/grpc-ecosystem/grpc-gateway/v2 -out . \
    -plugin "$BIN/protoc-gen-go-$TAG" \
    -plugin "$BIN/protoc-gen-go-grpc-$GRPC_GO" \
    -plugin "$BIN/protoc-gen-grpc-gateway-$TAG")
  (cd "$WORK" && go build -o "$BIN/parity" ./internal/abadaparity)
}
mkdir -p "$REF"
export -f build
export WORK BIN ROOT TAG GRPC_GO
flock "$REF/.lock" bash -c build
echo "built $BIN/parity (grpc-gateway $TAG, $(go version | cut -d' ' -f3))"
