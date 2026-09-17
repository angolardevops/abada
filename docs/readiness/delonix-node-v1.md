# Readiness: `delonix.node.v1`

Is abada ready to serve the HTTP/JSON side of the `delonix-runtime` node
contract? Measured on 2026-09-17 against
`angolardevops/delonix-runtime@964f6a4f` (`proto/delonix/node/v1`, the
descriptor set is committed as `conformance/contracts/delonix-node-v1.binpb`).

**Verdict: not yet.** Reading the contract's HTTP rules and choosing the RPC
for a request are done and proven equal to grpc-gateway, and so are the error
response (status, body, headers) once a gRPC status is in hand and the JSON
codec for bodies and responses. Putting path and query values into the
request, the call itself and streaming are not started.

## What the contract asks for, and where abada stands

| Needed by the contract | How much of it | abada | Proof |
|---|---|---|---|
| Read `google.api.http` from the descriptor set | 58 RPCs, 56 bindings | **done** | same 56 bindings, same order, as grpc-gateway's `protodesc` + `proto.GetExtension` |
| Route a request to its RPC | 56 bindings, all registered together | **done** | 728 requests, two unescaping modes, identical outcome, RPC and path values; one canonical request per binding reaches its own RPC |
| Put path values into typed request fields | 56 bindings | not started | — |
| Query parameters | 18 RPCs (every `List*`, `Delete*`, `GetImage`, `Logs`, `WatchEvents`, …) | not started | — |
| `body: "*"` | 26 `POST` | JSON decoding **done** (`Marshaler::decode_into`); not yet wired into a request | see canonical JSON below |
| `body: <field>` with a `FieldMask` | 1 `PATCH` (`UpdateContainer`) | decoding into the field **done** (`Marshaler::decode_field`, grpc-gateway's `encoding/json` rules); filling `update_mask` from the body when it is empty (`runtime.FieldMaskFromRequestBody`) not started | 178 `body: "<field>"` vectors over `conformance/protos` |
| Canonical proto3 JSON | 64-bit ints ×28, maps ×20, enums ×13, oneofs ×13, bytes ×9, `optional` ×3; `Any`, `Struct`, `FieldMask`, `Timestamp`, `Duration` | **done** (`abada::json`), one written deviation: at most 100 nested messages | the 28 contract bodies: same accept/reject, same decoded message and the same output bytes as grpc-gateway, with and without `EmitUnpopulated`; plus 868 cases over `conformance/protos` covering every shape the contract uses and the ones it does not |
| `google.rpc.Status` errors with grpc-gateway's HTTP codes | every RPC | **done**, details rendered through the type registry; not yet wired to a tonic call | `conformance/vectors/errors.json`: 25 codes, 64 statuses (4 with registered detail types), 29 routing errors, same status, headers, body and trailers as grpc-gateway on the wire; one written deviation for header values with control bytes |
| Server streaming | `WatchOperation`, `Logs`, `WatchEvents` | not started | — |
| `Exec`, `Console` (bidirectional, no mapping; WebSocket per ADR-0040 D4) | 2 RPCs | out of v0.1 scope | — |
| Serve gRPC and HTTP/JSON on one unix socket | the node API's shape | not started | — |
| Generated code (`protoc-gen-abada`, `abada-build`) | — | not started; the rule extraction it will use is done | — |
| Published on crates.io | — | no (0.0.1, unpublished) | — |

The inventory comes from `abadaoracle inventory` over the same descriptor set;
the numbers are counted, not estimated.

## Routing cost

`cargo run --release -p abada-codegen --example route_bench` against
`abadaoracle bench`: the 56 canonical requests, target parsing included, all 56
bindings registered.

| Run | abada | grpc-gateway |
|---|---|---|
| 1 | 4 220 ns/request | 4 226 ns/request |
| 2 | 3 622 ns/request | 5 106 ns/request |

The host (Ryzen 9 8940HX, 32 threads) was at load average 58 during both runs,
so the two are **the same order of magnitude and nothing finer**. Neither side
is ahead on this evidence. Both scan handlers linearly; abada also copies the
components once per candidate, which is the first thing to remove if routing
ever shows up in a profile.

## Found in the contract

**`GET /v1/operations/{id}:watch` works only because of declaration order.**
`GetOperation` (`/v1/operations/{id}`) also matches `/v1/operations/x:watch`,
binding `id = "x:watch"`. grpc-gateway, and abada with it, try the handler
registered last first, and the generated gateway registers methods in
declaration order — checked in the code `protoc-gen-grpc-gateway` v2.27.3
generates for this contract. Declaring `WatchOperation` before `GetOperation`
would send every watch to `GetOperation` without an error. The conformance test
shows it: registering first-wins makes `WatchOperation` unreachable. Across
services the order depends on the order the server calls each `Register*Handler`.

This is a property of the contract, reported to its owner, not changed here.

## Not validated

- Error `details` of a type a Go gateway binary links but abada's registry
  does not hold (abada's holds the contract, the well-known types and
  `google.rpc.Status`), and errors in the middle of a server stream.
- The JSON of a real request end to end: the codec is measured on bodies and
  messages, not behind a tonic call (the typed hop is not written).
- The 405 fallback visits other methods in a Go map order; abada uses
  registration order and the oracle drops any request whose answer depends on
  it (none were dropped for this contract).
- Path values are compared after Go's JSON encoding, which rewrites invalid UTF-8.
- The benchmark on a quiet host.
