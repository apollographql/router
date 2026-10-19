#!/usr/bin/env bash
#
# Smoke-test a built distroless Router image (see Dockerfile.router.distroless).
#
# The distroless image has no shell and no package manager, so the usual ways of
# poking at a container (`docker exec ... sh`, `--entrypoint=bash`) don't work,
# and a missing shared library shows up as a hard dynamic-loader failure at
# startup rather than as a warning. This script ensures that we don't ship a broken
# image because of a new shared library dependency creeping in we weren't aware of.
#
# Usage:
#   verify_distroless_image.sh <image-ref> [expected-version]
#
# Expects the image to already exist locally (e.g. after `docker buildx build
# --load`) or to be pullable. `expected-version` is optional; when given, the
# version the binary reports must contain it.
#
# Everything here goes through the Docker API only because in CI this runs against
# CircleCI's `setup_remote_docker` daemon, which has its own filesystem and its
# own network. Fixtures reach the container over `docker cp`, and readiness is
# read back out of the container's logs.

set -euo pipefail

IMAGE="${1:-}"
EXPECTED_VERSION="${2:-}"

if [ -z "${IMAGE}" ]; then
    echo "Usage: $0 <image-ref> [expected-version]" >&2
    exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
SUPERGRAPH="${REPO_ROOT}/examples/graphql/supergraph.graphql"

if [ ! -f "${SUPERGRAPH}" ]; then
    echo "Error: supergraph fixture not found at ${SUPERGRAPH}" >&2
    exit 1
fi

STARTUP_TIMEOUT_SECONDS="${STARTUP_TIMEOUT_SECONDS:-120}"
READY_LOG_MESSAGE="GraphQL endpoint exposed"
# The uid the router image runs as
EXPECTED_UID=1000

WORK_DIR="$(mktemp -d)"
CONTAINER=""
PROBE=""
PRINT_LOGS=false

cleanup () {
    if [ -n "${CONTAINER}" ]; then
        if [ "${PRINT_LOGS}" = "true" ]; then
            echo "--- router container logs ---"
            docker logs "${CONTAINER}" 2>&1 || true
            echo "--- end router container logs ---"
        fi
        docker rm -f "${CONTAINER}" > /dev/null 2>&1 || true
    fi
    if [ -n "${PROBE}" ]; then
        docker rm -f "${PROBE}" > /dev/null 2>&1 || true
    fi
    rm -rf "${WORK_DIR}"
}
trap cleanup EXIT

fail () {
    echo "FAIL: $1" >&2
    exit 1
}

echo "Verifying distroless image: ${IMAGE}"

###
# 1. The binary runs at all.
#
# This is the check that would have caught the zlib blocker: an unsatisfied
# NEEDED entry makes the dynamic loader abort before main(), so `--version`
# failing is how a shared library missing from the base image announces itself.
###
echo "==> Checking the Router binary executes"
if ! VERSION_OUTPUT="$(docker run --rm "${IMAGE}" --version 2>&1)"; then
    echo "${VERSION_OUTPUT}" >&2
    fail "the Router binary did not run. If the output above mentions a missing .so, the distroless base is short a library the binary needs"
fi
echo "    ${VERSION_OUTPUT}"

if [ -n "${EXPECTED_VERSION}" ]; then
    case "${VERSION_OUTPUT}" in
        *"${EXPECTED_VERSION}"*) ;;
        *) fail "expected the reported version to contain '${EXPECTED_VERSION}', got: ${VERSION_OUTPUT}" ;;
    esac
fi

# A created-but-never-started container is a read-only view of the image's
# filesystem that `docker cp` can pull files out of. It needs a command to be
# created at all, hence `--version`; nothing ever executes it.
PROBE="$(docker create "${IMAGE}" --version)"

