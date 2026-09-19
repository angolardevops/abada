#!/usr/bin/env bash
# Installs git-level guards that hold for any tool (Codex, Copilot, Cursor,
# Gemini, a human): refuse pushing to main and pushing tags. Idempotent.
set -euo pipefail
hooks="$(git rev-parse --git-path hooks)"
mkdir -p "$hooks"
cat > "$hooks/pre-push" <<'HOOK'
#!/usr/bin/env bash
# abada harness (scripts/harness/install-git-hooks.sh): see AGENTS.md §4.9.
while read -r _local_ref _local_sha remote_ref _remote_sha; do
  case "$remote_ref" in
    refs/heads/main) echo "abada harness: refused — never push to main; open a PR." >&2; exit 1 ;;
    refs/tags/*)     echo "abada harness: refused — tags are published by maintainers." >&2; exit 1 ;;
  esac
done
HOOK
chmod +x "$hooks/pre-push"
echo "installed $hooks/pre-push"
