# Parity with grpc-gateway: security and performance

Is abada at grpc-gateway v2.27.3's level for security and for performance?
First measurement, 2026-09-19, on `harness/seguranca-desempenho` (the tip of the
PR stack, `fe37c53`), host Ryzen 9 8940HX, 32 threads, load average 14–22
throughout (**above the 25% per-thread bar of `abada-performance`: every
number here is exploratory**). The gates are defined in
[`abada-security`](../../.claude/skills/abada-security/SKILL.md) (S1–S6) and
[`abada-performance`](../../.claude/skills/abada-performance/SKILL.md) (P1–P6).

**Verdict: not at grpc-gateway's level; the blocking finding of the first run
is closed.** That run found that a single `GET` aborted the process (below, S2);
it is fixed and pinned. 43 hostile inputs (39 reach abada; 4 were refused by the
`http` crate when the request was built) produce no panic, no abort and no
cross-request state, and nesting 200 000 in a JSON body answers 400. One row is
still a measured FAIL against grpc-gateway (memory amplification of large
repeated fields and maps), and most rows have no evidence because the instrument
for them does not exist yet. "At the level of grpc-gateway" may not be said
about abada, for either axis.

## Closed finding: one GET aborted the process

`GET /v1/query/x?nested.nested.….nested=1` with N `nested` components. With
N = 2 000 the answer was a normal 400; with N = 4 000 (and 9 000, a 54 KB URI,
under hyper's default header limit) the worker thread overflowed its 2 MiB stack
and the **whole process aborted** (`SIGABRT`; nothing can catch it). In an
unoptimised build the threshold was between 500 and 1 000.

Cause: `populate_field_value_from_path` was a faithful port of the Go function
except that Go's is a loop and abada's recursed once per component. Go's stack
grows to 1 GB, so grpc-gateway never notices; abada's does not grow.

Fix: the walk is a loop, **and** the request may not nest past 100 levels
(`MAX_MESSAGE_DEPTH`, the JSON codec's limit). The loop alone was not enough:
the same test still aborted, because dropping and encoding the 4 000-deep
message it built are recursive too (the mutation "loop, no limit" aborts in
debug `tests/security.rs` and in release `tests/request.rs`). The limit bounds
those. grpc-gateway answers 200 at 1 000 and 5 000 levels (vectors), so this is
a **written deviation**: DESIGN.md, "Query field paths", five vectors pinned by
name. Answers grpc-gateway gives before the limit is reached (unknown name,
"is not a message") are kept, also on 5 000-component paths.

Reproduce the old behaviour by reverting `fields.rs` to the parent commit:
`cargo test -p abada --test security` aborts in debug and in `--release`.

## Security

| # | Row | Verdict | Evidence |
|---|---|---|---|
| S1 | Path and routing | **NOT VALIDATED** as a hostile corpus | The well-formed-and-ugly cases of `path.json`/`request-*.json` already match grpc-gateway (see the Progress table). No `conformance/cases/security-path.json` exists: `%2f` in `**`, 64 KiB paths and `//` are not differentially tested. The property test fed 8 path cases (truncated/bad escapes, NUL, `%2f`, `..`, overlong UTF-8, int overflow): all answered 400/404, no panic |
| S2 | Request population | **PASS, one written deviation** | The abort is closed (above). Differential: 10 new vectors (`query-depth-*`, depths 99–5 000, scalar and `Timestamp` leaves, an unknown name and a non-message at the third component of a 5 000-component path) — 5 deviate by design, 5 agree with grpc-gateway. Property: the corpus now holds 4 000- and 9 000-component paths, and `a_deep_query_field_path_is_refused_past_100_levels` fixes 99/100 → 200 and 101/4 000/9 000 → 400 through the `Gateway`. `?fString=evil` on a route that binds `fString` from the path is 200, ignored as grpc-gateway does (`request-*.json`). Not validated: other recursion reachable from a request that this walk does not bound (an `Any` inside a `Struct` query value was not attacked separately; the JSON codec's own limit covers bodies), and stack use on a thread smaller than tokio's 2 MiB |
| S3 | JSON | **PARTIAL** | 868 JSON vectors match protojson, with one written deviation (100-message nesting limit, safer than Go's). Hostile shapes through the property test: BOM, invalid UTF-8, `1e999999999`, duplicate keys, lone surrogate, unregistered `Any`, nesting 99/101/10 000/200 000 all answered without panic or stack use scaling with input. Not differential: no vectors for these |
| S4 | Headers and metadata | **NOT VALIDATED** | 1 000 `Grpc-Metadata-*`, a 64 KiB value, bad `-bin` base64, `X-HTTP-Method-Override` garbage, `TE` lists: all answered, no panic. Injection in either direction and the response headers were not attacked; a control byte cannot even be built as an `http::HeaderValue` |
| S5 | Resource bounds | **FAIL** (relative to grpc-gateway) | table below |
| S6 | Supply chain | **PASS, partly** | `cargo deny check advisories bans sources`: ok (no `deny.toml`, so **licences not checked**); zero `unsafe` in `crates/*/src`; `cargo audit` not installed (the RustSec DB is what `deny` used); `Cargo.lock` is committed |

### S5: memory amplification, same body through both

`crates/abada/tests/security.rs` (peak live heap over input bytes, counting
allocator, one run in `--release`) against Go's `runtime.JSONPb` decoder into a
`dynamicpb` message — what the generated handler does — with peak sampled every
200 µs, maximum of 3 runs.

| Body | Bytes in | abada peak / total | grpc-gateway peak / total | abada time | Go time |
|---|---|---|---|---|---|
| `{"rInt32":[0,0,…]}`, 10⁶ elements | 2.0 MB | **57×** / 88× | 39× / 68× | 1.15 s | 2.6 s |
| `{"mStr":{…}}`, 10⁵ entries | 1.3 MB | **17×** / 38× | 12× / 21× | 0.76 s | 0.81 s |
| `{"fString":"…"}`, 1 MiB string | 1.0 MB | 1× / 1× | 6× / 7× | 10 ms | 119 ms |

Reading: both amplify well past the 10× bound of the skill, so this is not
"abada is unsafe and Go is safe": the mounter's body limit is what bounds it,
and neither gateway has one by default. But on repeated scalars and maps abada
uses **1.4–1.9× more memory than grpc-gateway** for the same input, which the
skill's rule ("not worse than Go's floor") calls a FAIL. It is faster or equal
in time and much better on a large string. The cause is not investigated (the
per-element cost is ~114 B; a growth-doubled `Vec` or an intermediate value tree
are the candidates, not proven). The test **ratchets** these two cases at
today's value (60× and 18×) with Go's figure next to them in `RATCHET`; the
ceiling may only go down.

Observations from the same run, not findings against abada:

- `Gateway` reads the whole body before decoding, exactly as grpc-gateway does.
  No limit is imposed; the mounter must add `http_body_util::Limited`. The
  README example does not show it yet.
- The body of 8 MiB `bytes` was refused with gRPC code 11 by the test's tonic
  server (4 MiB decode limit), not by abada. grpc-go answers
  `RESOURCE_EXHAUSTED` (HTTP 429) for the same condition; not compared.
- `abada::service::ResponseBody` is the `Response` type of `Gateway` but is not
  re-exported: a caller cannot name it. An API finding for the Rust reviewer.
- Two hostile inputs never reach abada: a 64 KiB path and a control byte in a
  header value are refused by the `http` crate when the request is built.

## Performance

| # | Row | Verdict | Evidence |
|---|---|---|---|
| P1 | Latency, end to end | **NOT MEASURED** | no end-to-end setup: the Go side has no generated-handler-over-network server in `conformance/oracle/e2e` for load, and no load generator (`oha`, `wrk`, `ghz`) is installed |
| P2 | Throughput and CPU | **NOT MEASURED** | same |
| P3 | Memory, flat over a soak | **NOT MEASURED** | only the per-request peak above; no soak |
| P4 | Tail under load | **NOT MEASURED** | same as P1 |
| P5 | Cold start | **NOT MEASURED** | — |
| P6 | Stages | **PARTIAL, exploratory** | routing only, below. JSON: ADR 0001 has the committed comparison, not re-run here |

Routing, the 56 canonical `delonix.node.v1` requests with all 56 bindings
registered, `route_bench` against `abadaoracle bench`, release, load average
13.9–22.1 before and after each run:

| Run | abada (this branch) | abada (`main`, A/B) | grpc-gateway |
|---|---|---|---|
| 1 | 18 572 ns | 18 821 ns | 22 748 ns |
| 2 | 18 187 ns | 18 162 ns | 29 591 ns (min 21 360, max 41 911) |
| 3 | — | — | 20 774 ns |
| Head, A/B rounds | 18 836, 18 742 ns | | |

abada is **not slower than grpc-gateway on routing** (median of medians 18.7 µs
vs 22.7 µs, ratio about 0.8, and Go's own spread covers the gap): same order of
magnitude, nothing finer. `main` and this branch are the same speed, so the
stack did not regress routing. Both sides are about 5× slower than the
2026-09-17 figures in [delonix-node-v1.md](delonix-node-v1.md) (3.6–4.2 µs,
recorded at load 58) — the host is in a different state now, unexplained, and
that earlier table should not be compared with this one.

## What this run did not validate

- Any differential security corpus (`conformance/cases/security-*.json` and its
  oracle): the property test proves *no panic and bounded cost*, not *same
  answer as grpc-gateway*.
- Raw-socket behaviour: request smuggling, `Content-Length` lies, slowloris,
  header-size limits (hyper's, not abada's, but unmeasured).
- Fuzzing: no `cargo fuzz` target exists; `cargo-fuzz` is not installed.
- Licences (`cargo deny check licenses`, needs a `deny.toml` decision) and
  `cargo audit`.
- Streaming and the generated code path (neither exists yet in the stack).
- Every performance row that needs a load generator and an end-to-end Go server.
- Any number on a quiet host. The run above overlapped with builds of mine.

## To reach the level

In order: (1) find and remove the memory overhead on repeated scalars and maps
until `RATCHET` reaches Go's column; (2) write the differential corpus for
S1/S4 in the oracle (S2 has it for depth); (3) add the end-to-end Go server and a load generator so
P1–P5 can be measured; (4) a `deny.toml` and a fuzz target per parser; (5) show
`Limited` in the README. Each is its own PR with its own proof.
