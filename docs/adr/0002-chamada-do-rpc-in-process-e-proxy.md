# ADR 0002 — How abada calls the RPC

- Status: accepted, 2026-09-19
- Settles: `docs/DESIGN.md`'s "Request metadata, calling the RPC, streaming |
  not started" row — specifically, how a unary RPC is actually invoked,
  in-process and by proxy, once `RequestBinding::decode` has produced a
  request message.
- Evidence: `benches/tonic-call-proto` (its own Cargo workspace, not a member
  of the main one — see Consequences), branch `proto/tonic-unary-call`.

## Context

abada's third design property is that its call path is abstract over "call
the service implementation" (in-process) and "call a Channel" (proxy) —
nothing in routing, transcoding or errors may assume a network hop. Before
this ADR, nothing called an RPC at all: `abada::request::dispatch` and
`RequestBinding::decode` produce a `prost_reflect::DynamicMessage`, and
`docs/DESIGN.md` said plainly "Request metadata, calling the RPC, streaming |
not started".

grpc-gateway v2.27.3 was read to see how Go resolves the in-process/proxy
split (`protoc-gen-grpc-gateway/internal/gengateway/template.go`,
`runtime/{handler,context,mux,errors}.go`). Its `runtime` package is entirely
generic (`proto.Message`, `grpc.ClientConnInterface`, `metadata.MD`,
`status.Status`), with no coupling to any generated service — that coupling
lives only in the per-service `.pb.gw.go` file the plugin emits. The
`local_request_...` (in-process) and `request_...` (proxy) generated
functions share all request-population code and differ only in the call
itself and in how response metadata is captured: the proxy path uses the
`grpc.Header`/`grpc.Trailer` call options; the in-process path installs a
fake `grpc.ServerTransportStream` into the context so the server
implementation's own `grpc.SetHeader`/`SendHeader`/`SetTrailer` calls are
captured and read back afterwards.

Rust has no equivalent of "install a fake `ServerTransportStream` in a
context". tonic's own architecture suggested a different mechanism:
`tonic::client::Grpc<S>` is generic over `S: tonic::client::GrpcService<
tonic::body::Body>`, and a generated `<Service>Server<T>` already implements
that trait directly — it is, after all, what `tonic::transport::Server`
mounts — the same trait a `tonic::transport::Channel` implements for a real
connection. If true, in-process and proxy become the exact same call in
abada, satisfying property 3 by construction instead of by two
hand-synchronised implementations.

## Options

- **(a) One call path via `tonic::client::Grpc<S: GrpcService<tonic::body::
  Body>>`.** In-process wraps the generated `<Service>Server<T>` directly,
  with no socket; proxy wraps a `tonic::transport::Channel` (or any other
  `GrpcService` implementation, e.g. a load balancer). Both go through the
  same `unary()` call.
- **(b) Two separate call paths**, mirroring grpc-gateway's own two generated
  functions: an in-process path that calls the server trait's method
  directly, with no HTTP framing at all, and a proxy path built on
  `tonic::transport::Channel`. Closer to a line-by-line port of the Go
  generator; more code, and two places for metadata and error handling to
  drift apart.

(b) was not built. (a) is directly testable, and if it holds it removes an
entire duplicated implementation and its drift risk; the "settle with
evidence" rule applies to whether (a) actually works, not to a cost
comparison between two working implementations that were never both written.

Message encoding was the second open question. abada has no per-RPC
generated Rust types yet (`protoc-gen-abada`/`abada-build` are unwritten), so
the call cannot use the typed `prost::Message` a real user's generated client
would use — it has to move a `prost_reflect::DynamicMessage`, the same type
`RequestBinding::decode` already produces, across the tonic client boundary.
tonic's `Codec` trait is generic over `Encode`/`Decode`, so a small
hand-written codec driven by a `MessageDescriptor` (encode via the message's
own descriptor; decode by building `DynamicMessage::new(descriptor)` and
merging the wire bytes) was tried instead of requiring a generated type.

## How it was measured

`benches/tonic-call-proto` (its own Cargo workspace — see Consequences): a
minimal `.proto` (one unary RPC, a request with a string, an int32 and a
`map<string, string>`), compiled with `tonic-prost-build` to get a real
generated `EchoServer<T>` trait/struct, and a fixed, hand-written
implementation of it that reads a request metadata key and sets a response
metadata key.

Two calls, both built from a `DynamicMessage` populated the way
`RequestBinding::decode` would populate one, both driven by the same
`DynamicCodec`:

- **in-process**: `tonic::client::Grpc::new(EchoServer::new(EchoImpl))` — no
  socket, no `tonic::transport::Server`.
- **proxy**: the same service served by a real `tonic::transport::Server` on
  a loopback `TcpListener`, called through a `tonic::transport::Channel`
  connected to it.

