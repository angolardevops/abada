---
name: abada-adr
description: When and how to write an Architecture Decision Record for abada — triggers (new crate, dependency of weight in the runtime, change to one of the four design properties, v0.1 scope moved, an open decision settled by evidence), the format used by docs/adr/0001, numbering, superseding, and how the decision is reflected in DESIGN.md and AGENTS.md. Use when a diff touches a boundary or when someone says "ADR", "decision", "should we use X or Y". Not for routine features inside existing boundaries (`abada-architecture`).
---

# ADRs in abada

A decision with alternatives and structural consequences is written down with
its evidence, so the next contributor does not re-open it by preference.

## Write one when

- a crate is added, removed, split or merged;
- a dependency of weight enters `abada` (runtime), or `unsafe` does;
- one of the four properties in DESIGN.md is weakened or reinterpreted;
- an item of the v0.1 In/Out scope moves;
- a decision DESIGN.md leaves open is settled;
- a deliberate, permanent deviation from grpc-gateway is chosen (a small
  deviation forced by a Rust type is written in DESIGN.md instead).

Do **not** write one for a bug fix, a behaviour that only follows grpc-gateway,
or a refactor inside a module.

## Settle with evidence, not preference

If the decision can be measured, measure first (`abada-measure`), and compare
candidates on **correctness against the oracle** as well as cost. ADR 0001 is
the model: pbjson was faster and lost because it produced the wrong JSON.

If the evidence is inconclusive, the ADR says so, states what would settle it,
and the status stays `proposed`. Do not force a verdict.

## File

`docs/adr/NNNN-<slug>.md`, next free number, four digits. English content.

```markdown
# ADR NNNN — <decision as a question or statement>

- Status: proposed | accepted, YYYY-MM-DD | superseded by NNNN
- Settles: <the open decision or change it answers>
- Evidence: <paths to benches, cases, vectors>

## Context
What forces the decision. What grpc-gateway does, read in its source.
What the first consumer needs, counted.

## Options
Each real alternative, with versions.

## How it was measured
Correctness method, performance method, host and load conditions.

## Results
Tables. What differs and by how much, with noise stated.

## Decision
One paragraph.

## Consequences
What becomes true, what debt is taken, what must be done before a claim can
be made, and when to reopen.

## Not validated
Explicitly.
```

## After accepting

- DESIGN.md: replace the open question with a pointer to the ADR; update Progress.
- AGENTS.md §6: add a row (`scripts/check-harness.py` fails otherwise).
- If a skill's rules change because of it, update the skill in the same PR.

## Superseding

A new ADR supersedes an old one; the old one gets
`Status: superseded by NNNN` and is otherwise left intact. Skills and AGENTS.md
never overrule an ADR.
