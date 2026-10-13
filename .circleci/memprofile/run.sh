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

# Optionally link through the rust-lld bundled with the toolchain, using only stable flags.
echo "MEMPROFILE_RUST_LLD=${MEMPROFILE_RUST_LLD:-unset}" >> "$out/env.txt"
case "$(echo "${MEMPROFILE_RUST_LLD:-false}" | tr '[:upper:]' '[:lower:]')" in true | 1 | yes) use_lld=1 ;; *) use_lld=0 ;; esac
if [ "$use_lld" = 1 ]; then
  gccld="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | awk '/^host:/{print $2}')/bin/gcc-ld"
  export RUSTFLAGS="-Clink-arg=-fuse-ld=lld -Clink-arg=-B$gccld"
fi
echo "RUSTFLAGS=${RUSTFLAGS:-}" >> "$out/env.txt"

# Build-layout variants (cumulative):
#   cuts       — no test harness for the two examples without tests; `--tests` skips [[example]] targets
#   debug0     — cuts + no debuginfo for workspace crates (dependencies keep their cached line tables)
#   noexamples — cuts + example crates left out of this build
extra=()
variant=${MEMPROFILE_VARIANT:-base}
if [ "$variant" != base ]; then
  for m in examples/context/rust/Cargo.toml:context-data examples/supergraph-sdl/rust/Cargo.toml:supergraph_sdl; do
    printf '\n[[bin]]\nname = "%s"\npath = "src/main.rs"\ntest = false\n' "${m##*:}" >> "${m%%:*}"
  done
  extra+=(--tests)
fi
if [ "$variant" = debug0 ]; then
  # Per workspace member, so dependency fingerprints (and the restored cache) stay valid.
  for p in $(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; [print(p["name"]) for p in json.load(sys.stdin)["packages"]]'); do
    extra+=(--config "profile.dev.package.$p.debug=0")
  done
fi
if [ "$variant" = noexamples ]; then
  for p in $(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; [print(p["name"]) for p in json.load(sys.stdin)["packages"] if "/examples/" in p["manifest_path"]]'); do
    extra+=(--exclude "$p")
  done
fi
echo "variant=$variant extra=${extra[*]}" >> "$out/env.txt"

python3 .circleci/memprofile/memsample.py "$out/samples.tsv" 0.5 &
sampler=$!
start=$(date +%s.%N)
cargo test --no-run --workspace --locked --features ci,snapshot "${extra[@]}" --timings > "$out/build.log" 2>&1
rc=$?
end=$(date +%s.%N)
kill "$sampler"
cp target/cargo-timings/cargo-timing.html "$out/" 2>/dev/null
echo "compiled=$(grep -c '^ *Compiling' "$out/build.log") wall_precise=$(python3 -c "print(round($end-$start,1))")" >> "$out/env.txt"

{
  echo "rc=$rc wall=$(python3 -c "print(round($end-$start))")s"
  echo "cgroup peak bytes: $(cat /sys/fs/cgroup/memory.peak 2>/dev/null || cat /sys/fs/cgroup/memory/memory.max_usage_in_bytes)"
  echo "cgroup events: $(tr '\n' ' ' < /sys/fs/cgroup/memory.events 2>/dev/null)"
} >> "$out/env.txt"
cat "$out/env.txt"
tail -5 "$out/build.log"
