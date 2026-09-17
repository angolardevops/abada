# ADR 0001 — How JSON is transcoded

- Status: **accepted**, 2026-09-17
- Settles: the "Open decision: how JSON is transcoded" of `docs/DESIGN.md`
- Evidence: `benches/json-transcode` (crate `abada-json-bench`, not published),
  `conformance/cases/json-delonix-node-v1.json`,
  `conformance/vectors/json-delonix-node-v1.json`

## Context

abada turns an HTTP/JSON body into the request message of a tonic service and
the response message back into JSON. Property 1 of the design is to behave like
grpc-gateway, so "proto3 canonical JSON" means **what grpc-gateway v2.27.3
writes and accepts**, not what the proto3 JSON spec allows.

What grpc-gateway does by default was read in its code, not assumed:
`runtime.ServeMux` uses `HTTPBodyMarshaler{JSONPb{MarshalOptions{EmitUnpopulated:
true}, UnmarshalOptions{DiscardUnknown: true}}}` (`runtime/marshaler_registry.go`).
So by default it **writes every field, including `null` for unset message
fields**, uses lowerCamelCase names (`UseProtoNames: false`), enums by name and
64-bit integers as strings; and it **discards unknown fields — and unknown enum
names** — instead of rejecting them. The generated handler decodes with
`marshaler.NewDecoder(body).Decode(&req)` and treats `io.EOF` as an empty body.

The contract that matters first, `delonix.node.v1`, needs 64-bit integers ×28,
maps ×20, enums ×13, oneofs ×13, bytes ×9, `optional` ×3, and `Any`
(`Operation.result`, returned by every `Create*`/`Delete*`), `FieldMask`
(`UpdateContainer`), `Struct` (`ProviderExtensions`), `Timestamp`, `Duration`
(`docs/readiness/delonix-node-v1.md`).

## Options

- **(a) `prost-reflect` 0.16.5** — `DynamicMessage` with its serde support,
  driven by the descriptor at runtime. For an in-process call the dynamic
  message reaches the user's prost type through the binary encoding
  (`JSON → DynamicMessage → bytes → M`, and back).
- **(b) `pbjson` 0.9.0** — `pbjson-build` generates serde impls for the user's
  prost types, with `pbjson-types` 0.9.0 for the well-known types
  (`JSON ↔ M` directly). Whether defaults are written (`emit_fields`) is chosen
  at build time, so both variants were generated.

A third option — transcoding straight between JSON and protobuf binary driven by
the descriptor, with no intermediate message (what Envoy's transcoder does) —
was not built. It is named under Consequences as the way to remove (a)'s extra
hop, to be measured before it is written.

## How it was measured

**Correctness.** `abadaoracle json` (run by `scripts/regen-vectors.sh`, checked
in CI by `--check`) loads the contract into grpc-gateway's own default marshaler
— taken from a bare `ServeMux` through `runtime.MarshalerForRequest`, not
rebuilt from its options — and writes for 29 bodies over 13 messages of the
contract: whether the handler's decode accepts the body, the decoded message in
deterministic binary, and the output with `EmitUnpopulated` true and false.
The Rust test (`benches/json-transcode/tests/grpc_gateway.rs`) runs both
candidates on every vector:

- *decode*: accepted/rejected as Go, and the decoded message equal to Go's (both
  compared as `DynamicMessage`s from binary, so encoder choices do not count);
- *encode*: starting from **Go's** decoded message (a decoding bug cannot hide
  an encoding one), the JSON compared as values — key order ignored, `0` and
  `0.0` told apart.

The findings are fixed in `benches/json-transcode/expected.txt`; any change in
either library or in grpc-gateway fails the test. The test was checked by
breaking it on purpose: `deny_unknown_fields(true)` and
`stringify_64_bit_integers(false)` each make it fail on the expected lines.

**Cost.** `cargo run --release -p abada-json-bench --bin bench`: for each of 5
bodies, every operation runs round-robin for 15 rounds of ~20 ms, reporting the
median, min and max ns/op; allocations are counted by a global allocator
(allocations + reallocations, per op). The reference is `abadaoracle json-bench`
(Go `testing.Benchmark`, 7 samples) over the same bodies with grpc-gateway's
default marshaler.

