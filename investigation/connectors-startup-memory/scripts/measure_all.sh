#!/usr/bin/env bash
# Sweep: measure router startup memory for every supergraph in artifacts/manifest.tsv.
# Output: artifacts/measurements.tsv
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ART="$ROOT/artifacts"
MAN="$ART/manifest.tsv"
OUT="$ART/measurements.tsv"

echo -e "family\tN\tconnects\tsynthetic_subgraphs\tready_sec\trss_max_bytes\tdhat_total_bytes\tdhat_total_blocks\tdhat_peak_bytes\tdhat_peak_blocks" > "$OUT"

# skip header; columns: family N K E connects source_graphs synthetic_subgraphs path status
tail -n +2 "$MAN" | while IFS=$'\t' read -r family n k e connects sgraphs synth path status; do
  [ "$status" = "ok" ] || { echo "[skip] $family N=$n status=$status"; continue; }
  run_dir="$ART/${family}_N${n}/run"
  echo "[measure] $family N=$n connects=$connects synth=$synth ..."
  line=$(bash "$ROOT/scripts/measure_one.sh" "$path" "$run_dir")
  echo -e "${family}\t${n}\t${connects}\t${synth}\t${line}" >> "$OUT"
  echo "    -> $line"
done

echo "=== measurements.tsv ==="
column -t -s $'\t' "$OUT"
