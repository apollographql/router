set -euo pipefail

if [ -z "${FEDERATION_VERSION:-}" ]; then
  FEDERATION_VERSION="2.11.0"
fi

regenerate_graphql() {
  local supergraph_config=$1
  local test_name
  test_name=$(basename "$supergraph_config" .yaml)
  local dir_name
  dir_name=$(dirname "$supergraph_config")
  echo "Regenerating $dir_name/$test_name.graphql"
  rover supergraph compose --federation-version "=$FEDERATION_VERSION" --config "$supergraph_config" > "$dir_name/$test_name.graphql"
}

if [ -z "${1:-}" ]; then
  for supergraph_config in **/*.yaml; do
    regenerate_graphql "$supergraph_config"
  done
else
  regenerate_graphql "$1"
fi