#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

if [[ ! -d "${JAVA_HOME:-}" && -x /usr/libexec/java_home ]]; then
  export JAVA_HOME="$(/usr/libexec/java_home)"
fi

mvn --quiet package
../node-header/node_modules/.bin/esbuild wrapper.js \
  --bundle \
  --format=esm \
  --platform=neutral \
  --target=es2022 \
  --outfile=target/plugin.bundle.js
../node-header/node_modules/.bin/jco componentize \
  target/plugin.bundle.js \
  --wit ../../../apollo-router/wit/router-plugin \
  --world-name router-plugin \
  --disable all \
  -o plugin.wasm
