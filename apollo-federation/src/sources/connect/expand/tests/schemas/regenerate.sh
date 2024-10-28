set -euo pipefail

for supergraph_config in */*.yaml; do
  test_name=$(basename "$supergraph_config" .yaml)
  dir_name=$(dirname "$supergraph_config")
  echo "Regenerating $dir_name/$test_name.graphql"
  rover supergraph compose --federation-version "=2.10.0-preview.0" --config "$supergraph_config" > "$dir_name/$test_name.graphql"
done