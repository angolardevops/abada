#!/usr/bin/env bash
# The performance gate's protocol (.claude/skills/abada-performance §2-3).
#
#   scripts/parity/run.sh [--quick|--full] [outdir]
#
# --full  : 10 s warm-up, 30 s measured, 3 runs, 300 s soak   (what the skill asks)
# --quick : 3 s warm-up, 10 s measured, 3 runs, 60 s soak      (labelled as such in results)
#
# Every run starts fresh gateway processes (so RSS and cold caches are honest),
# alternates the two sides run by run (so drift on a shared host hits both),
# and records host load before and after. Nothing is pinned.
set -euo pipefail

MODE=quick
[ "${1:-}" = "--quick" ] && { MODE=quick; shift; }
[ "${1:-}" = "--full" ] && { MODE=full; shift; }
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REF="${ABADA_REF_DIR:-$HOME/.cache/abada-ref}"
PARITY="$REF/bin/parity"
ABADA="$ROOT/target/release/abada-parity-gateway"
SET="$ROOT/conformance/contracts/abada-conformance-request-v1.binpb"
MIX="$ROOT/benches/parity/mix.json"
OUT="${1:-$ROOT/benches/parity/results/$(date -u +%Y%m%dT%H%M%SZ)-$MODE}"
mkdir -p "$OUT/raw"

if [ "$MODE" = full ]; then WARM=10s; DUR=30s; SOAK=300; else WARM=3s; DUR=10s; SOAK=60; fi
RUNS=3
LEVELS="1 8 32 128"
# ABADA_PARITY_SMOKE=1 only checks the pipeline (1 s runs); its numbers mean nothing.
if [ -n "${ABADA_PARITY_SMOKE:-}" ]; then WARM=1s; DUR=2s; SOAK=10; RUNS=1; LEVELS="1 8"; MODE="$MODE-smoke"; fi
# Open-loop rates and the soak rate are derived from the measured closed-loop
# capacity below: a fixed rate above capacity measures a queue, not a gateway.
[ "$(getconf CLK_TCK)" = 100 ] || { echo "CLK_TCK is not 100: loadgen's CPU figure would be wrong" >&2; exit 1; }
[ -x "$PARITY" ] && [ -x "$ABADA" ] || { echo "build both sides first (scripts/parity/build-reference.sh; cargo build --release -p abada-parity-gateway)" >&2; exit 1; }

PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; }
trap cleanup EXIT

load() { cut -d' ' -f1 /proc/loadavg; }
GO_PORT=8080; RS_PORT=8081; BE=127.0.0.1:9000

start_backend() { "$PARITY" backend -listen "$BE" -set "$SET" 2>"$OUT/raw/backend.log" & BACKEND=$!; PIDS+=("$BACKEND"); sleep 0.3; }
start_side() { # side -> sets GW (pid) and URL
  case "$1" in
    go)     "$PARITY" gateway -listen 127.0.0.1:$GO_PORT -backend "$BE" 2>>"$OUT/raw/gateway-go.log" & GW=$!; URL=http://127.0.0.1:$GO_PORT;;
    abada)  "$ABADA" --listen 127.0.0.1:$RS_PORT --backend "$BE" --set "$SET" 2>>"$OUT/raw/gateway-abada.log" & GW=$!; URL=http://127.0.0.1:$RS_PORT;;
  esac
  PIDS+=("$GW")
  "$PARITY" ready -mix "$MIX" -url "$URL" > /dev/null
}
stop_side() { kill "$GW" 2>/dev/null || true; wait "$GW" 2>/dev/null || true; }

