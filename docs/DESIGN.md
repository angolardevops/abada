# abada — design (v0.1)

`abada` is a Rust equivalent of Go's [grpc-gateway]: it reads the
`google.api.http` annotations of a `.proto` and produces a REST/JSON front for a
tonic gRPC service. Status: **proposed**, nothing implemented yet.

[grpc-gateway]: https://github.com/grpc-ecosystem/grpc-gateway

## Why another one

Rust already has crates in this space (`tonic-rest`, `tinc`,
`protoc-gen-grpc-gateway`, `connect2axum`). `abada` is justified only by these
four properties together, and each one is a test, not a claim:

1. **Behaviour-compatible with grpc-gateway.** Same path templates, query
   parameter mapping, body/response_body selection, error body and HTTP status
   mapping. Proven by a conformance suite that sends the SAME requests to a Go
   grpc-gateway and to `abada` over the SAME protos and compares the responses.
   A difference is a bug or a written, documented deviation.
2. **Two entry points, one generator.** `protoc-gen-abada` (for `protoc`/`buf`)
   and `abada-build` (for `build.rs`) both call `abada-codegen`, so they cannot
   drift.
3. **In-process and proxy.** The generated gateway calls either the tonic service
   implementation directly (no network hop, no second port) or a remote
   `tonic::transport::Channel`.
4. **A `tower::Service`, not a framework.** It mounts in axum, hyper or anything
   tower-based; axum integration is a convenience layer, not the core.

## Crates

| Crate | Role |
|---|---|
| `abada-codegen` | `FileDescriptorSet` → `HttpRule`s → Rust code. Pure: no I/O besides what it is handed |
| `protoc-gen-abada` | binary: `CodeGeneratorRequest` on stdin, `CodeGeneratorResponse` on stdout |
| `abada-build` | `build.rs` API in the style of `tonic-build` |
| `abada` | runtime used by the generated code: routing, transcoding, errors, streaming |

## v0.1 scope

**In:**
- `HttpRule`: `get`/`put`/`post`/`delete`/`patch`/`custom`, `body` (`"*"`, a field,
  or absent), `response_body`, `additional_bindings`.
- Path templates: `{field}`, `{field=segments/*}`, `*`, `**`, nested fields
  (`{parent.name}`), custom verbs (`/v1/{name}:start`).
- Fields that are neither in the path nor in the body become query parameters,
  including repeated and nested (`?filter.state=RUNNING&ids=a&ids=b`).
- JSON mapping: proto3 canonical JSON (well-known types, `int64` as string, enums
  by name, `FieldMask` as comma-separated camelCase).
- Errors: `google.rpc.Status` as JSON body; gRPC code → HTTP status with the same
  table as grpc-gateway.
- Metadata: `Authorization` and `Grpc-Metadata-*` headers forwarded; response
  metadata returned as `Grpc-Metadata-*` headers.
- Server streaming: newline-delimited JSON, one `{"result": …}` / `{"error": …}`
  object per message, as grpc-gateway does.
- Unix socket and TCP.

**Out (later, each with its own decision):**
- OpenAPI generation (until then: `protoc-gen-openapi` from gnostic).
- Client and bidirectional streaming over WebSocket.
- SSE as an alternative streaming encoding.
- Field behaviour validation (`REQUIRED`, `OUTPUT_ONLY`).

## Open decision: how JSON is transcoded

| Option | For | Against |
|---|---|---|
| `prost-reflect` `DynamicMessage` at runtime | Works with any prost types, no serde derive on user code; mapping is driven by the descriptor, which is what grpc-gateway does | Runtime descriptor lookup cost; one extra decode/encode step |
| `pbjson`-generated serde impls | Static, fast | Forces a second codegen step on the user's types; fields must match exactly |

Settle with a benchmark on a realistic message before the first release, not by
preference.

## First consumer

The `delonix.node.v1` contract in `angolardevops/delonix-runtime` annotates every
RPC with `google.api.http` (custom verbs, `PATCH` with `update_mask`, queries,
server-streamed watches). It is the first real contract the conformance suite
runs against, besides grpc-gateway's own examples.
