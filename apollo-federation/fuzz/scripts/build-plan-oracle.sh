#!/usr/bin/env bash
# Build a native query-plan-checker oracle from a checkout of the apollo-graphql-lean project.
#
#   scripts/build-plan-oracle.sh /path/to/apollo-graphql-lean [OUTPUT]
#
# The adapter in lean/QueryPlanCheckerOracle.lean is compiled and linked against the built model.
# The Lean revision it was built from is recorded beside the binary; the runner refuses a mismatch
# against PLAN_MODEL_COMMIT unless the override is set.
#
# Unlike the inclusion oracle this project declares no lean_exe, so there is no ready-made linker
# response file. The link line is assembled from every compiled object of the Apollo libraries and
# of the packages they depend on, minus anything carrying an entry point of its own -- the leanfmt
# dependency has a `Main`, which would collide with the adapter's. Adding a lean_exe upstream would
# make this shorter.
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 LEAN_PROJECT [OUTPUT]" >&2
  exit 2
fi

lean_project="$(cd "$1" && pwd)"
package_root="$(cd "$(dirname "$0")/.." && pwd)"
source_file="$package_root/lean/QueryPlanCheckerOracle.lean"
output="${2:-$package_root/target/query-plan-checker-lean-oracle}"
[[ "$output" = /* ]] || output="$(pwd)/$output"
output_dir="$(dirname "$output")"
c_file="$output_dir/QueryPlanCheckerOracle.c"
object_file="$output_dir/QueryPlanCheckerOracle.o"

mkdir -p "$output_dir"

# leanc shells out to the C++ toolchain; on macOS the SDK headers are not on the default path.
if [[ -z "${CPLUS_INCLUDE_PATH:-}" ]] && command -v xcrun >/dev/null 2>&1; then
  CPLUS_INCLUDE_PATH="$(xcrun --sdk macosx --show-sdk-path)/usr/include/c++/v1"
  export CPLUS_INCLUDE_PATH
fi

(
  cd "$lean_project"
  lean_prefix="$(lake env lean --print-prefix)"
  lake build Apollo.Definitions Apollo.Implementations
  # Static libraries rather than a hand-assembled object list: `lake build <module>:c.o` does not
  # compile its dependencies, and chasing the transitive closure by hand is how this breaks.
  lake build Apollo.Definitions:static Apollo.Implementations:static GraphQL:static
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
  # Apollo depends on graphql-lean, and a static link resolves left to right, so the Apollo
  # libraries have to come first. `find` sorts them that way here; make it explicit anyway.
  "$lean_prefix/bin/leanc" -o "$output" "$object_file" "${libraries[@]}" "${libraries[@]}"
)

git -C "$lean_project" rev-parse HEAD > "$output.model-commit"
echo "built $output from Lean commit $(cat "$output.model-commit")"
