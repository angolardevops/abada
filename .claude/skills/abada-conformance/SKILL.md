---
name: abada-conformance
description: How a behaviour enters abada — read grpc-gateway v2.27.3's source, write cases, let the Go oracle answer, replay the vectors in Rust, break the code on purpose, write the deviations. Use for ANY change to routing, path templates, query/path/body population, JSON, errors, headers/metadata, streaming or anything else grpc-gateway also does. Also use when a conformance test or `regen-vectors.sh --check` fails. Not for pure refactors with no behaviour change (still run the suite), nor for benchmarks (`abada-measure`).
---

# Conformance: grpc-gateway is the oracle

abada's first property is "behaves like grpc-gateway". That sentence is only
true where a vector produced by grpc-gateway's own code says so. Everything else
is a hope.

## 0. Before writing code

- Find the Go code that implements the behaviour at the pinned tag
  (`GATEWAY_TAG` in `scripts/regen-vectors.sh`, today `v2.27.3`). The checkout
  is `~/.cache/abada-ref/grpc-gateway-v2.27.3`; `scripts/regen-vectors.sh`
  clones it if missing. Typical places:
  - `runtime/mux.go` — `ServeMux`, routing, method override, fallback
  - `runtime/pattern.go`, `internal/httprule/` — templates
  - `runtime/errors.go`, `runtime/handler.go` — errors, streaming
  - `runtime/query.go`, `runtime/convert.go`, `runtime/fieldmask.go` — query,
    path values, `FieldMask`
  - `runtime/marshal_jsonpb.go`, `runtime/marshaler_registry.go` — JSON defaults
  - `protoc-gen-grpc-gateway/internal/gengateway/template.go` — what the
    GENERATED handler does (order of body/path/query, filters, `update_mask`)
- Write down, in your notes, the exact behaviour including the surprising parts.
  If you are reasoning from memory of grpc-gateway, stop and read the source.

## 1. Cases

`conformance/cases/<area>.json` (or `<area>-<contract>.json` for a real
contract). Cases are inputs only — never expected outputs. Include:

- the canonical request of every binding involved;
- boundaries: empty, missing, repeated, nested, escaped, non-ASCII, invalid
  UTF-8, too long, wrong type, duplicate;
- what Go rejects, and what Go accepts that you would have rejected;
- for randomised coverage, a fixed seed in the case file (see `path.json`).

Prefer a real contract (`conformance/contracts/*.binpb`) when the behaviour
depends on message shapes. For kinds a contract lacks, add a small proto under
`conformance/protos/` and commit its descriptor set with the `protoc` command
that produced it.

## 2. Oracle

`conformance/oracle/*.go` is `package main` of a program copied into the
grpc-gateway checkout (it needs `internal/` packages). One subcommand per area,
dispatched in `main.go`, one file per area. Rules:

- Call grpc-gateway's real code: a real `runtime.ServeMux`, the real default
  marshaler taken from it, generated handlers when the behaviour lives in
  generated code, a real `net/http` server when bytes on the wire matter.
  **Do not re-implement grpc-gateway in Go**; that tests your transcription.
- Record what an HTTP client would see (status, headers, body, trailers) or
  what the gRPC server received (the request message).
- Normalise only what is provably unstable, and say why in a comment
  (e.g. protojson's `detrand` comma spacing).
- Drop, don't guess, cases whose answer is non-deterministic in Go (map
  iteration order), and record them under `dropped_nondeterministic`.
- Record the generator (`grpc-gateway <tag>, <go version>`).

Add the area to `scripts/regen-vectors.sh` as a separate block with the same
`--check` shape as the existing ones.

## 3. Vectors

```bash
flock "${ABADA_REF_DIR:-$HOME/.cache/abada-ref}/.lock" scripts/regen-vectors.sh
```

Read the diff of `conformance/vectors/` before committing it. A vector that
changed and you cannot explain is a finding, not noise. Vectors are never
edited by hand (the Claude Code hook refuses it).

## 4. Replay in Rust

A test in the crate that owns the behaviour (`crates/abada/tests/<area>.rs`)
that loads the vectors with `serde_json` and compares **everything** the vector
records. On mismatch, print the case name, the input and both answers — the
failure message is how the next person debugs it.

Known deviations are listed **by case name** in the test and asserted to still
deviate, so closing a gap forces the list to be updated.

## 5. Mutation check — mandatory

Break the implementation on purpose, one thing at a time, and run the test:
a mapping entry, an ordering rule, an escape, an off-by-one in a tail, a
default. Each break must make the test fail. When one does not, add the case
that catches it and say so in the PR ("X was not caught until case Y").

Record in the PR which mutations were tried and the result.

## 6. Write it down

- `docs/DESIGN.md`: Progress row (state + proof with vector counts); a section
  for non-obvious behaviour; every **deviation** named, why, and which vectors
  show it.
- `docs/readiness/<contract>.md` if a consumer's answer changed.
- The PR: proven / not validated (the template has the sections).

## Failure modes seen in this repo

- A tail case only caught once requests shorter than the fixed tail were added.
- Uppercasing the first hex digit of a control-char escape changed nothing
  (always `0`/`1`); the mutation had to hit the second digit.
- An agent's brief said the default marshaler omits unpopulated fields; the
  source said `EmitUnpopulated: true`. The source wins; correct the brief.
- A `.go` file left in the shared cache from another branch kept compiling.
- Relative paths with `git -C` created a worktree inside the repository.

## Done means

- [ ] Go source read at the pinned tag; surprising behaviour written down
- [ ] cases include edge and invalid inputs
- [ ] vectors generated by the oracle, diff understood
- [ ] Rust test replays every recorded field
- [ ] mutation check done and reported
- [ ] deviations named in DESIGN.md and pinned in the test
- [ ] `scripts/check.sh` green, including `regen-vectors.sh --check`
