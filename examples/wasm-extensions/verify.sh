#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd "$(dirname "$0")" && pwd)"
repository_dir="$(cd "$example_dir/../.." && pwd)"
temporary_dir="$(mktemp -d)"
subgraph_pid=""
router_pid=""

cleanup() {
  if [[ -n "$router_pid" ]]; then
    kill "$router_pid" 2>/dev/null || true
    wait "$router_pid" 2>/dev/null || true
  fi
  if [[ -n "$subgraph_pid" ]]; then
    kill "$subgraph_pid" 2>/dev/null || true
    wait "$subgraph_pid" 2>/dev/null || true
  fi
  rm -rf "$temporary_dir"
}
trap cleanup EXIT

components=(
  rust-header/target/wasm32-wasip2/release/router_wasm_rust_header.wasm
  node-header/plugin.wasm
  python-header/plugin.wasm
  go-header/plugin.wasm
  java-header/plugin.wasm
  scala-header/plugin.wasm
)

cd "$example_dir"
for component in "${components[@]}"; do
  wasm-tools validate "$component"
  wasm-tools component wit "$component" | grep -q "apollo:router-plugin/hooks"
done

python3 subgraph.py >"$temporary_dir/subgraph.log" 2>&1 &
subgraph_pid=$!

cd "$repository_dir"
target/debug/router \
  --config examples/wasm-extensions/router.yaml \
  --supergraph examples/wasm-extensions/supergraph.graphql \
  >"$temporary_dir/router.log" 2>&1 &
router_pid=$!

request='{"query":"{ me }"}'
expected='{"data":{"me":"active,active,active,active,active,active"}}'
response=""
for _ in {1..600}; do
  if response="$(curl --silent --fail --max-time 1 http://127.0.0.1:4100/ \
    -H 'content-type: application/json' \
    --data "$request" 2>/dev/null)"; then
    break
  fi
  sleep 0.25
done

if [[ "$response" != "$expected" ]]; then
  echo "unexpected Router response: $response" >&2
  echo "Router log:" >&2
  sed -n '1,200p' "$temporary_dir/router.log" >&2
  echo "Subgraph log:" >&2
  sed -n '1,200p' "$temporary_dir/subgraph.log" >&2
  exit 1
fi

echo "$response"
