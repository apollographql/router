#!/bin/bash

if [ -z "$FRED_CI_NEXTEST" ]; then
  cargo test --release --lib --features "mocks i-keys" "$@"
else
  cargo nextest run --release --lib --features "mocks i-keys" "$@"
fi