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
| POST → GET path-length fallback and `X-HTTP-Method-Override` (`abada::request::dispatch`, `Router::route_with_fallback`) | done | 23 `ServeMux` vectors, including the body `ParseForm` consumes and the 400 before routing — see "Requests" below |
| Error responses: code → HTTP status, `google.rpc.Status` body, `WWW-Authenticate`, `Grpc-Metadata-*`/`Grpc-Trailer-*`, routing 404/501/400, details through a type registry (`abada::error`) | done | `conformance/vectors/errors.json`: 25 codes, 64 statuses (4 with details of a registered type) and 29 routing errors answered by grpc-gateway's handlers behind a real `net/http` server, compared on the wire — see "Errors" below |
| JSON transcoding: choice of library | decided: `prost-reflect` | [ADR 0001](adr/0001-transcodificacao-json.md) — 28 bodies over the contract compared with grpc-gateway's default marshaler (`benches/json-transcode`), both directions timed |
| JSON codec (`abada::json::Marshaler`): `body: "*"`, `body: "<field>"`, `response_body`, well-known types, `Any` through a `TypeRegistry` | done; one written deviation (nesting limit) | 868 cases over `conformance/protos` (every scalar kind, maps with every key kind, oneofs, proto3 `optional`, proto2 with required fields and extensions, all well-known types) and the 28 contract bodies: accept/reject, the decoded message byte for byte, and the output byte for byte with and without `EmitUnpopulated` — see "JSON" below |
| Path values → request fields (`abada::request::RequestBinding`) | done | 259 vectors: every scalar kind, enums, repeated, well-known types, oneof, `{a.b}` and `**`, top-level (`runtime.<Kind>`, base 0) and nested (`PopulateFieldFromPath`, base 10) told apart |
| Query parameters (`DefaultQueryParser`, the generator's filter, `ParseForm`) | done; one written deviation (nesting limit) | 151 vectors, 9 of them the deviation — see "Query field paths" below |
| Body: `"*"` / `"<field>"` / absent, and its order against path and query | done, through an interim decoder | 90 vectors; the proto3 JSON codec behind it is `abada::json`'s (see "Requests") |
| `PATCH` field mask from the body (`FieldMaskFromRequestBody`) | done | 23 vectors |
| `response_body` selection | exposed (`RequestBinding::response_body`), not written | resolved against the response type; the writer (`Marshaler::encode_field`) is done; wiring it into a response is a later phase |
| Calling the RPC: one call path for in-process and proxy | decided: `tonic::client::Grpc<S: GrpcService>` | [ADR 0002](adr/0002-chamada-do-rpc-in-process-e-proxy.md) — proven identical for a unary call in-process and over a real loopback proxy, `benches/tonic-call-proto` |
| Incoming metadata: `Authorization`/`Grpc-Metadata-*` headers, `X-Forwarded-*`, `Grpc-Timeout` (`abada::service::metadata::incoming`) | done | 30 vectors over `runtime.AnnotateContext` — see "Metadata" below |
| A successful response: status, `Content-Type`, `Grpc-Metadata-*`/`Grpc-Trailer-*` out (`abada::service::response::Response::success`) | done, whole message only | 8 vectors over `runtime.ForwardResponseMessage` — see "Response" below; `response_body` (one field instead of the whole message) still not wired in |
| The call itself: `service::call::unary`, in-process, one RPC registered by hand | done, narrow scope | 2 integration tests (`crates/abada/tests/call.rs`), 3 mutations — see "Call" below; no proxy, no rich errors, no streaming |
| The `tower::Service` (`service::Gateway`), a `Router<Registration>` of hand-registered RPCs | done, narrow scope | 2 integration tests (`crates/abada/tests/gateway.rs`), 3 mutations — see "Gateway" below |
| `response_body`, proxy, streaming | not started | — |
| Security and performance parity with grpc-gateway | measured, **not at level** | property test over a 44-input hostile corpus: no panic and no abort (the 4 000- and 9 000-component query paths that aborted the process are refused, S2 closed); one FAIL on memory amplification of large repeated fields and maps (1.4–1.9× grpc-gateway's); most rows NOT VALIDATED — `docs/readiness/grpc-gateway-parity.md` |

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
`HeaderValue`) and on `abada::json` for the body: the tower service will speak
`http` anyway, and the header rules below are about what an `http` value can
hold. There is no `tonic` dependency yet; `Status`/`Any`/`ServerMetadata` are
plain structs the service layer will fill from a `tonic::Status`.

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

**Details.** protojson renders an `Any` by looking its type up in a
registry (the host part of the type URL is ignored) and fails — hence the
fallback above — when it cannot. abada renders the body through
`abada::json::Marshaler`, so what resolves is what its `TypeRegistry` holds:
the descriptor pool it was given, plus the well-known types and
`google.rpc.Status`. `ErrorResponse::from_status` uses the default registry
(well-known types and `google.rpc.Status` only); `from_status_with` takes the
marshaler of the service, whose registry holds the user's types. In Go the
registry is whatever the gateway binary links, so the two agree on the
user's types, the well-known types and `google.rpc.Status`, and can differ on
a type a Go binary links for another reason (`google.protobuf.FileDescriptorProto`
is resolvable in the oracle, not in abada's default registry). The test
checks, detail by detail, that the oracle's registry and abada's default one
resolve the same type URLs in the vectors.

Not validated: streaming errors (`HTTPStreamError`, `{"error": …}` chunks),
`HTTPStatusError` from a user's routing handler, custom error handlers,
marshalers other than the default, `X-HTTP-Method-Override` errors, HTTP/2
framing, request targets `net/http` rejects before the mux (control bytes,
bad escapes), and invalid UTF-8 in a message (unreachable from a Rust
`String`; protojson would fall back).

## JSON

`abada::json::Marshaler` is grpc-gateway's `runtime.JSONPb` as a
`runtime.ServeMux` builds it (`EmitUnpopulated: true`, `DiscardUnknown:
true`), over `prost-reflect`'s `DynamicMessage` without its serde support:
protojson's tokenizer, decoder and encoder (v1.36.10) and the parts of Go's
`encoding/json`, `strconv`, `encoding/base64` and `time` that reach the
bytes, ported function by function. The API the request and response layers
use:

```rust
impl Marshaler {
    pub fn new(registry: TypeRegistry) -> Self; // the ServeMux default
    pub fn decode(&self, desc: &MessageDescriptor, body: &[u8]) -> Result<DynamicMessage, JsonError>;
    pub fn decode_into(&self, msg: &mut DynamicMessage, body: &[u8]) -> Result<(), JsonError>;          // body: "*"
    pub fn decode_field(&self, msg: &mut DynamicMessage, field: &FieldDescriptor, body: &[u8]) -> Result<(), JsonError>; // body: "<field>"
    pub fn unmarshal_into(&self, msg: &mut DynamicMessage, json: &[u8]) -> Result<(), JsonError>;       // protojson.Unmarshal
    pub fn encode(&self, msg: &DynamicMessage) -> Result<Vec<u8>, JsonError>;
    pub fn encode_field(&self, msg: &DynamicMessage, field: &FieldDescriptor) -> Result<Vec<u8>, JsonError>; // response_body
}
```

What the vectors fix, some of it against intuition:

- **Order of body and path.** `protojson.Unmarshal` resets the message. The
  generated handler decodes the body first and then copies path and query
  values in; populating a request in the other order loses the path values.
  An empty (or blank) body leaves the message untouched; bytes after the
  first JSON value are ignored (`encoding/json`'s `Decoder`).
- **`body: "<field>"` is not protojson.** grpc-gateway decodes into the Go
  field with `encoding/json` unless the field is a message: an `int64` must be
  a JSON number, an enum must be a number (`1.9` is 1, `3e9` is
  `-2147483648` on amd64), bytes are padded standard base64 or an array of
  numbers, map keys go through `strconv` with base 0 (`0x10`, `1_000`). A
  message field is replaced; a list is replaced unless the body is `null`; a
  map is merged; a proto3 `optional` or proto2 field is set by an empty body
  and unset by `null`.
- **`response_body` is not protojson either** for non-messages: `int64` is a
  number, `<>&` are escaped, a nil `bytes` is `[]`, an unset message field is
  written as an empty message (so an unset `Timestamp` is `1970-01-01T00:00:00Z`
  and an unset `Value` fails).
- **Output.** Fields in declaration order, extensions after them by full
  name; map keys sorted (numbers numerically); `EmitUnpopulated` writes
  `null` for unset messages and proto2 scalars, but nothing for unset oneof
  members **and proto3 `optional` fields**; floats in Go's shortest form
  (`1e+21`, `1e-7`, `-0`, `0` for a whole `Struct` number); `Any` payloads
  decoded the way Go's generated types decode them (a known field with the
  wrong wire type is unknown, not an error).
- **Input.** Duplicate fields, a field given by both names, a duplicate map
  key and a oneof given twice are rejected — including after an unknown enum
  name was discarded for the first member. An integer may be written
  `1e3`, `"1e3"`, `100e-2`, and a number inside a string stops at the first
  delimiter (`"1,"` is 1). `Timestamp` goes through `time.Parse`, whose layout
  fallback accepts a one-digit hour, a comma before the fraction and a
  `+24:00` offset. Unknown enum names are discarded, unknown numbers kept.
- **Binary form of `Any.value`** built from JSON is Go's deterministic
  encoding (map entries sorted, oneof members last), compared byte for byte.

Written deviation: messages nest at most 100 levels (prost's own limit for
the binary form the request is sent in), where grpc-gateway allows 10 000; a
deeper body is rejected instead of risking the stack. `depth_101` is the
vector that shows it.

Every behaviour above was checked by breaking the code on purpose: 42
mutations (the null for unset messages, the oneof and `optional` skip,
duplicate rejection, exponent integers, float spelling, key sorting,
declaration order, escapes, base64 alphabets, the `time.Parse` fallback,
duration and `FieldMask` rules, `Any` with well-known types, enum discard,
the `encoding/json` paths of `body` and `response_body`, binary field order,
`NaN` bits, type URL hosts, the nesting limit, required fields and the
shortcut that skips them, extensions, wire-type leniency, the object-body
fast path, error details) each make `tests/json.rs` or `tests/errors.rs`
fail. One of them — oneof members written twice in binary — makes the tests
run away instead of failing (the payload doubles at every nested `Value`);
its tamer variant, oneof members first, fails cleanly.

Not validated: the typed hop into a tonic service (`DynamicMessage` →
prost type) — prost-reflect's binary encoder drops `-0.0` from a proto3
`double`, which the Go gateway sends (read in its code, not run); `body`/`response_body` on a oneof
member or a nested field path (abada returns `Unsupported`); group fields;
`UseProtoNames`, `UseEnumNumbers` and `EmitDefaultValues` beyond reading the
code; a float-to-enum conversion on non-amd64 Go; nesting between 101 and
10 000 levels; invalid UTF-8 inside a proto2 string (abada cannot hold it);
error message text (protojson's is randomised per build and not compared).

## Requests

`abada::request` is the request step of the handler `protoc-gen-grpc-gateway`
v2.27.3 generates, read in `internal/gengateway/template.go` and the runtime it
calls, and measured by `conformance/oracle/e2e`: that oracle does not model
grpc-gateway, it **generates** the Go gateway for each descriptor set in
`conformance/contracts` (`protoc-gen-go`, `protoc-gen-go-grpc` v1.5.1 and
`protoc-gen-grpc-gateway` built from the checkout, run by `protogen` without
protoc), registers it on a `runtime.ServeMux` and records what an in-process
gRPC server receives. The generated Go code is not committed: it is a few
thousand lines that `scripts/regen-vectors.sh` rebuilds in the cache on every
run, and committing it would let it drift from the plugins that made it.
`conformance/protos` holds a test proto for the kinds the contract lacks; it
compiles into `conformance/contracts/abada-conformance-request-v1.binpb` and is routed
like a contract (97 more bindings through the route vectors).

`crates/abada/tests/request.rs` replays 600 vectors (522 over the test proto,
78 over `delonix.node.v1`, one or more requests for each of its 56 bindings)
through `Router`, `dispatch` and `RequestBinding::decode`, and compares the
message as a value, or the HTTP status and every `google.rpc.Status` body
written. What the vectors fix, against intuition:

- **Order**: body, then path variables (a path value overwrites the body's),
  then the query. A body `"*"` rule never reads the query; any rule reads it
  only when the request has a top-level field that is neither the body field
  nor a path variable, so `GET /v1/node?%zz` is not an error.
- **Two parsers for path values.** A top-level variable goes through
  `runtime.Int32` & co, which parse integers with base 0 (`0x1f`, `017`,
  `1_000`); a nested one (`{a.b}`) and every query value go through
  `parseField`, base 10. A nested enum is parsed twice, and the second parse
  (base 0) can reject what the first accepted (`08`) or change it (`010`).
- Top-level `Timestamp`/`Duration` path values go through protojson (`1.5s`);
  nested and query ones through `time.Parse`/`time.ParseDuration` (`1h30m`),
  with Go's error text. `FieldMask`, `Struct`, `Value`, maps and messages are
  refused as path variables by the generator, and abada refuses them too.
- **Query**: a key is matched by proto or JSON name, filtered after being
  normalized — unless one component is unknown, then the original spelling is
  filtered, so `?fString.x=1` on a `{f_string}` route is an error. `m[k]=v`
  fills maps; repeated fields take one value per occurrence (no comma split);
  a set oneof refuses any other member; an unknown name is ignored but still
  creates the messages on its way (`nested.zzz=1` sets `nested`).
- **Body into a field** is `encoding/json` into the Go field, not protojson:
  a message field is set even by an empty body, an `int64` field refuses `"5"`,
  an enum field takes only numbers (truncated), map keys go through
  `runtime.Int64` & co, `null` clears a `proto3 optional`.
- **`PATCH`** with a `body` field and exactly one `FieldMask` field (any name)
  fills the mask from the body's keys, sorted, lists and maps as leaves,
  `Struct` keys followed blindly; `{}` gives the path `""`; an unknown key is a
  400. A mask in the query still overrides it afterwards.
- **`ServeMux`**: `X-HTTP-Method-Override` and the `POST` → `GET` fallback
  apply only to a `POST` whose first `Content-Type` is exactly
  `application/x-www-form-urlencoded`; both call `ParseForm`, which consumes
  the body (the form values become query values, a JSON body decodes as empty),
  and a form error is a 400 even on a path nothing serves. The generated
  handlers otherwise discard the body before their own `ParseForm`, so a form
  body never reaches a handler, but a malformed `Content-Type` is still a 400.
- Marshaler selection: with only the default marshaler registered,
  `Content-Type` and `Accept` change nothing (vectors with `application/xml`).
- A string field given bytes that are not UTF-8 reaches the gRPC client, which
  refuses to marshal it: `13`, 500. abada answers the same at the same point.

**Query field paths.** grpc-gateway walks `?a.b.c=v` in a loop and builds a
request as deep as the path names: its vectors answer `request` at 1 000 and at
5 000 levels (`query-depth-1000`, `query-depth-5000`). abada refuses a request
that would nest past **100 levels, the root counted, and a message-typed last
field one more** — the JSON codec's limit (`depth_101`); prost, which decodes
what abada sends, takes 101 (measured once in review, and read in prost's
source; no test pins it), so what abada builds from scalars and
well-known types is never refused downstream for depth (`Struct`/`Value`
excepted, below). The error is 400, code 3, `exceeded max recursion depth`.

Why a limit and not only a loop: the original port recursed once per component
and aborted the process between 2 000 and 4 000 components (between 500 and
1 000 unoptimised) on a 2 MiB worker stack — a 54 KB `GET`, under hyper's
header limit. Making the walk iterative did not fix it: the 4 000-level message
it then built is dropped and encoded by recursive code, and the process still
aborted (mutation below). The limit bounds every recursion that follows.

The limit applies where the walk *reaches* it, so the answers grpc-gateway gives
before that point are kept: a 5 000-component path with an unknown name third is
still ignored (`query-depth-5000-unknown-at-3`, 200) and one with a scalar third
is still "is not a message" (`-5000-not-a-message-at-3`). Past level 100 only
half of that survives. A scalar in the middle still answers "is not a message"
as Go does, told from the descriptors without building anything
(`query-depth-not-a-message-at-101`, `-150`, `-2500`, agreeing). An **unknown
name is the limit's 400**, where Go answers 200 having built the whole empty
chain (`query-depth-unknown-at-101`, `-150`).

The last field is counted too: a value of message type (a well-known type, the
element of a list, the value of a map — a map's entry is not a level of its
own) is one level more. At exactly 100 components a scalar, a list of scalars
and a map of scalars answer as Go does, errors included
(`query-depth-100-repeated-scalar-leaf`, `-100-map-scalar-leaf`,
`-100-scalar-parse-error`, `-100-scalar-too-many-values`, agreeing); a
`Timestamp`, singular or repeated, is level 101 and refused
(`-100-timestamp-leaf`, `-100-repeated-timestamp-leaf`), and so is a map of
messages, where Go's own error for that field (`unsupported message type`) is
replaced by the limit's (`-100-map-msg-leaf`; at 99 the two agree). A
last-field *error* on a path of 101 or more components is the limit's 400 and
has no vector: Go builds the message first, and which error wins between two
deep keys is Go's map order.

Deviating vectors, pinned by name in `tests/request.rs` (`DEEPER_THAN_LIMIT`,
asserted to still deviate): `query-depth-101`, `-102`, `-100-timestamp-leaf`,
`-1000`, `-5000`, `-100-repeated-timestamp-leaf`, `-100-map-msg-leaf`,
`-unknown-at-101`, `-unknown-at-150` — 9 of the 24 `query-depth-*` vectors. The
other 15 agree: `-99`, `-100`, `-99-timestamp-leaf`, `-99-repeated-timestamp-leaf`,
`-99-map-msg-leaf`, `-unknown-at-100`, `-5000-unknown-at-3`,
`-5000-not-a-message-at-3`, the three `-not-a-message-at-*`, and the four
`-100-…` scalar-leaf cases above. Not pinned: an unknown name mid-path past the
limit, and a list or map mid-path past the limit (read in the code and probed
by the security review, not vectors).

Mutations, each run and each failing the vector or test named: the original
recursion aborts in `tests/security.rs` (debug and release) and in
`tests/request.rs`; the loop without the limit aborts in `tests/security.rs`
in debug only (in release it fails by assertion: 101 levels answer 200) and in
`tests/request.rs` in both; limit 101, the leaf level not counted and a
`path.len()` check up front fail the vectors above; without the descriptor scan
the three `not-a-message` vectors fail; counting a map's entry as a level fails
`-100-map-scalar-leaf` when maps of scalars are counted too (that was the
defect a review found) and `-99-map-msg-leaf` when only maps of messages are;
treating every map as scalar fails `-100-map-msg-leaf`.

Three consequences. A path variable `{a.b.c…}` of 101 or more components, which
grpc-gateway serves, answers 400 on every request (found by reading, not
measured). A `Struct` or `Value` last field is filled by the JSON codec, and
for prost an object level is three messages (`Value`, `Struct`, the map entry)
and an array level two: `prost_types::Value` decodes at most 33 levels of object
or 50 of array (measured once in review; no test pins either), fewer the deeper the field sits, and past
that the backend's decoder answers 400 (measured through the gateway with
arrays: a `fValue` of 50 at level 1 is 200, at level 51 is 400; with 10 or 49
arrays at level 99 it is 400, and from 100 levels of JSON the codec's own 400
answers first). No crash. And a query is bounded in depth, not in cost — see the parity
report, S5, where the amplification (90× to 125× by construction, and reachable
through a form `POST` body as well as a URI) is a FAIL.

**The body decoder seam.** Messages are decoded through
`abada::request::BodyDecoder` (`protojson.Unmarshal` with `DiscardUnknown`),
also used for `Struct`/`Value` query values. `InterimSerdeDecoder`, on
prost-reflect's serde, stands in until `abada::json` replaces it; the vectors
avoid the bodies ADR 0001 lists as deviations. Everything around the codec —
`json.Decoder` reading only the first value, `encoding/json` into non-message
fields, `strconv`, `time`, `base64`, `url`, `mime` — is abada's and compared
text for text; the text of an error the decoder writes (`ErrorOrigin::Decoder`,
39 vectors) is compared on its code only.

Checked by breaking the code on purpose, 27 mutations, each failing the named
vector: nested integers in base 0, top-level integers in base 10, no second
enum parse, path before body, query filter off, names not normalized, query
always parsed, oneof check off, map key split at the first `[`, handler
reading the form body, `text` refused as a media type, message field left
unset on an empty body, trailing bytes refused, enum numbers rounded,
`optional` pointer not allocated, mask paths unsorted, no `""` path for `{}`,
`Struct` keys not followed, no `GET` fallback, override not upper-cased,
override form error ignored, Go's NaN payload, no URL-safe base64 retry,
one-digit hours refused, offset ignored, duration fraction dropped, invalid
UTF-8 accepted.

Not validated: proto2 requests (refused), `body: "a.b"` (refused: the
generated Go code dereferences a nil message), a top-level path variable in a
oneof another body member already set (the Go message names Go types; abada's
text differs), `Any` in a `PATCH` body that is not an object (Go panics),
`X-HTTP-Method-Override` values outside ASCII, forms over 10 MiB, the query
parameter limit (incoming metadata is its own section below), client
and bidirectional streaming, `repeated_path_param_separator` other than `csv`,
`allow_patch_feature=false`, and any query whose answer depends on Go map
order (two keys for one field, or one error among several): the oracle drops
such cases and abada takes keys in order of appearance.

## Metadata

`abada::service::metadata::incoming` is `runtime.AnnotateContext`
(`runtime/context.go`) with the default header matcher
(`runtime/mux.go`'s `DefaultHeaderMatcher`) — turning an incoming HTTP
request's headers, `Host` and peer address into the gRPC metadata pairs and
deadline a call carries. `crates/abada/tests/metadata.rs` replays 30 vectors
from `conformance/vectors/metadata.json`, produced by a new `metadata`
subcommand of `conformance/oracle` that calls the real function directly (no
wire exchange: `AnnotateContext` never writes an HTTP response). What the
vectors fix, against intuition:

- **`Grpc-Timeout` is checked before anything else**, and a malformed one
  (shorter than two characters, or a unit other than `H M S m u n`) aborts
  the whole call — it does not just get skipped the way an invalid metadata
  key or non-ASCII value later in the same function does.
- **`Authorization` becomes two pairs**, not one: an unconditional
  `authorization` copy (backwards-compatible, never validated), and a
  `grpcgateway-authorization` copy through the same permanent-header path
  every other forwarded header takes — which the non-ASCII-value check
  *does* apply to, so a non-ASCII `Authorization` value keeps the first pair
  and drops the second.
- **A `-Bin`-suffixed header's value skips the printable-ASCII check
  entirely** and is base64-decoded instead — padded (`StdEncoding`) when the
  header value's length is a multiple of 4, unpadded (`RawStdEncoding`)
  otherwise; a decode failure aborts the call, the only other case that does.
- **`X-Forwarded-Host`/`-For` are never matched like an ordinary header**:
  an existing `X-Forwarded-Host` wins over `Host`; `X-Forwarded-For`'s
  existing values (in header order) are joined with the peer address's IP
  (parsed off `RemoteAddr`) appended last.

Checked by breaking the code on purpose: the padded/unpadded threshold
inverted (not caught until vectors with actual padding characters existed —
`AGhlbGxv` needs none), the timeout units for `M`/`m` swapped, the
non-ASCII-value check removed, the `Authorization` special case removed, and
the peer address prepended instead of appended to `X-Forwarded-For` — each
made a named vector fail.

Not validated: a custom header matcher or metadata annotator (`ServeMuxOption`s
Go accepts; not in v0.1 scope), an invalid gRPC metadata *key* from such a
matcher (the default one only ever produces valid keys from a real header
name), a negative `Grpc-Timeout` count (Go hands `context.WithTimeout` an
already-past deadline; `Duration` cannot represent that, so abada clamps to
zero — untested, and not the same value), `RemoteAddr` as a bracketed IPv6
address, and the exact text of a base64 decode failure (Go's own decoder
message is not reproduced, only that decoding fails).

## Response

`abada::service::response::Response::success` is `ForwardResponseMessage`
(`runtime/handler.go`) with the default marshaler, for a message already
fully populated — status, `Content-Type`, and outgoing metadata, over the
same wire-exchange oracle machinery `errors.rs` uses (trailers need real
HTTP/1.1 framing). It shares header/trailer writing with
[`ErrorResponse`](#errors) through one function,
`crate::error::write_metadata`: grpc-gateway's own
`DefaultHTTPErrorHandler` and `ForwardResponseMessage` both call
`handleForwardResponseServerMetadata`/`handleForwardResponseTrailerHeader`,
so abada calls one function from both places too, instead of keeping the
rule written twice.

`crates/abada/tests/response.rs` replays 8 vectors from
`conformance/vectors/response.json`. The response message is
`google.rpc.Status`, reused only because it is a real proto message the Go
oracle already links — `ForwardResponseMessage` does not care what type it
forwards, and encoding correctness is `abada::json`'s to prove, not this
suite's. Checked by breaking the code on purpose: `Content-Type` left unset,
and trailers written unconditionally instead of only when the request
accepts them — each failed a named vector (mutating the shared
`write_metadata` also re-confirmed `errors.rs` still passes, since both
suites exercise the same function).

Not validated: `response_body` (rendering one field instead of the whole
message — `RequestBinding::response_body` and `Marshaler::encode_field`
exist, wiring them together does not yet), and everything the "Errors"
section above already lists as not validated for header/trailer writing in
general, since this is the same code.

## Call

`abada::service::call::unary` calls one RPC and turns the result into a
[`response::Response`](#response) or an
[`ErrorResponse`](#errors), given what `RequestBinding::decode` and
`metadata::incoming` already produced, through the path
[ADR 0002](adr/0002-chamada-do-rpc-in-process-e-proxy.md) decided:
`tonic::client::Grpc<S: GrpcService>` plus a
[`service::codec::DynamicCodec`](adr/0002-chamada-do-rpc-in-process-e-proxy.md)
(the ADR's prototype codec, moved into the runtime crate unchanged). Adding
`tonic` (pinned `=0.14.5`, `default-features = false`, `features =
["codegen"]`, per the ADR's own measurement) to `abada`'s dependencies —
and `AGENTS.md` §2 — is this piece's own consequence of that decision.

This is **not** conformance-tested against grpc-gateway: nothing in
`runtime/*.go` corresponds to `call::unary` itself — it is abada's own
assembly of pieces each already proven on their own (request→message in
`tests/request.rs`, metadata in `tests/metadata.rs`, a successful response
in `tests/response.rs`, the call mechanism itself in ADR 0002's prototype).
What had no prior proof was whether the assembly's wiring is correct, so
`crates/abada/tests/call.rs` is an integration test instead: a real,
hand-written tonic service (`tonic::server::Grpc` — the server-side
counterpart of the client wrapper, driven by the same `DynamicCodec` —
codegen-free, since nothing in this crate's build or tests may depend on
`tonic-build`/`tonic-prost-build` either, for the same MSRV reason as the
runtime dependency) that echoes a request field back, copies one metadata
value across, and fails with `NOT_FOUND` on request. Two tests: a
successful call carries a request metadata value in and a response
metadata value out; a failing call becomes the right HTTP status and body.
Three mutations, each failing the test it should: outgoing metadata never
copied, response metadata never read back, and the gRPC status code not
carried into the error.

Deliberately narrow, and named as such rather than left implicit:

- **In-process only.** ADR 0002 already proved in-process and proxy are the
  same code; this increment does not repeat that proof, it only wires the
  in-process side end to end.
- **One RPC, registered by hand.** No `Router`/`RequestBinding` integration,
  no `tower::Service` for `tonic::client::Grpc<S>` to sit behind — a caller
  builds the `Grpc`, the codec and the message itself. That assembly (a
  `Router` of bindings, each with its own descriptors, driving `call::unary`
  behind an actual `impl tower::Service<http::Request<B>>`) is the next
  piece, not this one.
- **Errors carry only a code and a message.** `tonic::Status::details()`
  (the `grpc-status-details-bin` trailer) is not decoded into
  `error::Any`s — every error in this suite has none.
- **No response trailers.** `tonic::Response::metadata()` (used here) is
  headers; ADR 0002 already found no direct tonic hook for a handler to set
  trailers on a successful response, so there is nothing to read back yet.
- **The `Grpc-Timeout` value is passed to `tonic::Request::set_timeout`,
  and nothing here proves it actually cuts a slow call short** — the fake
  server never runs long enough to test that, and no case tries.
- **`grpc.ready()` failing** (the underlying `tower::Service` refusing a
  new request — a transport-level condition, not a `tonic::Status`) becomes
  a plain `Unknown` error; `runtime.HTTPError`'s exact text for this case
  in Go was not read, so the message is abada's own, not grpc-gateway's.

## Gateway

`abada::service::Gateway<S>` is the `tower::Service<http::Request<ReqBody>>`
`runtime.ServeMux.ServeHTTP` corresponds to: it holds a
`path::Router<Registration>` (a [`Registration`] pairs one `RequestBinding`
with the full `/<package>.<Service>/<Method>` path `call::unary` sends the
request to) and the call target `S`, and on every request runs the same
steps this crate already proves in isolation — `RequestPath::parse`,
`request::dispatch`, `RequestBinding::decode`, `metadata::incoming`,
`call::unary`, and the response/error writers — in that order, then turns
whichever of `service::response::Response`/`error::ErrorResponse` comes
back into a real `http::Response`. `ResponseBody` (its own small
`http_body::Body`) writes one data frame and, only when there are any, one
trailers frame — nothing more general is needed, since `abada` writes it
and nothing else has to read it back.

Proven by two integration tests
(`crates/abada/tests/gateway.rs`; like `call.rs`, no grpc-gateway
equivalent exists for this assembly, so this is integration, not
conformance): a `GET` request is routed through a real `RequestBinding`
built from the `abada-conformance-request-v1` contract's `PathService.Top`
method, reaches a real, hand-written, codegen-free tonic service (the same
kind `call.rs` and the ADR 0002 prototype use), and its response comes back
as a real `http::Response` — status, a response metadata header, and the
body all checked; separately, an unrouted request answers grpc-gateway's
own 404 (`ErrorResponse::for_route`, already proven in `errors.rs`, reused
as-is). Three mutations, each failing the test it should: the routed
response's body silently dropped, and — this is what the first version of
this test did *not* catch, because the test only checked a *response*
metadata header the server sets unconditionally rather than a *request*
metadata value it must be given to echo back — the incoming HTTP request's
metadata never reaching `call::unary` at all. Fixed by making the fake
server echo one request metadata value into its response, exactly so a
"metadata never wired through" mutation has something to break.

**A debugging note worth keeping**: building the codegen-free fake server
for `gateway.rs` surfaced a real gap in the "swap request/decode" rule
`call.rs`'s own doc comment already states — its own fake server had the
encode/decode descriptors in the wrong order for a *server* codec (should
be response, request — the opposite of the client's), silently correct
only because that test's request and response happened to share one
message type (`google.rpc.Status`). Fixed in both files (`docs/adr/
0002-chamada-do-rpc-in-process-e-proxy.md`'s prototype is not affected: it
never builds a server-side codec, only a generated one). A second,
Rust-specific finding: a function returning `-> impl SomeTrait` where
`SomeTrait` has an associated type does not, on its own, let the compiler
prove that associated type is `Send`, even when the concrete underlying
type's `Future` genuinely is — `Gateway`'s `impl tower::Service` requires
`S::Future: Send`, and satisfying it needed the return type written as
`impl GrpcService<Body, ..., Future: Send> + Clone` (an associated-type
bound directly in the `impl Trait`), not just returning a future that
happens to be `Send`.

Not validated: everything "Call" above already lists (proxy, rich errors,
`response_body`, streaming, real timeout cutoff), a request body `abada`'s
own `http_body::Body` reading fails on (falls back to a plain `Internal`
error; no case exercises a body that actually errors while being read),
and more than one `Registration` in the same `Router` (nothing here tests
that two real RPCs coexist correctly, only that one does and an unmatched
path answers 404).

## Decision: how JSON is transcoded

Settled by [ADR 0001](adr/0001-transcodificacao-json.md): **`prost-reflect`
`DynamicMessage`**, driven by the descriptor. `pbjson` was 2–21× cheaper per
operation on the `delonix.node.v1` bodies, but cannot read or write `Any` or
`FieldMask` as grpc-gateway does, and fails to encode unknown enum numbers.
Measured against grpc-gateway's default marshaler, not by preference. The
five deviations of `prost-reflect`'s serde support are closed by not using
it: see "JSON" below and the ADR's follow-up.

| Option | For | Against |
|---|---|---|
| `prost-reflect` `DynamicMessage` at runtime (**chosen**) | Works with any prost types, no serde derive on user code; mapping is driven by the descriptor, which is what grpc-gateway does | Runtime descriptor lookup cost; one extra decode/encode step |
| `pbjson`-generated serde impls | Static, fast | Forces a second codegen step on the user's types; fields must match exactly |

## First consumer

The `delonix.node.v1` contract in `angolardevops/delonix-runtime` annotates every
RPC with `google.api.http` (custom verbs, `PATCH` with `update_mask`, queries,
server-streamed watches). It is the first real contract the conformance suite
runs against, besides grpc-gateway's own examples.
