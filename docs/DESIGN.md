# abada — design (v0.1)

`abada` is a Rust equivalent of Go's [grpc-gateway]: it reads the
`google.api.http` annotations of a `.proto` and produces a REST/JSON front for a
tonic gRPC service. Status: **pre-release** — the Progress table below says what
is implemented and how it is proven.

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

## Progress

| Piece | State | Proof |
|---|---|---|
| Path template parser (`abada::path::PathTemplate`) | done | 447 templates (47 hand-written, 400 from a fixed seed) parse, print and compile op for op like `internal/httprule` |
| Pattern matching and unescaping modes (`Pattern`) | done | replayed in the route vectors below |
| Routing: handler order, verbs, 404/405/400 (`Router`) | done | 632 routes, all four unescaping modes, identical outcome, handler and bindings to `runtime.ServeMux` |
| Request target → `Path`/`RawPath` (`RequestPath`) | done | the bytes `net/url` leaves unescaped are measured from Go, not transcribed |
| Rule extraction from a descriptor set (`abada_codegen::bindings`) | done | the `delonix.node.v1` contract: same 56 bindings as grpc-gateway, and 728 requests routed identically with all of them registered — see `docs/readiness/delonix-node-v1.md` |
| POST → GET path-length fallback (`X-HTTP-Method-Override`) | not started | — |
| Error responses: code → HTTP status, `google.rpc.Status` body, `WWW-Authenticate`, `Grpc-Metadata-*`/`Grpc-Trailer-*`, routing 404/501/400 (`abada::error`) | done, except details of a registered type | `conformance/vectors/errors.json`: 25 codes, 64 statuses and 29 routing errors answered by grpc-gateway's handlers behind a real `net/http` server, compared on the wire — see "Errors" below |
| JSON transcoding: choice of library | decided: `prost-reflect` | [ADR 0001](adr/0001-transcodificacao-json.md) — 29 bodies over the contract compared with grpc-gateway's default marshaler (`benches/json-transcode`), both directions timed |
| Query parameters, body, JSON in the runtime, request metadata, streaming | not started | — |

The conformance suite (`crates/abada/tests/conformance.rs`) was checked by
breaking the code on purpose: dropping the `/` quirk of the parser, the
registration order, the deep-wildcard tail, the escape table or the bare-verb
404 each makes it fail. The tail was NOT caught until cases with a request
shorter than the fixed tail were added.

Two known limits of the vectors: grpc-gateway walks a Go map for the 405
fallback, so abada tries other methods in registration order and the oracle
drops any case whose answer depends on that order; and binding values are
compared after Go's JSON encoding, which replaces invalid UTF-8.

## Errors

`abada::error::ErrorResponse` is what grpc-gateway's `DefaultHTTPErrorHandler`
and `DefaultRoutingErrorHandler` put on an HTTP/1.1 connection, read from
`runtime/errors.go`, `runtime/handler.go` and `runtime/mux.go` and measured by
`abadaoracle errors`. It depends on `http` (for `StatusCode`, `HeaderMap`,
`HeaderValue`) and nothing else: the tower service will speak `http` anyway,
and the header rules below are about what an `http` value can hold. There is
no `tonic` dependency yet; `Status`/`Any`/`ServerMetadata` are plain structs
the service layer will fill from a `tonic::Status`.

What the vectors fix, some of it against intuition:

