#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

command -v wit-bindgen >/dev/null || {
  echo "wit-bindgen 0.60.0 or newer is required" >&2
  exit 1
}
command -v wasm-tools >/dev/null || {
  echo "wasm-tools is required" >&2
  exit 1
}

rm -rf generated
wit-bindgen go \
  --world router-plugin \
  --out-dir generated \
  ../../../apollo-router/wit/router-plugin
mkdir -p generated/export_apollo_router_plugin_hooks
cp plugin.go generated/export_apollo_router_plugin_hooks/plugin.go

(
  cd generated
  go mod tidy
  GOARCH=wasm GOOS=wasip1 go build \
    -buildmode=c-shared \
    -ldflags=-checklinkname=0 \
    -o core.wasm
)

if [[ ! -f wasi_snapshot_preview1.reactor.wasm ]]; then
  curl --fail --location \
    https://github.com/bytecodealliance/wasmtime/releases/download/v47.0.2/wasi_snapshot_preview1.reactor.wasm \
    --output wasi_snapshot_preview1.reactor.wasm
fi
echo "928546f9b8f704e0e01e656a2c12f08f6e0da6f5b29da0179ee282a4138ef5c4  wasi_snapshot_preview1.reactor.wasm" \
  | shasum --algorithm 256 --check

wasm-tools component embed \
  --world router-plugin \
  ../../../apollo-router/wit/router-plugin \
  generated/core.wasm \
  --output generated/core-with-wit.wasm
wasm-tools component new \
  --adapt wasi_snapshot_preview1.reactor.wasm \
  generated/core-with-wit.wasm \
  --output plugin.wasm