###
# 2. The image really is distroless.
#
# If a shell shows up in here, either the base image changed or somebody
# switched to a `:debug` distroless tag, and the smaller-attack-surface claim
# we make in the docs stops being true.
###
echo "==> Checking no shell is present"
for shell in /bin/sh /bin/bash /bin/dash /bin/ash /bin/busybox /busybox/sh /usr/bin/sh /usr/bin/bash; do
    if docker cp "${PROBE}:${shell}" - > /dev/null 2>&1; then
        fail "${shell} exists in the image; this is supposed to be a distroless build"
    fi
done

###
# 3. It runs as the same non-root user as the Debian image.
#
# Not just "non-root". The uid is part of the flavors' compatibility promise,
# because it decides the ownership of anything written to a mounted volume and
# whether a `runAsUser`/`fsGroup` pinned to 1000 still lines up. The distroless
# base defaults to its own `nonroot` uid (65532), so this also catches the USER
# line being dropped and the base's default silently taking over.
###
echo "==> Checking the image runs as uid ${EXPECTED_UID}"
IMAGE_USER="$(docker inspect --format '{{.Config.User}}' "${IMAGE}")"
case "${IMAGE_USER}" in
    "${EXPECTED_UID}"|"${EXPECTED_UID}:${EXPECTED_UID}") ;;
    *) fail "image user is '${IMAGE_USER}'; expected uid ${EXPECTED_UID}, to match the Debian image" ;;
esac
echo "    user: ${IMAGE_USER}"

# Docker resolves HOME from /etc/passwd, so a uid with no entry there gets a
# different environment than the Debian image gives the Router.
echo "==> Checking uid ${EXPECTED_UID} has an /etc/passwd entry"
PASSWD_CONTENTS="$(docker cp "${PROBE}:/etc/passwd" - | tar -xO)"
if ! PASSWD_ENTRY="$(printf '%s\n' "${PASSWD_CONTENTS}" | grep ":x:${EXPECTED_UID}:")"; then
    fail "no /etc/passwd entry for uid ${EXPECTED_UID}; HOME would not resolve the way it does on the Debian image"
fi
echo "    ${PASSWD_ENTRY}"

###
# 4. It boots and serves.
#
# The fixtures go in with `docker cp` into a created-but-not-started container
# rather than with a bind mount, because bind mounts read from the daemon's
# filesystem and the daemon here may be remote. `docker cp` also fails loudly
# if /dist/config and /dist/schema aren't there, which is what the
# containerization docs tell users to mount over.
###
echo "==> Booting the Router and waiting for it to serve GraphQL"
cp "${SUPERGRAPH}" "${WORK_DIR}/supergraph.graphql"
cat > "${WORK_DIR}/router.yaml" <<'EOF'
supergraph:
  listen: 0.0.0.0:4000
EOF

CONTAINER="$(docker create "${IMAGE}" --supergraph /dist/schema/supergraph.graphql)"
docker cp "${WORK_DIR}/router.yaml" "${CONTAINER}:/dist/config/router.yaml"
docker cp "${WORK_DIR}/supergraph.graphql" "${CONTAINER}:/dist/schema/supergraph.graphql"
docker start "${CONTAINER}" > /dev/null

DEADLINE=$((SECONDS + STARTUP_TIMEOUT_SECONDS))
READY=false
while [ "${SECONDS}" -lt "${DEADLINE}" ]; do
    if docker logs "${CONTAINER}" 2>&1 | grep -q "${READY_LOG_MESSAGE}"; then
        READY=true
        break
    fi
    if [ "$(docker inspect --format '{{.State.Running}}' "${CONTAINER}")" != "true" ]; then
        PRINT_LOGS=true
        fail "the container exited before serving GraphQL"
    fi
    sleep 1
done

if [ "${READY}" != "true" ]; then
    PRINT_LOGS=true
    fail "the Router did not report '${READY_LOG_MESSAGE}' within ${STARTUP_TIMEOUT_SECONDS}s"
fi
echo "    ${READY_LOG_MESSAGE}"

IMAGE_SIZE="$(docker image inspect --format '{{.Size}}' "${IMAGE}")"
echo "==> OK: ${IMAGE} ($((IMAGE_SIZE / 1024 / 1024)) MiB)"
