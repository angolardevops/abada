#!/usr/bin/env bash
# Everything CI runs. Every step runs even when an earlier one fails, and the
# summary lists what failed and what was NOT validated, so "green" always means
# "all of it ran and passed".
set -uo pipefail
cd "$(dirname "$0")/.."

failed=()
skipped=()
run() {
  echo "==> $*"
  "$@" || failed+=("$*")
}

REF="${ABADA_REF_DIR:-$HOME/.cache/abada-ref}"

run cargo fmt --all --check
run cargo clippy --workspace --all-targets -- -D warnings
run cargo test --workspace
if rustup run 1.85 cargo --version > /dev/null 2>&1; then
  run env CARGO_TARGET_DIR=target/msrv cargo +1.85 check --workspace --all-targets
else
  skipped+=("MSRV build (rustup toolchain install 1.85)")
fi
run python3 scripts/check-harness.py

if [ "${1:-}" = "--no-oracle" ]; then
  skipped+=("scripts/regen-vectors.sh --check (vectors not re-asked to grpc-gateway)")
elif ! command -v go > /dev/null; then
  skipped+=("scripts/regen-vectors.sh --check (go not installed)")
elif command -v flock > /dev/null; then
  mkdir -p "$REF"
  run flock "$REF/.lock" scripts/regen-vectors.sh --check
else
  run scripts/regen-vectors.sh --check
fi

echo
status=0
if [ ${#failed[@]} -gt 0 ]; then
  echo "FAILED:" >&2
  printf '  - %s\n' "${failed[@]}" >&2
  status=1
fi
if [ ${#skipped[@]} -gt 0 ]; then
  echo "NOT VALIDATED:" >&2
  printf '  - %s\n' "${skipped[@]}" >&2
  status=1
fi
[ $status -eq 0 ] && echo "all checks ran and passed"
exit $status
