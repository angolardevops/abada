# Contributing to abada

Thank you for helping. abada has one promise that everything else serves:
**it behaves like [grpc-gateway](https://github.com/grpc-ecosystem/grpc-gateway)**,
and that promise is kept by measurement, not by review.

The full operating manual — for people and for AI agents alike — is
[AGENTS.md](AGENTS.md). Read §3 (the method) and §4 (the rules) before your
first change.

## In short

1. **Open an issue first** for anything bigger than a fix, so scope and design
   can be checked against [docs/DESIGN.md](docs/DESIGN.md) before you write it.
2. **Behaviour comes from grpc-gateway's source** at the pinned tag, is captured
   as vectors by the Go oracle in `conformance/`, and is replayed by Rust tests.
   The procedure is in [`abada-conformance`](.claude/skills/abada-conformance/SKILL.md).
3. **Break your code on purpose** and show the tests fail.
4. **Run `scripts/check.sh`.** It runs what CI runs and tells you what it could
   not validate.
5. **Fill in the PR template**, including "Not validated".

## Using AI assistants

You are welcome to. The repository ships a harness so the assistant works
within the design instead of around it:

- `AGENTS.md` — instructions read by Codex, Copilot, Cursor, Gemini and
  Claude Code;
- `.claude/skills/` — procedures (architecture, conformance, ADRs, measuring);
- `.claude/agents/` — reviewers to run on your diff before opening the PR;
- `.claude/settings.json` + `scripts/harness/guard.py` — guards that refuse
  editing generated vectors, broad `git add`, pushing to `main`, merging and
  publishing.

You are responsible for what the assistant writes. A PR is judged by its proof,
not by who or what typed it.

## Toolchain

- Rust 1.85+ (edition 2024)
- Go 1.26.2 for the conformance oracle
- `protoc` when a case needs a descriptor set
- Python 3.11+ for `scripts/check-harness.py`

## License

By contributing you agree that your contributions are licensed under the
Apache License 2.0, like the rest of the project.