## Correctness findings

29 bodies; "=" means the same as grpc-gateway.

| | (a) prost-reflect | (b) pbjson |
|---|---|---|
| int64 as string, enums by name, bytes (std and URL-safe base64), oneofs, `optional` with presence, `Duration`, unknown fields discarded; rejecting a oneof set twice, an out-of-range int64, a fractional int32, an unresolvable `Any` type, a snake_case `FieldMask` path, a non-object body | = | = (the `Any` and `FieldMask` rejections for the wrong reason, see below) |
| `Any` (`Operation.result`, a message and a WKT inside) | = both ways | **rejects `@type` on decode; writes `{"typeUrl","value":<base64>}`** |
| `FieldMask` (`"spec.env,spec.resources.memoryLimitBytes"`) | = both ways | **rejects the string; writes `{"paths":[…snake_case]}`** |
| `Timestamp` output | = (`…Z`, 0/3/6/9 fraction digits) | **`…+00:00`** |
| `Struct` | = except integer-valued numbers written `0.0` for Go's `0` | same `0.0`; **`null` inside decodes to a `Value` with no kind** (Go keeps `NULL_VALUE`; protojson refuses to marshal a `Value` with no kind — read in `well_known_types.go`, not run) |
| unknown enum name in the body | = (discarded) | **rejects** |
| unknown enum number (`7`) | = (kept, written as `7`) | **rejects on decode; fails to encode** a message holding it (a 500 for a valid message from a newer server) |
| `null` for a map field | = (accepted) | **rejects** |
| `EmitUnpopulated: true` (the default) | **unset message fields omitted, Go writes `null`** — the only emit-mode difference in 29 bodies | same |
| `EmitUnpopulated: false` | = on every accepted body, but `Struct`'s `0.0` | wrong on `Any`, `FieldMask`, `Timestamp` |
| duplicate key / proto and JSON name of the same field | **accepts** (Go rejects) | = (rejects) |
| int64 in exponent form (`1e3`, `"2e2"`) | **rejects** (Go accepts) | **rejects** |
| lower-case `t`/`z` in a `Timestamp` | = (rejects) | **accepts** |

Not compared: object key order (both Rust candidates write maps from a
`HashMap`, in a different order on every run; Go sorts map keys), whitespace,
float/double/unsigned/fixed fields and wrapper types (the contract has none),
`body: "<field>"` and `response_body` selection, streaming wrappers, error
bodies, an empty body, and trailing bytes after the JSON value (Go's decoder
ignores them; both candidates as wired here reject them).

## Cost findings

Host: Ryzen 9 8940HX, 32 threads, rustc 1.98.1, Go 1.26.2, **shared with other
builds**. Three runs unpinned and three pinned (`taskset -c 7`), interleaved
with three Go runs, 12:05–12:13. The 1-minute load average read before and after
each run went from **8.3 to 57.1**. Figures below are the median of the three
unpinned run medians, in µs/op, with the range of those run medians. Pinned runs
were 13–47 % slower and noisier (the pinned core was not reserved), and change
no ordering.

