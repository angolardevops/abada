#!/usr/bin/env python3
"""PreToolUse guard for AI agents working on abada (see AGENTS.md §4, §5).

Reads the Claude Code hook payload on stdin. Exit 2 refuses the tool call and
the message on stderr is shown to the agent; exit 0 lets it through. Other
harnesses can call it the same way with {"tool_name", "tool_input"}.

It is a seatbelt, not a sandbox: it catches the usual forms of the mistakes
AGENTS.md forbids. A determined script can write anywhere; CI
(`regen-vectors.sh --check`, branch protection) is the backstop.
"""
import json
import re
import sys

# Generated files. `conformance/contracts/*.source` is hand-written provenance.
GENERATED = re.compile(r"conformance/(vectors/[^\s'\"]*|contracts/[^\s'\"]*\.binpb)")

# `git` followed by global options (-C <dir>, -c <k=v>, --git-dir=…, …).
GIT = r"\bgit(?:\s+(?:-[Cc]\s+\S+|--[a-z-]+(?:=\S+)?))*\s+"

BASH_RULES = [
    (re.compile(GIT + r"add\b[^;&|]*?(\s-[a-zA-Z]*[Au][a-zA-Z]*\b|\s--all\b|\s--update\b|\s\.(/)?(?=\s|$))"),
     "stage files by name: `git add <file>...` (never -A, --all, -u or .)"),
    (re.compile(GIT + r"commit\b[^;&|]*?\s-[a-zA-Z]*a[a-zA-Z]*\b"),
     "stage files by name, then commit (never `git commit -a`)"),
    (re.compile(GIT + r"push\b[^;&|]*?\s\+?(\S+:)?(refs/heads/)?main(?=\s|$)"),
     "never push to main; push your branch and open a PR"),
    (re.compile(GIT + r"push\b[^;&|]*?(\s--tags\b|\s--follow-tags\b|\srefs/tags/|\sv\d+\.\d+)"),
     "tags are published by maintainers"),
    (re.compile(r"--no-verify\b"),
     "hooks and checks are not skipped"),
    (re.compile(r"\bgh\s+pr\s+merge\b"),
     "merging is a maintainer decision, not an agent's"),
    (re.compile(r"\bgh\s+release\s+create\b"),
     "releases are published by maintainers"),
    (re.compile(r"\bcargo(\s+\+\S+)?\s+publish\b(?![^;&|]*--dry-run)"),
     "publishing is a maintainer decision, not an agent's"),
]

# Ways a shell command writes to a path. Each must name the generated path as
# its target, so reading (`grep`, `git diff`, `cat`) stays allowed.
TARGET = r"[^\s;&|]*" + GENERATED.pattern
WRITES = [
    re.compile(r"sed\s+(-[a-zA-Z]*i|--in-place)[^;&|]*" + TARGET),
    re.compile(r">>?\s*" + TARGET),
    re.compile(r"(^|[|;&]\s*)tee\s+(-a\s+)?" + TARGET),
    re.compile(r"\b(mv|cp|install|ln)\b[^;&|]*\s" + TARGET + r"\s*($|[;&|])"),
    re.compile(r"\btruncate\b[^;&|]*" + TARGET),
]


def refuse(reason: str) -> None:
    print(f"abada harness: refused — {reason}. See AGENTS.md.", file=sys.stderr)
    sys.exit(2)


def main() -> None:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, ValueError):
        return
    tool = payload.get("tool_name", "")
    args = payload.get("tool_input", {}) or {}

    if tool in ("Edit", "Write", "MultiEdit", "NotebookEdit"):
        path = str(args.get("file_path") or args.get("notebook_path") or "")
        if GENERATED.search(path):
            refuse(f"{path} is generated; change conformance/cases or "
                   "conformance/oracle and run scripts/regen-vectors.sh")
        return

    if tool == "Bash":
        cmd = str(args.get("command", ""))
        # Flags inside a quoted commit message are not flags.
        bare = re.sub(r"'[^']*'|\"(?:[^\"\\]|\\.)*\"", "''", cmd)
        for rule, reason in BASH_RULES:
            if rule.search(bare):
                refuse(reason)
        if "regen-vectors.sh" in cmd:
            return
        if any(w.search(cmd) for w in WRITES):
            refuse("conformance/vectors and conformance/contracts/*.binpb are generated; "
                   "use scripts/regen-vectors.sh or protoc")


if __name__ == "__main__":
    main()
