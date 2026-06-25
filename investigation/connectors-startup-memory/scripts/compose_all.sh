#!/usr/bin/env bash
# Generate + compose the full N-sweep for both families (connectors, pure).
# Validation gate: every supergraph must pass `rover supergraph compose`.
# Produces artifacts/<family>_N<n>/supergraph.{yaml,graphql} and a manifest TSV.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ART="$ROOT/artifacts"
GEN="$ROOT/scripts/gen_schema.py"
CLI_DIR="$(cd "$ROOT/../.." && pwd)"   # repo root for cargo

N_SWEEP=(${N_SWEEP:-1 2 4 8 16 32 64})
K=${K:-8}
E=${E:-4}

export APOLLO_ELV2_LICENSE=accept
manifest="$ART/manifest.tsv"
echo -e "family\tN\tK\tE\tconnects\tsource_graphs\tsynthetic_subgraphs\tsupergraph_path\tcompose_status" > "$manifest"

for family in connectors pure; do
  for n in "${N_SWEEP[@]}"; do
    dir="$ART/${family}_N${n}"
    python3 "$GEN" --family "$family" --n "$n" --k "$K" --e "$E" --out-dir "$dir" >/dev/null
    if APOLLO_ELV2_LICENSE=accept rover supergraph compose --config "$dir/supergraph.yaml" \
         > "$dir/supergraph.graphql" 2> "$dir/compose.log"; then
      status=ok
    else
      status=FAIL
    fi
    connects=$(grep -c 'name: "connect"' "$dir/supergraph.graphql" 2>/dev/null || true)
    sgraphs=$(grep -c '@join__graph(name: "' "$dir/supergraph.graphql" 2>/dev/null || true)
    # synthetic subgraph count = number of @connect (1 synthetic subgraph per connect),
    # confirmed via federation-cli `expand`. For pure family this is just source_graphs.
    if [ "$family" = "connectors" ]; then synth="$connects"; else synth="$sgraphs"; fi
    echo -e "${family}\t${n}\t${K}\t${E}\t${connects}\t${sgraphs}\t${synth}\t${dir}/supergraph.graphql\t${status}" >> "$manifest"
    echo "[compose] ${family} N=${n} connects=${connects} source_graphs=${sgraphs} -> ${status}"
  done
done

echo "=== manifest ==="
column -t -s $'\t' "$manifest"