| body (bytes) | op | (a) JSON↔Dynamic | (a) with typed hop | (b) pbjson | allocs (a)/(a typed)/(b) | Go reference, 3 runs |
|---|---|---|---|---|---|---|
| `CreateContainerRequest` (1 977) | decode | 19.8 [19.3–23.9] | 52.6 [50.6–63.7] | 8.4 [8.4–10.4] | 139 / 202 / 74 | 60.6, 66.8, 188 |
| | encode | 11.6 [11.0–14.8] | 24.9 [24.7–30.9] | 3.1 [3.0–3.3] | 7 / 87 / 13 | 34.0, 39.9, 173 |
| `CreateVirtualMachineRequest` with `Struct` (1 021) | decode | 17.0 [14.4–17.4] | 48.2 [41.8–49.7] | 5.2 [4.5–5.3] | 112 / 154 / 47 | 68.7, 72.2, 105 |
| | encode | 17.1 [15.0–17.8] | 31.7 [27.6–32.7] | 1.5 [1.4–1.5] | 26 / 92 / 5 | 30.6, 34.5, 190 |
| `ListContainersResponse`, 50 items (38 562) | decode | 637 [553–812] | 1 635 [1 450–1 962] | 262 [229–347] | 3 437 / 4 596 / 1 459 | 1 780, 2 254, 14 494 |
| | encode | 507 [431–658] | 892 [770–1 132] | 115 [98–131] | 184 / 1 918 / 459 | 1 530, 1 995, 13 401 |
| `Operation` with `Any` (828) | decode | 10.0 [8.4–22.9] | 13.6 [11.8–32.0] | cannot decode | 52 / 62 / — | 38.1, 39.8, 324 |
| | encode | 10.6 [8.6–25.2] | 12.3 [10.7–30.7] | 0.9 [0.8–2.9], **wrong output** | 21 / 35 / 6 | 31.6, 35.9, 203 |
| `LogChunk` (176) | decode | 0.78 [0.71–2.27] | 1.53 [1.28–4.83] | 0.31 [0.27–1.05] | 6 / 9 / 1 | 5.2, 6.7, 36.2 |
| | encode | 0.49 [0.43–1.70] | 0.84 [0.70–2.85] | 0.24 [0.20–0.92] | 1 / 4 / 3 | 3.1, 17.4, 24.6 |

One-time and lookup costs of (a), same runs: `DescriptorPool::decode` of the
46 018-byte contract 1.79 ms [1.63–2.37] and 18 129 allocations, once at start;
`get_message_by_name` 38 ns [36–46], 0 allocations. A per-request lookup made a
tiny decode 293 ns instead of 243 ns — tens of nanoseconds, and none if the
generated code keeps the `MessageDescriptor` in a static.

What the numbers support, and nothing finer:

- **pbjson is faster, by a margin above the noise**: decode 2.4–3.3× against
  (a)'s dynamic step alone and 5–9× against the full typed path; encode 2–11×
  and 3.5–21×. Run medians spread by up to ~50 % (×3 on one run of the smallest
  body); the smallest ratio, 2×, is on `LogChunk` encode and is the least firm.
- **The typed hop costs as much as the transcoding or more**: the full typed
  path is 1.4–2.8× the dynamic step on decode and 1.2–2.1× on encode, and it
  brings most of the response path's allocations.
- **Go is a reference, not a like-for-like figure**: the oracle uses `dynamicpb`,
  not the generated Go types grpc-gateway users have, which are faster. (a)'s
  full typed path is in the same order of magnitude as grpc-gateway over dynamic
  messages, and nothing finer; generated Go types were not measured.
- The absolute figures are this loaded host's; do not quote them as abada's
  latency.

## Decision

**(a) `prost-reflect`.** The numbers favour pbjson; the correctness rules it out
for this contract, and correctness is the requirement while cost is a budget:

1. pbjson cannot read or write `Any`, and `Operation.result` is the response of
   every `Create*`/`Delete*` of the first consumer. It writes `FieldMask` as an
   object, and `UpdateContainer` is the contract's `PATCH`. These are not
   options to tune: `pbjson-types` has no `Any` or `FieldMask` serde, and a
   canonical `Any` needs a type registry, which is a descriptor pool — option (a)
   again.
2. pbjson fails to encode a message with an enum number it does not know, which
   turns a valid response from a newer server into an error; rejects unknown
   enum names grpc-gateway discards; and changes every `Timestamp`'s spelling.
   These come from generated code and `pbjson-types`, not from a setting.
3. pbjson also forces a second code generation step on the user's types — the
   "against" already written in DESIGN.md — and fixes `EmitUnpopulated` at build
   time, where grpc-gateway sets it per `ServeMux`.
