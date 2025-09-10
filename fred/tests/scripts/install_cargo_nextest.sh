#!/bin/bash

if [ -z "$FRED_CI_NEXTEST" ]; then
  echo "Skip installing cargo-nextest"
else
  cargo install cargo-nextest
fi