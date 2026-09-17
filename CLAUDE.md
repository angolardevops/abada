# CLAUDE.md — abada

@AGENTS.md

## Claude Code specifics

- Skills in `.claude/skills/` and reviewers in `.claude/agents/` are listed in
  §8 of `AGENTS.md`. Load the matching skill before designing; run the matching
  reviewers (Agent tool) before opening a PR.
- `.claude/settings.json` installs a guard hook (`scripts/harness/guard.py`):
  writing to `conformance/vectors/` or `conformance/contracts/*.binpb` is
  refused, and so are `git add -A|.|-u`, `git commit -a`, pushes to `main` or
  of tags (also with `git -C`), `--no-verify`, `gh pr merge`, `gh release
  create` and `cargo publish`. The cases it is tested against are in
  `scripts/harness/guard_cases.json`. A refusal is the harness working — change
  the approach, do not route around it with another tool.
- When several sessions work in parallel, use worktrees outside the repository
  (§5 of `AGENTS.md`), not the shared checkout.