4. (a) matched grpc-gateway on every shape the 29 bodies exercise **except**
   the deviations below, all fixable in abada without a different library. The
   largest one — no `null` for unset messages under the default
   `EmitUnpopulated: true` — is shared by pbjson, so it does not separate them.

## Consequences

- The `abada` runtime depends on `prost-reflect` (with `serde`). The generated
  code embeds the descriptor set, builds the pool once (~2 ms for this
  contract) and keeps each `MessageDescriptor` in a static; no lookup per request.
- Deviations to close **before** abada claims behaviour compatibility for JSON,
  each already a line of `expected.txt` that must turn to `identical`:
  1. `EmitUnpopulated: true` must write `null` for unset message fields.
     prost-reflect has no option for it, so abada needs its own serializer walk
     over the `DynamicMessage` (or an upstream option). This is grpc-gateway's
     default, so it is not optional.
  2. Duplicate keys, and a field given by both names, must be rejected.
  3. 64-bit integers in exponent form (`1e3`, `"2e2"`) must be accepted.
  4. `Struct` numbers with an integral value must be written without `.0`.
  5. Map keys should be written sorted, and the suite must start comparing
     key order, before any claim about byte-identical output.
- Cost debt, measured above: the `DynamicMessage ↔ M` hop is a quarter to two
  thirds of (a)'s request-path cost, depending on the body. For the proxy mode a tonic codec over `DynamicMessage` (or
  over bytes) removes it; for the in-process mode, transcoding straight between
  JSON and binary is the candidate. Neither is built until a profile of a whole
  request shows transcoding matters — that profile does not exist yet.
- **Reopen** if a pbjson release (or abada-owned well-known types plus a fix for
  unknown enums in pbjson-build) turns pbjson's lines of `expected.txt` to
  `identical`: on these figures it would be 2–21× cheaper per operation, and
  this ADR would need a successor.

## Not validated

- A quiet host. Every figure was taken under load average 8–57; only ratios well
  above the observed spread are claimed.
- The cost of a whole HTTP request (hyper, routing, the tonic call), so the share
  of a request spent transcoding is unknown.
- grpc-gateway with generated Go types (the oracle uses `dynamicpb`).
- The proxy-mode path `JSON → DynamicMessage → bytes` on its own (only the
  dynamic step and the full typed path were timed).
- The shapes listed under "Not compared", and every message of the contract:
  29 bodies over 13 of its messages were checked, not the 20 maps and 13 oneofs
  one by one.
- Versions other than prost-reflect 0.16.5, pbjson/pbjson-build/pbjson-types
  0.9.0, serde_json 1.0.151, grpc-gateway v2.27.3 with google.golang.org/protobuf
  v1.36.10.

## Follow-up (2026-09-17): the deviations are closed

The decision stands — `prost-reflect`'s `DynamicMessage`, driven by the
descriptor — but not its serde support. The five deviations, and what the
body comparison did not look at, were closed by `abada::json::Marshaler`,
a port of protojson (v1.36.10) and of the parts of Go's `encoding/json`,
`strconv`, `encoding/base64` and `time` that reach grpc-gateway's bytes, over
`DynamicMessage`. No serde, no `serde_json`: a duplicate key cannot be seen
through serde's map visitor, and protojson's own tokenizer is what decides
what a body may contain.

The count above was wrong: `conformance/cases/json-delonix-node-v1.json` has
**28** bodies, not 29.

| Deviation (Consequences, above) | Closed by | Evidence |
|---|---|---|
| 1. no `null` for unset messages under `EmitUnpopulated` | protojson's `unpopulatedFieldRanger`: `null` for unset messages and proto2 scalars, nothing for unset oneof members **or proto3 `optional` fields** (a synthetic oneof) | contract bodies with unset messages; `optionals_empty` writes `{}` |
| 2. duplicate keys and a field given by both names accepted | a per-object set of field numbers; also duplicate map keys and a oneof given twice | `duplicate_key`, `proto_and_json_name_both`, `dup_*`, `map_key_duplicate*`, `oneof_*twice` |
| 3. 64-bit integers in exponent form rejected | `normalizeToIntString`, including numbers inside strings, which stop at the first delimiter (`"1,"` is 1) | `int64_exponent`, `i32_*exponent*`, `i32_string_trailing_*` |
| 4. `Struct` numbers written `0.0` | `strconv.AppendFloat` shortest form with protojson's `e`/`f` rule | contract VM body; `struct_*`, `dbl_*`, `flt_*`, `enc_float*` |
| 5. map keys in `HashMap` order, key order not compared | `order.GenericKeyOrder` (numbers numerically, strings by bytes); outputs are now compared **byte for byte** | `map_all_key_kinds`, `enc_many_map_keys`; every output vector |

