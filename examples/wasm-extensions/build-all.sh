#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd "$(dirname "$0")" && pwd)"
repository_dir="$(cd "$example_dir/../.." && pwd)"
rust_cargo="$(rustup which cargo 2>/dev/null || command -v cargo)"
rust_compiler="$(rustup which rustc 2>/dev/null || command -v rustc)"

cd "$example_dir"
npm ci
npm run build:node

(
  cd rust-header
  RUSTC="$rust_compiler" "$rust_cargo" build --release --target wasm32-wasip2
)

(
  cd python-header
  if [[ ! -x .venv/bin/python ]]; then
    python3 -m venv .venv
  fi
  .venv/bin/pip install --quiet --requirement requirements.txt
  rm -rf componentize_py_async_support wit_world
  rm -f componentize_py_runtime.pyi componentize_py_types.py poll_loop.py
  .venv/bin/componentize-py \
    -d ../../../apollo-router/wit/router-plugin \
    -w router-plugin \
    bindings .
  .venv/bin/componentize-py \
    -d ../../../apollo-router/wit/router-plugin \
    -w router-plugin \
    componentize plugin \
    -o plugin.wasm \
    --stub-wasi
)

(cd go-header && ./build.sh)
(cd java-header && ./build.sh)
(cd scala-header && ./build.sh)

cd "$repository_dir"
cargo build -p apollo-router --bin router
