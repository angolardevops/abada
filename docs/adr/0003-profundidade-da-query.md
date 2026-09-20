# ADR 0003 — A query field path nests at most 100 levels

- Status: **accepted**, 2026-09-20. Written after the fact: the code and its
  vectors were merged in PR 13 with this question marked open for the
  maintainer; this records the decision and the evidence behind it.
- Settles: the blocking finding S2 of `docs/readiness/grpc-gateway-parity.md`
  (one `GET` aborted the process), and whether the fix is a permanent deviation
  from grpc-gateway. `abada-adr` asks for an ADR for a deliberate, permanent
  deviation; this one is not forced by a Rust type but by the stack and by
  prost's recursive code, so it is written here and not only in `DESIGN.md`.
- Evidence: `conformance/cases/request-abada-conformance-request-v1.json` (the
  24 `query-depth-*` cases) and its vectors, `crates/abada/tests/request.rs`
  (`DEEPER_THAN_LIMIT`), `crates/abada/tests/security.rs`,
  `crates/abada/src/request/fields.rs` (`MAX_MESSAGE_DEPTH`).

## Context

`?a.b.c=v` names a field path. grpc-gateway v2.27.3 walks it in a loop
(`runtime/query.go`, `populateFieldValueFromPath`), creating each message on the
way with `Mutable`, and has no depth limit: its vectors answer `request` for a
path of 1 000 and of 5 000 components. Go's stack grows, so a deep path costs
memory and nothing else.

abada's first port recursed once per component and runs on a tokio worker with
a 2 MiB stack. A `GET` of 54 KB (9 000 components, under hyper's default header
limit) aborted the whole process: `SIGABRT`, no unwinding, nothing to catch. The
threshold was between 2 000 and 4 000 components in release and between 500 and
1 000 unoptimised, so a debug-built service died on a 7 KB request. `AGENTS.md`
rule 7 (nothing a request controls may panic) was broken by design.

## Options

- **(a) Make the walk a loop, no limit** — what Go does. Tried: the process
  still aborts. The message the loop builds is 4 000 levels deep, and dropping
  it and encoding it for the gRPC call are recursive (`prost-reflect`, prost),
  on the same 2 MiB stack.
- **(b) A loop and a depth limit, 100 levels of message, the root counted** —
  the JSON codec's limit (ADR 0001, `abada::json::DEFAULT_RECURSION_LIMIT`).
  prost, which decodes what abada sends, accepts 101 (measured once, no test).
  The limit bounds every recursion that follows the walk.
- **(c) Run the request on a larger stack** — a thread with a big stack, or a
  growable one. Not built. The stack belongs to the runtime of whoever mounts
  `Gateway` (property 4: a `tower::Service`, not a framework), so abada cannot
  set it, and a bigger stack only moves the threshold.
- **(d) Reject long query strings or many components up front** — a `path.len()`
  check before the walk. Built as a mutation and it fails: it refuses paths
  grpc-gateway answers before the limit is reached (`query-depth-5000-unknown-at-3`,
  an unknown name third in a 5 000-component path, answers 200 in Go).
- **(e) Iterative drop and encode in the dependencies** — removes the reason
  for a limit. Not built: it means changing or forking `prost-reflect`, an ADR
  of its own (dependency of weight), and would still leave a request that costs
  memory in proportion to its depth.

## How it was measured

Correctness: 24 cases run through grpc-gateway's generated handlers
(`scripts/regen-vectors.sh`, CI `--check`), replayed in Rust. Depths 99 to
5 000; scalar, list, map and `Timestamp` last fields; an unknown name and a
scalar in the middle of the path; a parse error and too many values at 100
components. The Rust test asserts the deviating ones still deviate.

Robustness: the 4 000- and 9 000-component paths are in the hostile corpus
(`tests/security.rs`), and one test fixes the answers around the limit through
the real `Gateway`. Mutations, each run and each failing a named vector or test:
the original recursion and the loop without the limit (both abort), limit 101,
the leaf level not counted, a `path.len()` check up front, no descriptor scan
past the limit, a map's entry counted as a level, every map treated as scalar.

Stack, by the security review: a request 100 levels deep overflows at 64 KiB in
release and 256 KiB in debug, so the default 2 MiB has about 20× and 5× margin.
Host load average was 6 to 20 in every timing; they are orders of magnitude.

## Decision

Option (b). The walk is a loop, and a request that would nest past 100 levels of
message (the root is the first; a message-typed last field, the element of a
list of messages or the value of a map of messages is one more; a map's entry
is not a level) is refused with 400, code 3, `exceeded max recursion depth`.
Answers grpc-gateway gives before the limit is reached are kept; past it, a
name that is not a message is still "is not a message", and an unknown name is
the limit's 400. This is a permanent deviation, written in `docs/DESIGN.md`
("Query field paths") and pinned by nine vectors.

## Consequences

- abada refuses input grpc-gateway serves: a query path past 100 levels, an
  unknown name past the limit, and — read in the code, not measured — a path
  variable `{a.b.c…}` of 101 or more components, which now answers 400 on every
  request. A descriptor with such a template could be rejected at build time by
  the generator; it is not.
- The limit makes a query finite in depth, not in cost. Divergent 100-deep keys
  peak at 92× to 125× their size, over a URI or over a form `POST` body, above
  the 10× rule: S5 stays FAIL. A per-request budget on the messages the walk
  builds would bound it and is the follow-up. grpc-gateway's own figure was not
  measured.
- A `Struct` or `Value` last field is filled by the JSON codec; prost decodes at
  most 33 levels of object or 50 of array of it, fewer the deeper the field, and
  the backend's decoder answers 400 beyond that (measured once, no test).
- Reopen this if `prost-reflect` or prost drop and encode without recursion
  (option e becomes free), if a consumer needs a path deeper than 100, or when
  the per-request budget exists and can replace the depth as the bound.

## Not validated

- grpc-gateway's memory and time for the deep and divergent inputs.
- No vector for an unknown name or a list or map in the middle of a path past
  the limit, for a last-field error on a path of 101 or more components, or for
  a path variable of 101 or more components.
- "prost accepts 101" and the Struct/Value limits are single measurements.
- A nested `Any` returned by a backend, a real remote tonic backend, sockets,
  fuzzing.
