# AGENTS.md — abada

The operating manual for anyone — human or AI agent — who changes this
repository. It is read by Codex, Copilot, Cursor, Gemini and Claude Code
(`CLAUDE.md`, `GEMINI.md` and `.github/copilot-instructions.md` point here).
When this file and a tool-specific file disagree, **this file wins** and the
other one is a bug.

`scripts/check-harness.py` checks in CI that the crate table (names and
dependencies), the ADR index, the skill and reviewer lists and every relative
link match the repository. The prose rules and the facts in §7 are not
machine-checked; the reviewers in §8 check them.

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
| `abada` | runtime used by generated code: routing, transcoding, errors, streaming | `http`; `prost-reflect`, `prost-types`, `serde`, `serde_json` (ADR 0001); `prost`, `bytes`, `tower` (`features = ["util"]` — `Gateway` implements it, tests use `service_fn` to build a codegen-free double), `http-body`, `http-body-util` (property 4 — `Gateway`'s `tower::Service` reads/writes bodies); `tonic` pinned `=0.14.5`, `default-features = false`, `features = ["codegen"]` only (ADR 0002 — never `tonic-build`, `tonic-prost-build`, `tonic-prost`, and never a `tonic` version whose own MSRV is measured above 1.85) — never `axum`, never `abada-codegen` |
| `abada-codegen` | `FileDescriptorSet` → `HttpRule`s → Rust code; pure, no I/O besides what it is handed | `abada`, `prost` |
| `abada-build` | `build.rs` API in the style of `tonic-build` | `abada-codegen` |
| `protoc-gen-abada` | protoc/buf plugin: `CodeGeneratorRequest` on stdin, response on stdout | `abada-codegen` |
| `abada-json-bench` | `benches/json-transcode`, evidence for ADR 0001; `publish = false` | anything it measures |

The arrow never points back: `abada` does not know code generation exists.
The "May depend on" column is the dependency allow-list: a normal dependency
not named there fails `scripts/check-harness.py` until the table (and, for a
dependency of weight, an ADR) says so. Details and the rules for public API
live in the
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
7. **Nothing a request or a user's descriptor controls may panic.** No
   `unwrap`, `panic!` or unchecked indexing on those paths; `expect` only for
   an invariant the same function establishes, stated in the message. Tests
   and build scripts may unwrap.
8. **Numbers carry their conditions.** A benchmark states the host, its load
   average before and after, runs, and what is NOT comparable
   ([`abada-measure`](.claude/skills/abada-measure/SKILL.md)).
9. **Agents do not merge, do not push to `main`, do not publish** (crates.io,
   GitHub Pages, tags). Those are maintainer decisions.

## 5. Workflow

```bash
scripts/check.sh               # everything CI runs: fmt, clippy, tests, MSRV, harness, vectors --check
scripts/check.sh --no-oracle   # without Go; still runs the rest, lists what was NOT validated, exits non-zero
scripts/harness/install-git-hooks.sh  # git-level guards for any tool (no push to main, no tags)
scripts/regen-vectors.sh       # regenerate vectors after changing cases or the oracle
scripts/check-harness.py       # this file, the skills and the agents are consistent
```

Toolchain: Rust stable for development and `1.85` as the MSRV (edition 2024;
CI builds with both), Go `1.26.2` for the oracle, Python 3.11+ for the harness
check, `protoc` when a case needs a descriptor set.

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
`flock "${ABADA_REF_DIR:-$HOME/.cache/abada-ref}/.lock" scripts/regen-vectors.sh`
(`scripts/check.sh` does this for you).

**Commits** are small and say why. **PRs** use
[the template](.github/pull_request_template.md): proof, not validated, docs
touched.

## 6. Decisions

| ADR | Decision |
|---|---|
| [0001](docs/adr/0001-transcodificacao-json.md) | JSON is transcoded with `prost-reflect` `DynamicMessage`; five deviations to close before claiming JSON compatibility |
| [0002](docs/adr/0002-chamada-do-rpc-in-process-e-proxy.md) | in-process and proxy calls are one code path, `tonic::client::Grpc<S: GrpcService>`, over a descriptor-driven codec; abada's MSRV vs `tonic-prost-build` is a blocking open question before `tonic` enters `abada`'s own dependencies |

An ADR is superseded by another ADR, never by an edit to a skill or to this
file.

## 7. Facts that cost time to learn

- **grpc-gateway tries the handler registered LAST first.** Declaration order
  in a `.proto` changes routing (`GetOperation` vs `WatchOperation` in
  `delonix.node.v1`). abada reproduces it; do not "fix" it.
- **A path that matches with the wrong method is answered 501 (code 12), not
  405.** A malformed escape is 400 with code 2, and routing continues into the
  same response.
- **grpc-gateway's JSON bytes are not stable across Go builds**: protojson adds
  a space after commas depending on a hash of the binary (`internal/detrand`).
  The oracle normalises it; never compare raw protojson bytes from two builds.
- **The default marshaler emits unpopulated fields** (`EmitUnpopulated: true`,
  `null` for unset messages) and discards unknown fields and unknown enum names.
- **The method-mismatch fallback walks a Go map**: its order is random. The
  oracle drops cases whose answer depends on it.
- **Binding values in vectors went through Go's JSON encoder**, which replaces
  invalid UTF-8.
- **`http::HeaderValue` cannot hold control bytes** that Go writes raw over
  HTTP/1.1; abada drops the header (written deviation).
- **A benchmark on a loaded host proves an order of magnitude and nothing
  finer.** Pinning to a core that is not reserved made results worse.
- **A server-side `Codec` swaps encode/decode relative to the client's**:
  the server encodes the response and decodes the request. Getting this
  backwards only fails when request and response are different message
  types — with the same type (as one test's fixture happened to use) it
  silently works, for the wrong reason.
- **`-> impl Trait` does not, by itself, prove an associated type of that
  trait is `Send`**, even when the concrete type's really is. A function
  returning a hand-written `impl GrpcService<...>` needs the bound written
  as `Future: Send` inside the `impl Trait` (an associated-type bound), not
  left implicit, or a caller requiring `S::Future: Send` (as `Gateway`'s
  `tower::Service` impl does) gets an opaque, hard-to-place `Send` error.

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

**Mechanical guards.** Claude Code loads `.claude/settings.json`, whose hook
(`scripts/harness/guard.py`) refuses editing generated files, broad `git add`,
`git commit -a`, pushing to `main` or tags, `--no-verify`, merging, releasing
and publishing. Other tools get the git-level part through
`scripts/harness/install-git-hooks.sh`. Both are seatbelts, not sandboxes:
CI and branch protection are the backstop.
