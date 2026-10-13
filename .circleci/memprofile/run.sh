#!/bin/bash
# Builds exactly what the test job builds (no test execution) while sampling memory.
# Results land in /tmp/memprofile for store_artifacts.
set -uo pipefail
out=/tmp/memprofile
mkdir -p "$out"

{
  echo "nproc=$(nproc)"
  free -m
  echo "cgroup limit: $(cat /sys/fs/cgroup/memory.max 2>/dev/null || cat /sys/fs/cgroup/memory/memory.limit_in_bytes)"
  ld -v
  rustc -V
  echo "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-unset} CARGO_PROFILE_DEV_DEBUG=${CARGO_PROFILE_DEV_DEBUG:-unset}"
  echo "restored target: $(du -sh target 2>/dev/null | cut -f1)"
} > "$out/env.txt" 2>&1

python3 .circleci/memprofile/memsample.py "$out/samples.tsv" 0.5 &
sampler=$!
start=$(date +%s)
cargo nextest run --no-run --workspace --locked --features ci,snapshot > "$out/build.log" 2>&1
rc=$?
kill "$sampler"

{
  echo "rc=$rc wall=$(( $(date +%s) - start ))s"
  echo "cgroup peak bytes: $(cat /sys/fs/cgroup/memory.peak 2>/dev/null || cat /sys/fs/cgroup/memory/memory.max_usage_in_bytes)"
  echo "cgroup events: $(tr '\n' ' ' < /sys/fs/cgroup/memory.events 2>/dev/null)"
} >> "$out/env.txt"
cat "$out/env.txt"
tail -5 "$out/build.log"
