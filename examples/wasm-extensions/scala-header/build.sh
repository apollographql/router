#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

if [[ ! -d "${JAVA_HOME:-}" && -x /usr/libexec/java_home ]]; then
  export JAVA_HOME="$(/usr/libexec/java_home)"
fi

command -v scala-cli >/dev/null || {
  echo "scala-cli is required" >&2
  exit 1
}

mkdir -p target
scala-cli --power package \
  --js \
  --js-module-kind es \
  --force \
  Plugin.scala \
  --output target/plugin.js
../node_modules/.bin/esbuild wrapper.js \
  --bundle \
  --format=esm \
  --platform=neutral \
  --target=es2022 \
  --outfile=target/plugin.bundle.js
../node_modules/.bin/jco componentize \
  target/plugin.bundle.js \
  --wit ../../../apollo-router/wit/router-plugin \
  --world-name router-plugin \
  --disable all \
  -o plugin.wasm
