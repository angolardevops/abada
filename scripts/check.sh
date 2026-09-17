#!/usr/bin/env bash
# Everything CI runs, in the same order. Prints what was NOT validated and
# exits non-zero when a step is skipped, so "green" always means "all of it".
set -euo pipefail
cd "$(dirname "$0")/.."

skipped=()
run() { echo "==> $*"; "$@"; }

run cargo fmt --all --check
run cargo clippy --workspace --all-targets -- -D warnings
run cargo test --workspace
run python3 scripts/check-harness.py

if [ "${1:-}" = "--no-oracle" ]; then
  skipped+=("scripts/regen-vectors.sh --check (vectors not re-asked to grpc-gateway)")
elif ! command -v go > /dev/null; then
  skipped+=("scripts/regen-vectors.sh --check (go not installed)")
else
  mkdir -p "$HOME/.cache/abada-ref"
  if command -v flock > /dev/null; then
    run flock "$HOME/.cache/abada-ref/.lock" scripts/regen-vectors.sh --check
  else
    run scripts/regen-vectors.sh --check
  fi
fi

if [ ${#skipped[@]} -gt 0 ]; then
  echo
  echo "NOT VALIDATED:" >&2
  printf '  - %s\n' "${skipped[@]}" >&2
  exit 1
fi
echo
echo "all checks ran and passed"
