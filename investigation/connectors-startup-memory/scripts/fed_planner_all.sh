#!/usr/bin/env bash
# Federation-only startup heap profiling (no full router): expand_connectors + QueryPlanner::new.
# Isolates federation-side allocations from total router RSS.
# Output: artifacts/planner_measurements.tsv
# Usage: fed_planner_all.sh [family_Nx ...]   (default: a representative subset)
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$ROOT/../.." && pwd)"
ART="$ROOT/artifacts"
OUT="$ART/planner_measurements.tsv"

RUNS=("$@")
if [ "${#RUNS[@]}" -eq 0 ]; then
  RUNS=(connectors_N1 connectors_N8 connectors_N16 connectors_N32 pure_N8 pure_N16 pure_N32 pure_N64)
fi

[ -f "$OUT" ] || echo -e "run\tmax_bytes\ttotal_bytes\ttotal_blocks\tcurr_bytes" > "$OUT"

for r in "${RUNS[@]}"; do
  sg="$ART/$r/supergraph.graphql"
  [ -f "$sg" ] || { echo "[planner] missing $sg"; continue; }
  echo "[planner] $r ..."
  line=$(cd "$REPO" && CONNECTORS_SUPERGRAPH="$sg" \
    cargo test -q -p apollo-federation --test connectors_startup_profiling \
      -- --ignored --nocapture --test-threads=1 2>/dev/null \
    | grep '^CONNECTORS_DHAT')
  mb=$(echo "$line" | sed -n 's/.*max_bytes=\([0-9]*\).*/\1/p')
  tb=$(echo "$line" | sed -n 's/.*total_bytes=\([0-9]*\).*/\1/p')
  tk=$(echo "$line" | sed -n 's/.*total_blocks=\([0-9]*\).*/\1/p')
  cb=$(echo "$line" | sed -n 's/.*curr_bytes=\([0-9]*\).*/\1/p')
  echo -e "${r}\t${mb:-NA}\t${tb:-NA}\t${tk:-NA}\t${cb:-NA}" >> "$OUT"
  echo "    -> max_bytes=${mb:-NA} total_bytes=${tb:-NA} total_blocks=${tk:-NA}"
done

echo "=== planner_measurements.tsv ==="
column -t -s $'\t' "$OUT"
