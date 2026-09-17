# Copilot instructions — abada

Read and follow [AGENTS.md](../AGENTS.md). It is the single source of
instructions for this repository. The procedures it references live in
`.claude/skills/*/SKILL.md` and are plain Markdown.

The three mistakes that cost the most here:

1. Claiming grpc-gateway compatibility without vectors produced by grpc-gateway
   itself (`conformance/`, `scripts/regen-vectors.sh`).
2. Editing `conformance/vectors/*.json` by hand — they are generated.
3. Reporting only what works. Every change says what was proven and what was
   not validated.