- Body: `{"code":N,"message":"…","details":[…]}`, fields in that order,
  `details` always present (`[]` when empty), no trailing newline,
  `Content-Type: application/json`. protojson escapes only `"`, `\` and
  control characters (`\u001f`, lower-case), not `<>&`, DEL or U+2028.
- protojson adds a space after each comma or not depending on a hash of the
  Go binary (`internal/detrand`): grpc-gateway's bytes are not stable across
  builds. abada writes the compact form and the oracle removes that space.
- Code 0 has no message and no details (a gRPC client turns OK into a nil
  error). Codes outside 0–16 are 500 and keep their number in the body.
- `WWW-Authenticate` carries the message for code 16 only, even when empty.
- Response metadata becomes `Grpc-Metadata-<key>`. Trailer metadata becomes
  `Trailer: Grpc-Trailer-<Key>` plus trailers only when the request's first
  `TE` value, lower-cased the Go way, *contains* `trailers` (`notrailers`
  counts; `TRAİLERS` counts because Go lower-cases U+0130 to `i`).
- Header values are written as `net/http` writes them: CR/LF become spaces,
  surrounding blanks are trimmed. A value with another control byte is
  written raw by Go over HTTP/1.1; `http::HeaderValue` cannot hold it, so
  abada drops it — **a written deviation**, the same thing Go's HTTP/2
  server does. The test lists the two vectors that hit it.
- Routing: 404 is code 5 `Not Found`; a wrong method is code 12 `Method Not
  Allowed` with **501**, not 405; a path not starting with `/` is code 3
  `Bad Request`. A malformed escape is a 400 with code **2** (`Unknown`) and
  `malformed path escape "<strconv.Quote of the sequence>"` — and routing
  goes on: grpc-gateway writes one such body per candidate that hit the
  escape, then the 404/501 body, or the matched handler's output, into the
  same 400. `RouteOutcome::BadRequest` keeps every sequence and the outcome
  that followed so abada can write the same bytes. `strconv.IsPrint` is
  taken from Go as a table for the only runes a 3-byte sequence can hold.
- If marshalling fails, the body is the constant
  `{"code": 13, "message": "failed to marshal error message"}` with 500 and
  no metadata; `WWW-Authenticate` is still set for code 16.

**Known gap: details of a registered type.** protojson renders an `Any` by
looking its type up in the binary's registry (the host part of the type URL
is ignored), and fails — hence the fallback above — when it cannot. abada has
no registry yet — the library is now decided (see below) but not wired in — so it renders
an empty `Any` as `{}` and treats every other detail as unresolvable. That is
exact for types the Go binary does not link, and wrong for types it does:
`google.protobuf.Duration` or `google.rpc.Status` in `details` come out as
the 500 fallback instead of `{"@type":…,…}`. The four vectors that show it
are named in `crates/abada/tests/errors.rs`, which checks abada still falls
back on them so that closing the gap has to update the list. What the Go
registry contains depends on what the gateway binary links; the oracle's is
not a user's.

Not validated: streaming errors (`HTTPStreamError`, `{"error": …}` chunks),
`HTTPStatusError` from a user's routing handler, custom error handlers,
marshalers other than the default, `X-HTTP-Method-Override` errors, HTTP/2
framing, request targets `net/http` rejects before the mux (control bytes,
bad escapes), and invalid UTF-8 in a message (unreachable from a Rust
`String`; protojson would fall back).

## Decision: how JSON is transcoded

Settled by [ADR 0001](adr/0001-transcodificacao-json.md): **`prost-reflect`
`DynamicMessage`**, driven by the descriptor. `pbjson` was 2–21× cheaper per
operation on the `delonix.node.v1` bodies, but cannot read or write `Any` or
`FieldMask` as grpc-gateway does, and fails to encode unknown enum numbers.
Measured against grpc-gateway's default marshaler, not by preference; five
deviations of `prost-reflect` remain to be closed (see the ADR).

| Option | For | Against |
|---|---|---|
| `prost-reflect` `DynamicMessage` at runtime (**chosen**) | Works with any prost types, no serde derive on user code; mapping is driven by the descriptor, which is what grpc-gateway does | Runtime descriptor lookup cost; one extra decode/encode step |
| `pbjson`-generated serde impls | Static, fast | Forces a second codegen step on the user's types; fields must match exactly |

## First consumer

The `delonix.node.v1` contract in `angolardevops/delonix-runtime` annotates every
RPC with `google.api.http` (custom verbs, `PATCH` with `update_mask`, queries,
server-streamed watches). It is the first real contract the conformance suite
runs against, besides grpc-gateway's own examples.