Measured in `benches/json-transcode` as a third column: on the 28 contract
bodies abada decodes to the same message as grpc-gateway (19 accepted, 9
rejected by both) and writes the same **bytes** with and without
`EmitUnpopulated` (`expected.txt`, `abada ... same-bytes`). Beyond the
contract, `conformance/vectors/json-abada-conformance-v1.json` holds 868
cases over `conformance/protos` — every scalar kind, maps with every key
kind, oneofs with well-known types, proto3 `optional`, proto2 with defaults,
required fields and extensions, every well-known type — answered by
grpc-gateway linked with protoc-gen-go's types: 506 bodies (199 rejected),
97 messages built in prototext that no body can produce (30 of them fail to
marshal: NaN in a `Value`, out-of-range `Timestamp`/`Duration`, irreversible
`FieldMask` paths, `Any` with bytes that do not decode), 178
`body: "<field>"` decodes and 87 `response_body` encodes (11 of which fail to
marshal, as in Go).

Found on the way, against intuition, and now fixed in the vectors:
`body: "<field>"` and `response_body` for a non-message field do not use
protojson at all but `encoding/json` over the Go field type (an `int64` field
body must be a number, enums are numbers, map keys go through `strconv` with
base 0, `<>&` are escaped on the way out); `time.Parse` accepts a one-digit
hour, a comma before the fraction and a `+24:00` offset; Go's generated types
write oneof members last in binary; `math.NaN()` has a payload bit that
reaches `Any.value`.

One written deviation remains: messages nest at most 100 levels (prost's
limit for the binary form the request travels in), where grpc-gateway allows
10 000 (vector `depth_101`).

Cost, same harness and host, three unpinned runs at load average ~10
(14:04–14:06), median of run medians, µs/op, abada against (a) in the same
runs:

| body | decode abada / (a) | encode abada / (a) |
|---|---|---|
| `CreateContainerRequest` | 27.9 / 20.0 (1.40×) | 15.1 / 12.2 (1.24×) |
| `CreateVirtualMachineRequest` | 18.6 / 16.4 (1.13×) | 10.0 / 16.3 (0.61×) |
| `ListContainersResponse`, 50 items | 738 / 568 (1.30×) | 553 / 460 (1.20×) |
| `Operation` with `Any` | 13.6 / 10.3 (1.32×) | 11.5 / 10.9 (1.05×) |
| `LogChunk` | 1.32 / 0.88 (1.50×) | 0.67 / 0.58 (1.16×) |

abada decodes with fewer allocations than (a) (93 against 139 for
`CreateContainerRequest`) and stays well under (a)'s typed path, which is
what a request pays anyway. It is slower than (a)'s serde step by the price
of doing protojson's work (two tokens where serde has one, duplicate
detection, Go's number grammar); the run-to-run spread was up to 50 %, so
only "same order, at most 1.5× the dynamic step" is claimed. pbjson remains
2–12× cheaper than abada and remains ruled out on correctness.

Not validated by this follow-up: the typed hop (`DynamicMessage` → prost
type; prost-reflect's encoder drops `-0.0` in a proto3 `double`, read in its
code), `body`/`response_body` on oneof members and nested field paths, group
fields, `UseProtoNames`/`UseEnumNumbers`/`EmitDefaultValues` (implemented,
not measured), float-to-enum conversion on non-amd64 Go, protojson error
texts (randomised per build), a quiet host.