{
  echo "{"
  echo " \"mode\": \"$MODE\", \"warmup\": \"$WARM\", \"duration\": \"$DUR\", \"runs\": $RUNS, \"soak_seconds\": $SOAK,"
  echo " \"date_utc\": \"$(date -u +%FT%TZ)\", \"commit\": \"$(git -C "$ROOT" rev-parse --short HEAD)\","
  echo " \"cpu\": \"$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs)\", \"threads\": $(nproc),"
  echo " \"governor\": \"$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unknown)\","
  echo " \"energy_preference\": \"$(cat /sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference 2>/dev/null || echo unknown)\","
  echo " \"cpu_mhz_mean_before\": $(awk '/MHz/ {s+=$4; n++} END {printf "%.0f", s/n}' /proc/cpuinfo),"
  echo " \"go\": \"$(go version | cut -d' ' -f3)\", \"rustc\": \"$(rustc --version)\","
  echo " \"load_before\": $(load),"
  echo " \"other_builds\": \"$(pgrep -c -x -f 'cargo|rustc' || true)\""
  echo "}"
} > "$OUT/host.json"

start_backend
echo "== correctness: both gateways answer the mix the same way"
for s in go abada; do start_side $s; "$PARITY" check -mix "$MIX" -url "$URL" > "$OUT/raw/check-$s.json"; stop_side; done
python3 "$ROOT/scripts/parity/summarize.py" check "$OUT" || true

echo "== closed loop"
for L in $LEVELS; do for r in $(seq $RUNS); do for s in go abada; do
  start_side $s
  "$PARITY" loadgen -mix "$MIX" -url "$URL" -c "$L" -d "$DUR" -warmup "$WARM" -pid "$GW" > "$OUT/raw/closed-$s-c$L-r$r.json"
  stop_side; echo "closed c=$L run=$r $s load=$(load)"
done; done; done

CAP=$(python3 "$ROOT/scripts/parity/summarize.py" capacity "$OUT")
RATES="$((CAP / 4)) $((CAP / 2))"
SOAK_RATE=$((CAP / 2))
echo "closed-loop capacity (the slower side, highest level): $CAP req/s -> open-loop rates $RATES, soak $SOAK_RATE"
echo "== open loop (fixed rate, latency from the intended start)"
for R in $RATES; do for r in $(seq $RUNS); do for s in go abada; do
  start_side $s
  "$PARITY" loadgen -mix "$MIX" -url "$URL" -rate "$R" -c 512 -d "$DUR" -warmup "$WARM" -pid "$GW" > "$OUT/raw/open-$s-rate$R-r$r.json"
  stop_side; echo "open rate=$R run=$r $s load=$(load)"
done; done; done

echo "== soak: ${SOAK}s at $SOAK_RATE req/s, RSS every 5 s"
for s in go abada; do
  start_side $s
  "$PARITY" loadgen -mix "$MIX" -url "$URL" -rate "$SOAK_RATE" -c 512 -d "${SOAK}s" -warmup "$WARM" -pid "$GW" -rss-every 5s > "$OUT/raw/soak-$s.json"
  stop_side; echo "soak $s load=$(load)"
done

echo "== cold start: process spawn to the first correct answer (ms), 7 runs"
for s in go abada; do for r in 1 2 3 4 5 6 7; do
  t0=$(date +%s%N)
  case $s in
    go)    "$PARITY" gateway -listen 127.0.0.1:$GO_PORT -backend "$BE" 2>/dev/null & GW=$!; URL=http://127.0.0.1:$GO_PORT;;
    abada) "$ABADA" --listen 127.0.0.1:$RS_PORT --backend "$BE" --set "$SET" 2>/dev/null & GW=$!; URL=http://127.0.0.1:$RS_PORT;;
  esac
  t1=$("$PARITY" ready -mix "$MIX" -url "$URL")
  echo "$s $(( (t1 - t0) / 1000 ))" >> "$OUT/raw/coldstart.txt"   # microseconds
  kill "$GW"; wait "$GW" 2>/dev/null || true
done; done

python3 - "$OUT/host.json" "$(load)" "$(awk '/MHz/ {s+=$4; n++} END {printf "%.0f", s/n}' /proc/cpuinfo)" <<'PY'
import json,sys
p=sys.argv[1]; h=json.load(open(p)); h["load_after"]=float(sys.argv[2]); h["cpu_mhz_mean_after"]=float(sys.argv[3]); json.dump(h,open(p,"w"),indent=1)
PY
python3 "$ROOT/scripts/parity/summarize.py" report "$OUT" | tee "$OUT/report.md"
echo "results in $OUT"
