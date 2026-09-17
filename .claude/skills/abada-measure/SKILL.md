---
name: abada-measure
description: How abada measures cost and readiness — benchmarks against grpc-gateway on the same inputs with host load recorded, repeated runs and honest noise statements; and readiness reports (docs/readiness/<contract>.md) that answer "can abada serve this consumer?" with counted needs, a verdict and what was not validated. Use when benchmarking, when a change may move hot-path cost, when an ADR needs numbers, or when a consumer contract's readiness changes. Not for correctness vectors (`abada-conformance`).
---

# Measuring in abada

A number without its conditions is an opinion. A readiness report without the
"not validated" section is marketing.

## Benchmarks

**Compare like with like.** The Go side runs grpc-gateway's code on the same
inputs (`abadaoracle bench`, `abadaoracle json-bench`); the Rust side runs the
same requests or bodies. State what is not comparable (e.g. Go with dynamic
messages vs generated types).

**Record the conditions**, in the ADR or report:

- CPU model and thread count;
- `uptime` load average **before and after every run**;
- release profile, toolchain versions, library versions;
- number of runs (≥ 3) and which statistic (median);
- allocations when cheap to get.

**State the noise.** Give the spread between runs. Claim only differences
larger than it. On a shared or loaded host, the honest result is often "same
order of magnitude". Pinning to a core that is not reserved can make it worse;
if you pin, say so and compare.

**Separate one-time from per-request cost** (descriptor pool build, route
registration vs per-request work).

**Keep raw logs out of `/tmp`** if they back a committed number; the
summarised table goes in the document.

Where benchmarks live: `crates/*/examples/*_bench.rs` for small ones,
`benches/<name>/` (`publish = false`) when a comparison needs its own
dependencies. The runtime crate never gains a dependency for a benchmark.

## Readiness reports

`docs/readiness/<contract>.md` answers one question for one consumer, measured
on a date against a pinned commit of the consumer's contract
(`conformance/contracts/<name>.binpb` + `.source`).

Structure (see `delonix-node-v1.md`):

1. **Verdict** in the first lines: "not yet", "yes, for X", "yes". Never "almost".
2. **What the contract asks for**, counted by `abadaoracle inventory`, not
   estimated — a table: need, how much of the contract, abada's state, proof.
3. **Cost** if measured, with conditions.
4. **Found in the contract** — properties of the consumer's contract that
   matter (e.g. routing that depends on declaration order), reported to its
   owner, not changed here.
5. **Not validated.**

Update the report in the same PR that changes a row's state. A row moves to
done only with its proof filled in.
