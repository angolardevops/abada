---
name: abada-architecture
description: The structure of abada and the rules for extending it — crate boundaries and dependency direction, where a module belongs, what may be public, dependency policy for the runtime crate, error and panic policy, hot-path discipline, MSRV/edition, and the v0.1 scope. Use before adding a module, a public type or function, a dependency, a feature flag, or when unsure which crate code belongs in. Not for recording a decision that changes these rules (`abada-adr`), nor for grpc-gateway behaviour details (`abada-conformance`).
---

# abada's architecture

Source of truth: [`docs/DESIGN.md`](../../../docs/DESIGN.md) and the ADRs in
`docs/adr/`. This skill turns them into rules you can apply to a diff. If the
two disagree, DESIGN.md/ADR win and this file is a bug.

## The four properties are the architecture

1. Behaviour-compatible with grpc-gateway → every behaviour has an oracle
   (`abada-conformance`).
2. Two entry points, one generator → `protoc-gen-abada` and `abada-build` are
   thin; **all** generation logic is in `abada-codegen`. If you are writing
   logic in either entry point, move it.
3. In-process and proxy → the runtime's call path is abstract over "call the
   service implementation" and "call a `Channel`". Nothing in routing,
   transcoding or errors may assume a network hop, a port or a socket.
4. A `tower::Service`, not a framework → the runtime speaks `http` types and
   `tower::Service`. `axum`, `hyper` server types, `actix`, `warp` never appear
   in `abada`'s dependencies; integrations are optional features or separate
   crates.

## Dependency direction

```
protoc-gen-abada ─┐
                  ├─▶ abada-codegen ─▶ abada (runtime)
abada-build ──────┘
```

- `abada` never depends on `abada-codegen`, `prost-build`, `protoc` or any
  build-time crate.
- Generated code depends only on `abada` (and the user's own prost/tonic types).
  If generated code needs a helper, the helper lives in `abada`, public but
  `#[doc(hidden)]` under a `__private` module when it is not user API.
- `benches/*` may depend on anything; nothing may depend on `benches/*`.

## Runtime (`abada`) module map

| Module | Owns |
|---|---|
| `path` | template parser, `Pattern`, `Router`, `RequestPath`, unescaping modes |
| `error` | `google.rpc.Status` responses, code → HTTP status, routing errors, error headers/trailers |
| `json` (in progress) | proto3 JSON as grpc-gateway's default marshaler (ADR 0001: `prost-reflect`) |
| `request` (in progress) | path values, query parameters, body selection, `FieldMask` for PATCH |
| `service` (in progress) | metadata in (`metadata::incoming`), a successful response out (`response::Response::success`), and the call itself (`call::unary`, in-process only, one RPC by hand — `codec::DynamicCodec`), all done narrow-scope; the `tower::Service` itself, proxy, `response_body`, NDJSON streaming (all planned) |

One module owns one concern. A module does not re-implement another's job
(e.g. JSON escaping lives in one place; `error` calls it).

## Dependency policy for `abada`

A new runtime dependency needs, in the PR: what it replaces, its transitive
cost (`cargo tree -p abada -e normal`), MSRV compatibility, and why it cannot
be a dev- or optional dependency. A dependency of weight (async runtime, a
serialisation framework, anything with a build script or `unsafe`-heavy) needs
an ADR. The allow-list is the "May depend on" column of `AGENTS.md` §2, and
`scripts/check-harness.py` enforces it — keep one list, there.

No runtime choice is imposed on users: no `tokio` features beyond what `tower`
requires, no global allocator, no logging backend (use `tracing` facade only if
accepted by ADR).

## Public API

These rules apply to **new and changed** public items. Existing items that do
not follow them yet are known debt, to be settled before the first release:
`path::{Segment, RouteOutcome, UnescapingMode, PatternError, MatchError}` and
`error::RoutingError` lack `#[non_exhaustive]`; `error::{Status, Any, ServerMetadata, ErrorResponse}`
expose public fields. Do not copy those patterns into new code.

- Public means semver. Before `1.0` it can change, but each change is named in
  the PR.
- Return typed errors (`enum` with `Display` + `std::error::Error`), not
  `String`, not `Box<dyn Error>` in public signatures.
- `#[non_exhaustive]` on public enums and structs that will grow (outcomes,
  errors, options).
- Constructors or builders for option structs; no public fields that must be
  kept consistent with each other.
- Every public item has a doc comment saying what grpc-gateway function it
  mirrors, when it does.

## Panics, errors, `unsafe`

- Nothing a request or a user's descriptor controls may panic: no `unwrap`,
  `panic!` or unchecked indexing on those paths. Hostile input returns the
  grpc-gateway error for it.
- `expect` only for an invariant the same function establishes (e.g. a stack
  it just pushed to), with the invariant in the message. This is the same rule
  as `AGENTS.md` §4.7.
- No `unsafe` in `abada` without an ADR and a `// SAFETY:` comment per block.

## Hot path

Routing, request population and JSON run per request.

- No allocation per candidate handler, no descriptor lookup by name per
  request (resolve once at registration).
- No `format!` for things that are compared, not displayed.
- Changes that plausibly move cost carry a number (`abada-measure`).

## Code style

- Edition 2024, `rust-version = 1.85` — do not use newer std APIs; the `msrv`
  CI job and `scripts/check.sh` build with 1.85.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` clean.
- Module doc comment at the top of every file saying what it mirrors in
  grpc-gateway and what it deliberately does not do.
- Comments explain why and cite Go (`runtime/mux.go`) — not what the next line
  does.

## v0.1 scope

In and Out are listed in DESIGN.md. Moving an item across the line needs an
ADR. In particular, **OpenAPI generation, client/bidi streaming over WebSocket,
SSE, and field-behaviour validation are Out**; do not add them as "small
extras".

## Checklist for a diff

- [ ] code is in the crate and module that owns the concern
- [ ] no dependency arrow reversed; no framework type in `abada`
- [ ] new dependency justified (or ADR)
- [ ] public items documented, errors typed, growable types `#[non_exhaustive]`
- [ ] no panic reachable from request or user descriptor
- [ ] hot-path cost considered, measured if moved
- [ ] MSRV respected
