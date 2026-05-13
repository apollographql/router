#!/usr/bin/env bash
#
# check_retry_list_drift.sh
#
# Detects "drift" in the retry-list filter inside `.config/nextest.toml`:
# entries that reference test names which no longer exist (renamed, deleted,
# or feature-gated away). These dead entries accumulate silently because
# nextest's filter language doesn't error on names that match nothing.
#
# Why: Phase 9 of the de-flake project (#9338) found 14 of 30 retry-list
# entries were dead. The list looked imposing but every other entry was a
# paper tiger. Periodically auditing prevents the next "243 -> 0" cleanup
# cycle from starting with a head start of dead entries.
#
# What it does:
#   1. Parses `.config/nextest.toml` for the `retries = 2` block and extracts
#      every `test(=<name>)` entry from its `filter = '''...'''` value.
#   2. Runs `cargo nextest list -p apollo-router --message-format json`
#      with the same feature flags used in CI.
#   3. Confirms each exact-match entry corresponds to at least one live
#      test.  Exits non-zero with a list of dead entries if any miss.
#
# What it does NOT do:
#   * Glob entries (`test(#...)`) are skipped. Globs match zero or more
#     tests by design; "matches zero" isn't necessarily drift, and verifying
#     would require re-implementing nextest's glob semantics.
#   * Does not validate `binary_id(...)` predicates - we only check that
#     the test name itself is reachable somewhere in the workspace.
#
# Usage:
#   scripts/check_retry_list_drift.sh
#
# Exits 0 on success, 1 if drift is detected, 2 on parser / tool errors.

set -euo pipefail

NEXTEST_CONFIG="${NEXTEST_CONFIG:-.config/nextest.toml}"
CARGO_PACKAGE="${CARGO_PACKAGE:-apollo-router}"
# Match CI feature flags.  These are the same features CI passes to
# `cargo xtask test` (see `.circleci/config.yml`: `--features ci,snapshot`)
# so that the set of "live tests" the audit sees matches the set CI sees.
CARGO_FEATURES="${CARGO_FEATURES:-ci,snapshot}"

if [[ ! -f "${NEXTEST_CONFIG}" ]]; then
  echo "error: ${NEXTEST_CONFIG} not found" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# 1. Extract the retry-list filter block.
#
# The retry list lives in the first `[[profile.default.overrides]]` block
# that contains `retries = 2`.  We extract the contents of its
# `filter = '''...'''` triple-quoted string.
# ---------------------------------------------------------------------------
retry_filter="$(awk '
  /^\[\[profile\.default\.overrides\]\]/ { in_block = 1; saw_retries = 0; next }
  in_block && /^\[/                      { in_block = 0; in_filter = 0 }
  in_block && /^retries[[:space:]]*=/    { saw_retries = 1 }
  in_block && saw_retries && /filter[[:space:]]*=[[:space:]]*'\'\'\''/ {
    in_filter = 1
    next
  }
  in_filter && /'\'\'\''/                { in_filter = 0; exit }
  in_filter                              { print }
' "${NEXTEST_CONFIG}")"

if [[ -z "${retry_filter}" ]]; then
  echo "info: no retry-list filter found in ${NEXTEST_CONFIG} - nothing to check" >&2
  exit 0
fi

# ---------------------------------------------------------------------------
# 2. Extract `test(=<name>)` entries.  Skip `test(#<glob>)` deliberately.
# ---------------------------------------------------------------------------
mapfile -t exact_entries < <(
  printf '%s\n' "${retry_filter}" \
    | grep -oE 'test\(=[^)]+\)' \
    | sed -E 's/^test\(=//; s/\)$//' \
    | sort -u
)

glob_count="$(printf '%s\n' "${retry_filter}" | grep -cE 'test\(#[^)]+\)' || true)"

echo "Retry-list audit: ${#exact_entries[@]} exact-match entries, ${glob_count} glob entries (skipped)."

if [[ "${#exact_entries[@]}" -eq 0 ]]; then
  echo "No exact-match entries to check."
  exit 0
fi

# ---------------------------------------------------------------------------
# 3. List all tests in the package via nextest's JSON output.
# ---------------------------------------------------------------------------
echo "Listing tests in package '${CARGO_PACKAGE}' with features '${CARGO_FEATURES}'..."

tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir}"' EXIT

list_output="${tmpdir}/nextest-list.json"

if ! cargo nextest list \
      -p "${CARGO_PACKAGE}" \
      --features "${CARGO_FEATURES}" \
      --message-format json \
      --tests \
      > "${list_output}" 2> "${tmpdir}/nextest-list.err"; then
  echo "error: 'cargo nextest list' failed" >&2
  cat "${tmpdir}/nextest-list.err" >&2
  exit 2
fi

# Flatten the JSON list into a stream of "test-name" lines.  The JSON shape
# is {"rust-suites": {"<suite>": {"testcases": {"<name>": {...}}}}}.
all_tests="${tmpdir}/all-tests.txt"
if command -v jq > /dev/null 2>&1; then
  jq -r '."rust-suites"[].testcases | keys[]' "${list_output}" \
    | sort -u > "${all_tests}"
else
  # Fallback: nextest also supports `--message-format human` listing.
  cargo nextest list \
        -p "${CARGO_PACKAGE}" \
        --features "${CARGO_FEATURES}" \
        --tests 2> /dev/null \
    | awk '/^    / { print $1 }' \
    | sort -u > "${all_tests}"
fi

total_tests="$(wc -l < "${all_tests}" | tr -d ' ')"
echo "Found ${total_tests} tests."

# ---------------------------------------------------------------------------
# 4. Verify each exact-match entry resolves to at least one live test.
# ---------------------------------------------------------------------------
dead_entries=()
for entry in "${exact_entries[@]}"; do
  if ! grep -Fxq "${entry}" "${all_tests}"; then
    dead_entries+=("${entry}")
  fi
done

if [[ "${#dead_entries[@]}" -eq 0 ]]; then
  echo "OK: all ${#exact_entries[@]} exact-match retry-list entries resolve to live tests."
  exit 0
fi

echo ""
echo "DRIFT DETECTED: ${#dead_entries[@]} retry-list entries reference tests that do not exist:" >&2
for entry in "${dead_entries[@]}"; do
  echo "  - test(=${entry})" >&2
done
echo "" >&2
echo "These tests have likely been renamed, removed, or feature-gated since the" >&2
echo "entry was added.  Remove them from the 'filter' value in:" >&2
echo "  ${NEXTEST_CONFIG}" >&2
echo "" >&2
echo "If a test is feature-gated, ensure CARGO_FEATURES in this script includes" >&2
echo "the gating feature, then re-run." >&2
exit 1
