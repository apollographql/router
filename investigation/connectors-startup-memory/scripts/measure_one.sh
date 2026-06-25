#!/usr/bin/env bash
# Measure one router startup: peak RSS (/usr/bin/time -l) + dhat-heap.json.
# Usage: measure_one.sh <supergraph.graphql> <run_dir>
# Writes <run_dir>/{router.log,time.txt,dhat-heap.json}. Echoes a TSV result line:
#   ready_sec  rss_max_bytes  dhat_total_bytes  dhat_total_blocks  dhat_peak_bytes  dhat_peak_blocks
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$ROOT/../.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
ROUTER="${ROUTER_BIN:-$TARGET_DIR/release-dhat/router}"
CONFIG="$ROOT/scripts/router.yaml"
HEALTH="http://127.0.0.1:8098/health"

# absolutize args (the run subshell cd's into RUN_DIR so dhat-heap.json lands there)
SUPERGRAPH="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
mkdir -p "$2"; RUN_DIR="$(cd "$2" && pwd)"
rm -f "$RUN_DIR/dhat-heap.json"
LOG="$RUN_DIR/router.log"; TIME_OUT="$RUN_DIR/time.txt"

export APOLLO_TELEMETRY_DISABLED=true

start=$(python3 -c 'import time;print(time.time())')
( cd "$RUN_DIR" && exec /usr/bin/time -l "$ROUTER" -s "$SUPERGRAPH" -c "$CONFIG" >"$LOG" 2>"$TIME_OUT" ) &
TIME_PID=$!

ready=0; ready_sec=""
READY_ITERS="${READY_ITERS:-240}"   # 0.5s each; default 120s
for _ in $(seq 1 "$READY_ITERS"); do
  if curl -fs "$HEALTH" >/dev/null 2>&1; then
    ready=1
    ready_sec=$(python3 -c "import time;print(f'{time.time()-$start:.2f}')")
    break
  fi
  kill -0 "$TIME_PID" 2>/dev/null || break
  sleep 0.5
done

RPID=$(pgrep -P "$TIME_PID" 2>/dev/null || true)
if [ -n "$RPID" ]; then kill -TERM "$RPID" 2>/dev/null || true; fi
# give graceful shutdown + dhat atexit a moment; force-kill the wrapper if it hangs
( sleep 30; kill -KILL "$TIME_PID" 2>/dev/null ) &
WATCH=$!
wait "$TIME_PID" 2>/dev/null
kill "$WATCH" 2>/dev/null || true

rss=$(awk '/maximum resident set size/{print $1}' "$TIME_OUT" 2>/dev/null | tail -1)
[ -z "$rss" ] && rss=NA
[ -z "$ready_sec" ] && ready_sec=NA

if [ -f "$RUN_DIR/dhat-heap.json" ]; then
  dhat=$(python3 "$ROOT/scripts/parse_dhat.py" "$RUN_DIR/dhat-heap.json" --tsv 2>/dev/null)
  [ -z "$dhat" ] && dhat="NA	NA	NA	NA"
else
  dhat="NA	NA	NA	NA"
fi
[ "$ready" = "1" ] || echo "[measure] WARN: $SUPERGRAPH never became healthy" >&2
printf "%s\t%s\t%s\n" "$ready_sec" "$rss" "$dhat"
