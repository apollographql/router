#!/bin/bash -e

all_features=`yq -oy '.features["i-all"]' Cargo.toml | tr -d '\n' | sed -e 's/- / /g' | cut -c 2-`
redis_stack_features=`yq -oy '.features["i-redis-stack"]' Cargo.toml | tr -d '\n' | sed -e 's/- / /g' | cut -c 2-`

for feature in $all_features; do
  echo "Checking $feature"
  cargo clippy --lib -p fred --no-default-features --features "glommio $feature" -- -Dwarnings
done

for feature in $redis_stack_features; do
  echo "Checking $feature"
  cargo clippy --lib -p fred --no-default-features --features "glommio $feature" -- -Dwarnings
done