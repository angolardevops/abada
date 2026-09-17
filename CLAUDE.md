# CLAUDE.md — abada

@AGENTS.md

## Claude Code specifics

- Skills in `.claude/skills/` and reviewers in `.claude/agents/` are listed in
  §8 of `AGENTS.md`. Load the matching skill before designing; run the matching
  reviewers (Agent tool) before opening a PR.
- `.claude/settings.json` installs guard hooks (`scripts/harness/`): editing
  `conformance/vectors/` or `conformance/contracts/` with Edit/Write is refused,
  and so are `git add -A|.|-u`, pushes to `main`, `--no-verify` and
  `gh pr merge`. A refusal is the harness working — change the approach, do not
  route around it with another tool.
- When several sessions work in parallel, use worktrees outside the repository
  (§5 of `AGENTS.md`), not the shared checkout.
