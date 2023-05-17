#! /usr/bin/env bash

###
# Run an apollo-harness test under heaptrack.
#
# Since heaptrack is linux specific, the best way to do this is by running
# the tests in a container.
###

# shellcheck disable=SC1091
# shellcheck source=./incl.sh
source "$(dirname "${0}")/incl.sh"

results="${1}"
program="${2}"
target="${3}"
schema="${4}"
query="${5}"

CONMAN=$(which docker || which podman) || advise "${install_conman_advice:?}"

# Run the test with 1 or 2 arguments
if [[ "${query}" != "" ]]; then
    ${CONMAN} run \
        --rm \
        --mount "type=bind,source=${PWD}/scripts,target=/scripts" \
        --mount "type=bind,source=${PWD}/results,target=/results" \
        --mount "type=bind,source=${PWD}/testdata,target=/testdata" \
        --mount "type=bind,source=${PWD}/../target/${target}/release,target=/programs" \
        apollo_harness:latest /scripts/runit.sh "${results}" \
        "${program}" \
        "testdata/${schema}" \
        "testdata/${query}" > /dev/null 2>&1 || terminate "${CONMAN} failed to execute our test under heaptrack"
else
    ${CONMAN} run \
        --rm \
        --mount "type=bind,source=${PWD}/scripts,target=/scripts" \
        --mount "type=bind,source=${PWD}/results,target=/results" \
        --mount "type=bind,source=${PWD}/testdata,target=/testdata" \
        --mount "type=bind,source=${PWD}/../target/${target}/release,target=/programs" \
        apollo_harness:latest /scripts/runit.sh "${results}" \
        "${program}" \
        "testdata/${schema}" > /dev/null 2>&1 || terminate "${CONMAN} failed to execute our test under heaptrack"
fi

# Display the heaptrack analyze results
printf "\nResults: %s.out\n" "${results}"
cat "results/${results}.out"
echo

