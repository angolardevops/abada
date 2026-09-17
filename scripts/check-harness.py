#!/usr/bin/env python3
"""Checks that the contributor harness matches the repository.

- every relative Markdown link in the harness and docs resolves;
- skills and reviewer agents have frontmatter whose name matches the file;
- AGENTS.md lists every workspace member, skill, reviewer and ADR, and nothing
  that does not exist, and every normal dependency is in its crate's allow-list;
- the guard hook refuses and allows what scripts/harness/guard_cases.json says.
"""
import json
import pathlib
import re
import subprocess
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
errors: list[str] = []


def err(msg: str) -> None:
    errors.append(msg)


def frontmatter(path: pathlib.Path) -> dict[str, str]:
    text = path.read_text()
    m = re.match(r"---\n(.*?)\n---\n", text, re.S)
    if not m:
        err(f"{path.relative_to(ROOT)}: missing frontmatter")
        return {}
    out = {}
    for line in m.group(1).splitlines():
        k, sep, v = line.partition(":")
        if sep and not line.startswith(" "):
            out[k.strip()] = v.strip()
    return out


def check_links(files: list[pathlib.Path]) -> None:
    link = re.compile(r"\[[^\]]*\]\(([^)\s]+)\)")
    for f in files:
        text = f.read_text()
        # links inside fenced code blocks are examples, not links
        text = re.sub(r"```.*?```", "", text, flags=re.S)
        for target in link.findall(text):
            if re.match(r"[a-z]+:", target) or target.startswith("#"):
                continue
            path = target.split("#", 1)[0]
            if not (f.parent / path).exists():
                err(f"{f.relative_to(ROOT)}: broken link {target}")


def main() -> int:
    agents_md = (ROOT / "AGENTS.md").read_text()

    md_files = [ROOT / n for n in ("AGENTS.md", "CLAUDE.md", "GEMINI.md", "CONTRIBUTING.md", "README.md")]
    md_files += [ROOT / ".github/copilot-instructions.md", ROOT / ".github/pull_request_template.md"]
    md_files += sorted((ROOT / ".claude").rglob("*.md"))
    md_files += sorted((ROOT / "docs").rglob("*.md"))
    for f in md_files:
        if not f.exists():
            err(f"{f.relative_to(ROOT)}: missing")
    check_links([f for f in md_files if f.exists()])

    # workspace members <-> AGENTS.md crate table
    members = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["members"]
    for m in members:
        name = tomllib.loads((ROOT / m / "Cargo.toml").read_text())["package"]["name"]
        if f"| `{name}` |" not in agents_md:
            err(f"AGENTS.md §2: workspace member `{name}` ({m}) is not in the crate table")
    # "May depend on" column: the allow-list for normal dependencies
    rows = {m.group(1): m.group(2) for m in
            re.finditer(r"^\| `([a-z0-9-]+)` \|[^|]*\|([^|]*)\|", agents_md, re.M)}
    for m in members:
        manifest = tomllib.loads((ROOT / m / "Cargo.toml").read_text())
        name = manifest["package"]["name"]
        allowed_text = rows.get(name, "")
        if "anything" in allowed_text:
            continue
        allowed = set(re.findall(r"`([a-z0-9_-]+)`", allowed_text.split("never")[0]))
        for dep in manifest.get("dependencies", {}):
            if dep not in allowed:
                err(f"{m}/Cargo.toml: dependency `{dep}` is not in the AGENTS.md §2 allow-list for `{name}`")
    listed = set(re.findall(r"^\| `([a-z0-9-]+)` \|", agents_md, re.M))
    names = {tomllib.loads((ROOT / m / "Cargo.toml").read_text())["package"]["name"] for m in members}
    for n in listed - names:
        err(f"AGENTS.md §2: `{n}` is listed but is not a workspace member")

    # skills
    for d in sorted((ROOT / ".claude/skills").iterdir()):
        skill = d / "SKILL.md"
        if not skill.exists():
            err(f".claude/skills/{d.name}: no SKILL.md")
            continue
        fm = frontmatter(skill)
        if fm.get("name") != d.name:
            err(f"{skill.relative_to(ROOT)}: name '{fm.get('name')}' != directory '{d.name}'")
        if len(fm.get("description", "")) < 80:
            err(f"{skill.relative_to(ROOT)}: description too short to trigger reliably")
        if f"(.claude/skills/{d.name}/SKILL.md)" not in agents_md:
            err(f"AGENTS.md §8: skill {d.name} not listed")

    # reviewer agents
    for a in sorted((ROOT / ".claude/agents").glob("*.md")):
        fm = frontmatter(a)
        if fm.get("name") != a.stem:
            err(f"{a.relative_to(ROOT)}: name '{fm.get('name')}' != file '{a.stem}'")
        for key in ("description", "tools"):
            if not fm.get(key):
                err(f"{a.relative_to(ROOT)}: frontmatter lacks {key}")
        if f"(.claude/agents/{a.name})" not in agents_md:
            err(f"AGENTS.md §8: reviewer {a.stem} not listed")

    # ADRs
    for adr in sorted((ROOT / "docs/adr").glob("[0-9][0-9][0-9][0-9]-*.md")):
        if f"(docs/adr/{adr.name})" not in agents_md:
            err(f"AGENTS.md §6: {adr.name} not in the decision index")

    # guard hook
    guard = ROOT / "scripts/harness/guard.py"
    for case in json.loads((ROOT / "scripts/harness/guard_cases.json").read_text()):
        want = case.pop("refuse")
        rc = subprocess.run([sys.executable, str(guard)], input=json.dumps(case),
                            text=True, capture_output=True).returncode
        if (rc == 2) != want:
            err(f"guard: {'allowed' if rc != 2 else 'refused'} {json.dumps(case['tool_input'])}")
    if not (ROOT / "scripts/harness/install-git-hooks.sh").exists():
        err("scripts/harness/install-git-hooks.sh: missing")
    settings = json.loads((ROOT / ".claude/settings.json").read_text())
    if "scripts/harness/guard.py" not in json.dumps(settings):
        err(".claude/settings.json: guard hook not installed")

    if errors:
        print("harness check failed:", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1
    print(f"harness consistent ({len(md_files)} documents, {len(members)} crates)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
