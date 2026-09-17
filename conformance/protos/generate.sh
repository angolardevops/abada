#!/usr/bin/env bash
# Compiles conformance/protos into the two committed artifacts the JSON vectors
# use: the descriptor set abada's tests load, and the Go types the oracle links
# (grpc-gateway's body and response_body handling depends on the generated Go
# field types, which dynamicpb does not have). Not run by CI, which has no
# protoc; the oracle checks that the Go types and the descriptor set agree.
#
# Measured with: libprotoc 25.1, protoc-gen-go v1.36.10 (built from the
# google.golang.org/protobuf version grpc-gateway v2.27.3 requires).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROTOC="${PROTOC:-protoc}"
PROTOC_INCLUDE="${PROTOC_INCLUDE:-$(dirname "$(command -v "$PROTOC")")/../include}"
PROTOC_GEN_GO="${PROTOC_GEN_GO:-$HOME/.cache/abada-ref/bin/protoc-gen-go}"

cd "$ROOT/conformance/protos"
"$PROTOC" -I . -I "$PROTOC_INCLUDE" --include_imports \
  --descriptor_set_out="$ROOT/conformance/protos/abada-conformance-v1.binpb" \
  --plugin=protoc-gen-go="$PROTOC_GEN_GO" \
  --go_out="$ROOT/conformance/oracle" \
  --go_opt=module=github.com/grpc-ecosystem/grpc-gateway/v2/internal/abadaoracle \
  abada/conformance/v1/json.proto abada/conformance/v1/json_proto2.proto
echo "wrote conformance/protos/abada-conformance-v1.binpb and conformance/oracle/abadapb"
