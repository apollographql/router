#!/usr/bin/env bash
# Build a native Lean oracle from a checkout of the graphql-lean project.
#
#   scripts/build-oracle.sh /path/to/graphql-lean [OUTPUT]
#
# The adapter in lean/QueryInclusionOracle.lean is compiled and linked against the built model.
# The Lean revision it was built from is recorded beside the binary; the runner refuses a
# mismatch against LEAN_MODEL_COMMIT unless the override is set.
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 LEAN_PROJECT [OUTPUT]" >&2
  exit 2
fi

lean_project="$(cd "$1" && pwd)"
package_root="$(cd "$(dirname "$0")/.." && pwd)"
source_file="$package_root/lean/QueryInclusionOracle.lean"
output="${2:-$package_root/target/query-inclusion-lean-oracle}"
[[ "$output" = /* ]] || output="$(pwd)/$output"
output_dir="$(dirname "$output")"
c_file="$output_dir/QueryInclusionOracle.c"
object_file="$output_dir/QueryInclusionOracle.o"
response_file="$output_dir/query-inclusion-oracle.rsp"

# Any lean_exe in the project yields a linker response file listing the model's native objects.
benchmark_response="$lean_project/.lake/build/bin/query-inclusion-bench.rsp"
query_inclusion_object="$lean_project/.lake/build/ir/GraphQL/Theories/QueryInclusion.c.o.export"

mkdir -p "$output_dir"

# leanc shells out to the C++ toolchain; on macOS the SDK headers are not on the default path.
if [[ -z "${CPLUS_INCLUDE_PATH:-}" ]] && command -v xcrun >/dev/null 2>&1; then
  CPLUS_INCLUDE_PATH="$(xcrun --sdk macosx --show-sdk-path)/usr/include/c++/v1"
  export CPLUS_INCLUDE_PATH
fi

(
  cd "$lean_project"
  lean_prefix="$(lake env lean --print-prefix)"
  lake build GraphQL query-inclusion-bench GraphQL.Theories.QueryInclusion:c.o
  lake env lean --stdin -c "$c_file" < "$source_file"
  "$lean_prefix/bin/leanc" -I "$lean_prefix/include" -c "$c_file" -o "$object_file"
)

for required in "$benchmark_response" "$query_inclusion_object"; do
  if [[ ! -f "$required" ]]; then
    echo "missing Lake build output: $required" >&2
    exit 1
  fi
done

# Reuse the benchmark's link line, minus its own entry point. The benchmark already depends on
# the QueryInclusion module, so its objects are present; appending them again is a duplicate
# symbol error rather than a fix.
sed '/Benchmarks\/QueryInclusion\.c\.o\.export/d' "$benchmark_response" > "$response_file"
if ! grep -q "$query_inclusion_object" "$response_file"; then
  printf '"%s"\n' "$query_inclusion_object" >> "$response_file"
fi

(
  cd "$lean_project"
  lean_prefix="$(lake env lean --print-prefix)"
  "$lean_prefix/bin/leanc" -o "$output" "$object_file" "@$response_file"
)

git -C "$lean_project" rev-parse HEAD > "$output.model-commit"
echo "built $output from Lean commit $(cat "$output.model-commit")"
