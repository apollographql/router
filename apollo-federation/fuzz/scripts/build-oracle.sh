#!/usr/bin/env bash
# Build a native query-inclusion oracle from a checkout of the apollo-graphql-lean project.
#
#   scripts/build-oracle.sh /path/to/apollo-graphql-lean [OUTPUT]
#
# The adapter in lean/QueryInclusionOracle.lean is compiled and linked against the built model.
# The Lean revision it was built from is recorded beside the binary; the runner refuses a
# mismatch against LEAN_MODEL_COMMIT unless the override is set.
#
# `GraphQL.Theories.QueryInclusion` lives in the graphql-lean package this project depends on, so
# the link line is assembled from static libraries the same way build-plan-oracle.sh does it --
# `lake build <module>:c.o` does not compile that module's dependencies. The leanfmt dependency is
# excluded because it carries a `Main` of its own, which would collide with the adapter's.
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

mkdir -p "$output_dir"

# leanc shells out to the C++ toolchain; on macOS the SDK headers are not on the default path.
if [[ -z "${CPLUS_INCLUDE_PATH:-}" ]] && command -v xcrun >/dev/null 2>&1; then
  CPLUS_INCLUDE_PATH="$(xcrun --sdk macosx --show-sdk-path)/usr/include/c++/v1"
  export CPLUS_INCLUDE_PATH
fi

(
  cd "$lean_project"
  lean_prefix="$(lake env lean --print-prefix)"
  lake build GraphQL
  lake build GraphQL:static
  lake env lean --stdin -c "$c_file" < "$source_file"
  "$lean_prefix/bin/leanc" -I "$lean_prefix/include" -c "$c_file" -o "$object_file"

  libraries=()
  while IFS= read -r library; do
    libraries+=("$library")
  done < <(find .lake/build/lib .lake/packages/*/.lake/build/lib -name '*.a' ! -path '*/leanfmt/*' | sort)
  if [[ ${#libraries[@]} -eq 0 ]]; then
    echo "no Lean static libraries found under $lean_project" >&2
    exit 1
  fi
  # A static link resolves left to right; repeating the list closes cycles between the packages.
  "$lean_prefix/bin/leanc" -o "$output" "$object_file" "${libraries[@]}" "${libraries[@]}"
)

git -C "$lean_project" rev-parse HEAD > "$output.model-commit"
echo "built $output from Lean commit $(cat "$output.model-commit")"
