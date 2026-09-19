---
name: abada-performance
description: How abada proves it is as fast as grpc-gateway, and no worse under load — the parity gate for latency, throughput, memory and cold start, measured end to end (the real Gateway behind a real tonic server vs grpc-gateway behind a real gRPC server, same requests), plus per-stage costs (route, request population, JSON, call) that say where a regression is. Use when a change may touch the request hot path, when someone asks "is abada as fast as grpc-gateway?", before a release, or when a benchmark number is quoted anywhere. Builds on `abada-measure` (conditions and noise rules) and does not replace it. Not for correctness (`abada-conformance`) nor hostile input (`abada-security`).
---

# Performance: parity with grpc-gateway, measured on the same inputs

[`abada-measure`](../abada-measure/SKILL.md) says how to write a number down
honestly. This skill says **which numbers decide whether abada is at
grpc-gateway's level**, how to take them, and what each verdict allows anyone to
say. A micro-benchmark that abada wins is not parity: routing 56 bindings in
4 µs matters only if the whole request is not 40 µs of something else.

## 1. What "the same level" means

abada is at grpc-gateway's level when, on the same host, the same protos, the
same requests and the same fake gRPC service:

- **P1 Latency** — p50 and p99 of a request end to end are no worse than
  grpc-gateway's, beyond the noise you measured;
- **P2 Throughput** — sustained requests/second at fixed concurrency is no
  worse, and CPU per request is no worse;
- **P3 Memory** — resident set after warm-up and under load is no worse, and
  **flat** over a long run (a soak, not a snapshot);
- **P4 Tail under load** — p99.9 does not blow up as concurrency rises past
  the number of cores;
- **P5 Cold start** — time from process start to first served request, and to
  a registered 56-binding router, is no worse;
- **P6 Stages** — each stage's cost is known so a regression names its stage.

"No worse" is a claim about a ratio and its interval, not two single numbers.

## 2. The setup that makes the comparison honest

Both sides serve the **same contract** (`conformance/contracts/*.binpb`) and
the **same fake backend**: a gRPC server that returns a fixed response from
memory, with no work of its own. The backend must be so cheap that what is
measured is the gateway. Then:

| Side | What runs |
|---|---|
| Reference | Go: `conformance/oracle/parity` — the code `protoc-gen-grpc-gateway` v2.27.3 generates for the contract, on a `runtime.ServeMux` over a real `net/http` server and a real `grpc.ClientConn`, default options. Built by `scripts/parity/build-reference.sh` |
| abada | `benches/parity-gateway` — `Gateway` mounted in hyper (HTTP/1.1, keep-alive), calling the backend through one `tonic::transport::Channel` (proxy mode, the counterpart of the Go `ClientConn`) |
| Backend (shared) | `parity backend`: a gRPC server with a fixed in-memory answer per RPC and no work of its own; both gateways call the same process |
| Load generator (shared) | `parity loadgen`, out of process; closed loop and open loop (latency from the intended start), CPU and RSS of the gateway read from `/proc` |

`scripts/parity/run.sh --quick|--full` runs the whole protocol (fresh processes
per run, sides alternated run by run, correctness check first, cold start,
soak) and `scripts/parity/summarize.py` prints the tables with medians,
ranges and a verdict per row. **Before any request is measured, `check` must
show both gateways answering the whole mix identically**; a request they answer
differently is excluded from the load (`weight: 0` in `benches/parity/mix.json`
with the reason) and reported as not comparable — that is how `response_body`
was found missing.

Run **both modes for abada** and label them: in-process has no Go equivalent
and must never be compared with the reference's network hop as if it did — it is
reported as abada's own number.

**Load generator.** One generator for both sides, out of process, so it does not
steal from either (`parity loadgen`; `oha` or `wrk` are acceptable for a spot check). Fixed
concurrency levels — 1, 8, `nproc`, 4×`nproc` — for a fixed duration (≥ 30 s
after a 10 s warm-up), **closed loop** for throughput and **open loop at a
fixed rate** (coordinated-omission-safe, e.g. `oha -q`) for latency
percentiles. A closed-loop p99 hides the queueing that an open-loop one shows.

**Request mix** — not one request. Weighted, written in the case file:
small `GET` with one path value; `GET` with a query and a repeated field;
`POST` with a 1 KiB and a 64 KiB JSON body; `PATCH` with `FieldMask`; a
response with 64-bit ints, maps and a `Timestamp`; an error response. Each
request is reported separately and as the mix.

## 3. Conditions (from `abada-measure`, repeated because they get skipped)

Before and after every run: `uptime`, CPU model and thread count, **governor and
current clock** (a laptop in `powersave` at 544 MHz makes every number about 5×
slower and every wake-up longer; the ratio survives, the absolute numbers do not,
and it must be said), 
whether another build is running (`pgrep -a cargo`). **A run started with load
average above 25% of the thread count is exploratory, not a result** — it goes
in the report labelled as such, and no ratio from it is quoted. Three runs
minimum, median, and the min-max spread beside it. Release profile with the
`lto`/`codegen-units`/`panic` settings written out — the same on every run.
Pin nothing unless you compare pinned against unpinned and say so.

## 4. Per-stage costs (P6)

In-process, no network, so a regression has a name. Criterion or the existing
`examples/*_bench.rs` style; each stage reports ns/request and allocations per
request (counting allocator):

1. target parse (`RequestPath::parse`) and route (`Router`) — vs `abadaoracle bench`
2. request population — path, query, body → message
3. JSON decode of the body and JSON encode of the response — vs
   `abadaoracle json-bench` (ADR 0001 has the method)
4. metadata in and out
5. the unary call through `tonic::client::Grpc` with the fake service
6. error response rendering

The sum of the stages must explain the end-to-end number within 20%; if not,
there is a cost nobody is measuring.

## 5. Verdicts

Per row P1–P6, then overall. Never "almost".

- **AT LEVEL** — ratio abada/reference ≤ 1.0 + noise, interval stated.
- **BELOW LEVEL** — ratio above that. Give the ratio, the stage that explains
  it (§4) and the smallest change expected to fix it.
- **AHEAD** — ratio ≤ 0.8 outside the noise, on every request in the mix. Say
  so in one line and do not build claims on it: it holds for this host and mix.
- **NOT MEASURED** — cannot be PASS. State the missing tool or condition.

Anything faster than the reference is reported next to what it costs
(compile time, binary size, memory) — a win that moves cost elsewhere is not a
win.

## 6. What may be said publicly

Only what a committed report supports, with host, date, versions and
commit. "As fast as grpc-gateway" requires P1–P5 AT LEVEL **or** AHEAD on the
whole mix, from runs that were not exploratory. Otherwise say the numbers and
stop: "same order of magnitude on a loaded host" is a legitimate sentence and
the current honest one for routing.

## 7. Regression guard

Once a baseline is committed (`docs/readiness/grpc-gateway-parity.md` + raw
tables under `benches/`), a change to the hot path (`crates/abada/src/{path,
request,json,service}`) must re-run §4 and show no stage worse than the noise,
or state the stage and why. A number that only moves in the wrong direction is
never explained away with "the host was busy" unless the rerun on a quiet host
says so.
