#!/usr/bin/env python3
"""PreToolUse guard for AI agents working on abada (see AGENTS.md §4, §5).

Reads the Claude Code hook payload on stdin. Exit 2 refuses the tool call and
the message on stderr is shown to the agent; exit 0 lets it through. Other
harnesses can call it the same way with {"tool_name", "tool_input"}.
"""
import json
import re
import sys

GENERATED = ("conformance/vectors/", "conformance/contracts/")

BASH_RULES = [
    (re.compile(r"\bgit\s+add\b[^;&|]*(\s-A\b|\s--all\b|\s-u\b|\s--update\b|\s\.(\s|$))"),
     "stage files by name: `git add <file>...` (never -A, --all, -u or .)"),
    (re.compile(r"\bgit\s+push\b[^;&|]*\s(\S+:)?(refs/heads/)?main(\s|$)"),
     "never push to main; push your branch and open a PR"),
    (re.compile(r"--no-verify\b"),
     "hooks and checks are not skipped"),
    (re.compile(r"\bgh\s+pr\s+merge\b"),
     "merging is a maintainer decision, not an agent's"),
    (re.compile(r"\bcargo\s+publish\b(?![^;&|]*--dry-run)"),
     "publishing is a maintainer decision, not an agent's"),
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
        if any(p in path for p in GENERATED):
            refuse(f"{path} is generated; change conformance/cases or "
                   "conformance/oracle and run scripts/regen-vectors.sh")
        return

    if tool == "Bash":
        cmd = str(args.get("command", ""))
        for rule, reason in BASH_RULES:
            if rule.search(cmd):
                refuse(reason)
        if "regen-vectors.sh" not in cmd and any(p in cmd for p in GENERATED):
            if re.search(r"sed\s+-i|>\s*\S*conformance/(vectors|contracts)/|\btee\b|\bmv\b[^;&|]*conformance/(vectors|contracts)/|\bcp\b[^;&|]*conformance/(vectors|contracts)/", cmd):
                refuse("conformance/vectors and conformance/contracts are generated; "
                       "use scripts/regen-vectors.sh or protoc")


if __name__ == "__main__":
    main()