Both assert on the decoded response message fields and on a response
metadata key the server set via `Response::metadata_mut()`.

## Results

Both paths returned identical decoded response fields (`echoed`, `count`)
and the same response metadata value, from the same `DynamicCodec`, the same
request-construction code, and the same `tonic::client::Grpc::unary` call —
differing only in what `S` is. `cargo test`, `cargo clippy --all-targets --
-D warnings` and `cargo fmt --check` are clean on the prototype crate.

One structural finding while measuring: `tonic-prost-build` 0.14.6 declares
`rust-version = 1.88`, above the `1.85` abada's workspace promises. Adding it
as a normal build-dependency of a workspace member would break the `msrv` CI
job for the whole repository — not because `tonic` itself (runtime,
`rust-version = 1.75`) needs it, but because the *codegen* crate does. The
prototype's own `Cargo.toml` declares its own `[workspace]` and is
`exclude`d from the root one specifically to keep this from touching abada's
MSRV claim while the question below is unresolved.

## Decision

abada's future `service` module will call every unary RPC through
`tonic::client::Grpc<S>`, generic over `S: tonic::client::GrpcService<
tonic::body::Body>` — one call path for in-process (`S` = a generated
`<Service>Server<T>`) and for proxy (`S` = a `tonic::transport::Channel`, or
anything else implementing the same trait). Request and response messages
travel as `prost_reflect::DynamicMessage`, encoded and decoded by a
`MessageDescriptor`-driven `tonic::codec::Codec`, not the generated prost
type — consistent with `RequestBinding::decode` already producing a
`DynamicMessage`, and with there being no per-RPC generated Rust code yet.

This settles the call-path *shape* only. It does not decide the exact
`service` module API, how `tonic::Status`/response metadata become
`abada::error::Status`/`ServerMetadata` on the error path, or how streaming
RPCs are called — out of scope here; streaming is its own later phase
(`WatchOperation`/`Logs`/`WatchEvents`).

## Consequences

- `abada` (the runtime crate) will need `tonic` (client-side types only:
  `tonic::client::Grpc`, `tonic::client::GrpcService`, `tonic::codec::Codec`,
  `tonic::Status`) and `tower` once the `service` module is written.
  `AGENTS.md` §2's allow-list already reserves `tower`/`prost` "when the
  service lands"; a `tonic` entry needs adding in the same PR that adds the
  dependency, per `abada-architecture`.
- **Before that PR**, abada's MSRV claim needs a decision: either raise
  `rust-version` in the workspace `Cargo.toml` (and the `msrv` CI job) past
  whatever `tonic`'s own build tooling requires at the time, or avoid
  `tonic-prost-build`/`tonic-build`'s codegen macros in abada's own build and
  write the small, fixed service-trait glue the prototype needed by hand.
  This ADR does not decide which — it is a blocking open question for
  whoever writes the `service` module, not a preference to settle now.
- The in-process path never touches a socket, `hyper` or `h2` framing at the
  OS level — `tonic::client::Grpc` still frames messages the way
  gRPC-over-HTTP/2 does (length-prefixed protobuf), just without a transport
  underneath. This is a property of tonic's own layering, not something
  abada added.
- Trailers on a *successful* unary response were not exercised: tonic does
  not expose a direct way for a handler to set trailers on `Ok(Response<T>)`
  (unlike Go's `grpc.SetTrailer`), and grpc-gateway's `Grpc-Trailer-*`
  forwarding (confirmed by reading `runtime/handler.go`) assumes they exist.
  This needs its own investigation before the `service` module claims
  trailer support — it may be a real, permanent deviation, not just an
  untested corner.

## Not validated

- Trailers on a successful response (see above).
- The error path: mapping a `tonic::Status` (and its metadata) returned by a
  failing RPC into `abada::error::Status`/`ServerMetadata`/`ErrorResponse`.
- Real network conditions for the proxy path (timeouts, connection failure,
  retries) — the prototype only used a healthy loopback connection.
- Streaming RPCs — this ADR is scoped to unary calls only, per the roadmap's
  own phase split ("tower::Service unário" before "streaming NDJSON").
- Whether `S: GrpcService<tonic::body::Body>` is satisfied by every
  transport abada should support in practice — only `Channel` and a bare
  generated `Server<T>` were tried. A `tower::Service` a user builds by hand
  (e.g. wrapping load-balancing or auth middleware) is architecturally
  expected to work per the trait bound, but was not tried.
- Whether abada's runtime should depend on `tonic` at all, versus defining
  its own minimal `GrpcService`-shaped trait to avoid the transitive
  `tonic-prost-build` MSRV problem reaching even indirectly — not attempted;
  flagged as the likely next question, not answered here.
