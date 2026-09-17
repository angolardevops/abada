# AGENTS.md — abada

The operating manual for anyone — human or AI agent — who changes this
repository. It is read by Codex, Copilot, Cursor, Gemini and Claude Code
(`CLAUDE.md`, `GEMINI.md` and `.github/copilot-instructions.md` point here).
When this file and a tool-specific file disagree, **this file wins** and the
other one is a bug.

`scripts/check-harness.py` keeps this file honest: the crate table, the ADR
index and every relative link are checked in CI.

---

## 1. What abada is

REST/JSON gateways for [tonic](https://github.com/hyperium/tonic) gRPC services,
generated from `google.api.http` annotations — the Rust counterpart of
[grpc-gateway](https://github.com/grpc-ecosystem/grpc-gateway), which is the
reference, the inspiration and the oracle.

It exists only because of four properties **together**, each one a test, not a
claim ([docs/DESIGN.md](docs/DESIGN.md)):

1. **Behaviour-compatible with grpc-gateway v2.27.3** — proven by vectors that
   grpc-gateway's own code produced.
2. **Two entry points, one generator** — `protoc-gen-abada` and `abada-build`
   both call `abada-codegen`.
3. **In-process and proxy** — the gateway calls the service implementation
   directly or a remote `tonic::transport::Channel`.
4. **A `tower::Service`, not a framework** — axum is a convenience layer.

A change that weakens one of these is not a feature; it needs an ADR (§6).

## 2. Map

| Crate | Role | May depend on |
|---|---|---|
| `abada` | runtime used by generated code: routing, transcoding, errors, streaming | `http`, `tower`, `prost`, `prost-reflect` (ADR 0001) — never `axum`, never `abada-codegen` |
| `abada-codegen` | `FileDescriptorSet` → `HttpRule`s → Rust code; pure, no I/O besides what it is handed | `abada`, `prost` |
| `abada-build` | `build.rs` API in the style of `tonic-build` | `abada-codegen` |
| `protoc-gen-abada` | protoc/buf plugin: `CodeGeneratorRequest` on stdin, response on stdout | `abada-codegen` |
| `abada-json-bench` | `benches/json-transcode`, evidence for ADR 0001; `publish = false` | anything it measures |

The arrow never points back: `abada` does not know code generation exists.
Details and the rules for public API live in the
[`abada-architecture`](.claude/skills/abada-architecture/SKILL.md) skill.

| Path | What it holds | Who writes it |
|---|---|---|
| `conformance/cases/*.json` | inputs you choose | you, by hand |
| `conformance/oracle/*.go` | the Go program that runs grpc-gateway's code over the cases | you, by hand |
| `conformance/vectors/*.json` | grpc-gateway's answers | **only `scripts/regen-vectors.sh`** — never edited |
| `conformance/contracts/*.binpb` | real descriptor sets (with a `.source` naming the commit) | `protoc`/`buf`, never edited |
| `docs/DESIGN.md` | the design, the Progress table, the written deviations | you, in the same PR as the code |
| `docs/adr/NNNN-*.md` | decisions with alternatives and evidence | you (§6) |
| `docs/readiness/<contract>.md` | can abada serve this consumer, measured | you, when the answer changes |

## 3. The method — how anything gets in

Every behaviour abada claims to share with grpc-gateway enters the same way.
The full procedure is the [`abada-conformance`](.claude/skills/abada-conformance/SKILL.md)
skill; the short form:

1. **Read grpc-gateway's source** at the pinned tag (the checkout lives in
   `~/.cache/abada-ref/grpc-gateway-v2.27.3`). Not its docs, not memory.
2. **Write cases** in `conformance/cases/`, including the ugly ones.
3. **Let grpc-gateway answer** through `conformance/oracle/` and
   `scripts/regen-vectors.sh`.
4. **Replay the vectors** in a Rust test.
5. **Break the code on purpose** and see the test fail. A test that stays green
   under a mutation proves nothing; add the case that catches it.
6. **Write what differs** in `docs/DESIGN.md` as a named deviation, with the
   vectors that show it listed in the test.

CI re-asks grpc-gateway (`regen-vectors.sh --check`), so a hand-edited or
stale vector cannot make the Rust tests pass.

## 4. Rules that are not negotiated

1. **Proof is measured, not asserted.** "It compiles", "the command returned 0"
   or "it looks like grpc-gateway" close nothing. The proof is a vector, a test
   that failed under mutation, or a number with how it was measured.
2. **Report both halves.** Every PR and every final report says what was proven
   AND what was not validated, explicitly. A report with only the good half is
   a defect.
3. **Vectors are never edited by hand**, and never regenerated to make a test
   pass without understanding why they changed.
4. **grpc-gateway's behaviour wins over the proto3 spec, over other Rust crates
   and over taste.** Where abada cannot or should not match it, the deviation is
   written down (DESIGN.md) and pinned by a test.
5. **Docs move with code.** A PR that changes behaviour updates the Progress
   table of `docs/DESIGN.md`, and the readiness report when the answer for a
   consumer changes. A field or option that is accepted and ignored is worse
   than one that does not exist.
6. **No structural change without an ADR** — new crate, new dependency of
   weight in `abada`, a change to one of the four properties, a v0.1 scope item
   moved in or out ([`abada-adr`](.claude/skills/abada-adr/SKILL.md)).
7. **No `unwrap`/`expect`/`panic!` on a path reachable from a request.**
   Request input is hostile. Tests and build scripts may unwrap.
8. **Numbers carry their conditions.** A benchmark states the host, its load
   average before and after, runs, and what is NOT comparable
   ([`abada-measure`](.claude/skills/abada-measure/SKILL.md)).
9. **Agents do not merge, do not push to `main`, do not publish** (crates.io,
   GitHub Pages, tags). Those are maintainer decisions.

## 5. Workflow

```bash
scripts/check.sh               # everything CI runs: fmt, clippy -D warnings, tests, vectors --check
scripts/check.sh --no-oracle   # without Go; prints what was NOT validated and exits non-zero
scripts/regen-vectors.sh       # regenerate vectors after changing cases or the oracle
scripts/check-harness.py       # this file, the skills and the agents are consistent
```

Toolchain: Rust `1.85` (edition 2024, see `Cargo.toml`), Go `1.26.2` for the
oracle, `protoc` when a case needs a descriptor set.

**Branches and parallel work.** One branch per task, from `origin/main`. When
several agents work on the same machine, each one gets its own
`git worktree` in a directory **outside the repository** (a worktree inside the
checkout gets picked up by `cargo`, `grep` and `git add`) and one that
**survives a reboot** (not `/tmp`). Use absolute paths with `git -C`. Stage
files by name: `git add <file>...`, never `-A`, `.` or `-u`. Check
`git branch --show-current` before every commit. Commit as soon as a batch is
verified.

**The Go checkout is shared.** `regen-vectors.sh` copies the oracle into
`~/.cache/abada-ref`; two runs at the same time corrupt each other. Run it under
a lock when more than one agent is working:
`flock ~/.cache/abada-ref/.lock scripts/regen-vectors.sh`.

**Commits** are small and say why. **PRs** use
[the template](.github/pull_request_template.md): proof, not validated, docs
touched.

## 6. Decisions

| ADR | Decision |
|---|---|
| [0001](docs/adr/0001-transcodificacao-json.md) | JSON is transcoded with `prost-reflect` `DynamicMessage`; five deviations to close before claiming JSON compatibility |

An ADR is superseded by another ADR, never by an edit to a skill or to this
file.

## 7. Facts that cost time to learn

- **grpc-gateway tries the handler registered LAST first.** Declaration order
  in a `.proto` changes routing (`GetOperation` vs `WatchOperation` in
  `delonix.node.v1`). abada reproduces it; do not "fix" it.
- **A wrong method is 501 (code 12), not 405.** A malformed escape is 400 with
  code 2, and routing continues into the same response.
- **grpc-gateway's JSON bytes are not stable across Go builds**: protojson adds
  a space after commas depending on a hash of the binary (`internal/detrand`).
  The oracle normalises it; never compare raw protojson bytes from two builds.
- **The default marshaler emits unpopulated fields** (`EmitUnpopulated: true`,
  `null` for unset messages) and discards unknown fields and unknown enum names.
- **The 405 fallback walks a Go map**: its order is random. The oracle drops
  cases whose answer depends on it.
- **Binding values in vectors went through Go's JSON encoder**, which replaces
  invalid UTF-8.
- **`http::HeaderValue` cannot hold control bytes** that Go writes raw over
  HTTP/1.1; abada drops the header (written deviation).
- **A benchmark on a loaded host proves an order of magnitude and nothing
  finer.** Pinning to a core that is not reserved made results worse.

## 8. Skills and reviewers

Skills are procedures; reviewers are agents that check a diff against them.
Any tool can read them as plain Markdown.

| Skill | Use it when |
|---|---|
| [`abada-architecture`](.claude/skills/abada-architecture/SKILL.md) | adding a module, a public type, a dependency, or deciding which crate code belongs in |
| [`abada-conformance`](.claude/skills/abada-conformance/SKILL.md) | implementing or changing any behaviour grpc-gateway also has |
| [`abada-adr`](.claude/skills/abada-adr/SKILL.md) | a change touches a boundary, a dependency of weight, the four properties or the v0.1 scope |
| [`abada-measure`](.claude/skills/abada-measure/SKILL.md) | benchmarking, or writing/updating a readiness report for a consumer |

| Reviewer | Checks |
|---|---|
| [`abada-conformance-reviewer`](.claude/agents/abada-conformance-reviewer.md) | every claimed behaviour has vectors, a mutation check and written deviations |
| [`abada-rust-reviewer`](.claude/agents/abada-rust-reviewer.md) | Rust correctness: panics on request paths, allocation in hot paths, API surface, MSRV, unsafe |
| [`abada-design-guardian`](.claude/agents/abada-design-guardian.md) | crate boundaries, scope, ADR needed, docs moved with the code, both halves reported |

Before opening a PR, run the reviewers that match the diff. Their findings go
in the PR or get fixed; they are not optional reading.
