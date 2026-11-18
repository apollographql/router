#!/bin/bash
set -e

# Download and validate Router binary. This script handles local artifacts,
# release downloads, and artifact downloads, all with checksum validation.
# NOTE: This script is only intended to be executed from inside
# Dockerfile.router (or Dockerfile.router.distroless) which live beside this.
#
# There are three ways to source the Router tarball, tried in this order:
#
#   1. Local artifacts: if ${LOCAL_ARTIFACTS_DIR}/sha256sums.txt exists, the
#      tarball for TARGETPLATFORM is taken from that directory. CI uses this for
#      nightly and test builds by passing
#      `--build-context router-artifacts=<dir>` to `docker buildx build`, where
#      <dir> holds one tarball per platform. Because each platform's build picks
#      its own tarball, this is what makes multi-platform builds possible for
#      binaries that haven't been published as a release.
#   2. ARTIFACT_URL: a single tarball downloaded from a URL (e.g. a CircleCI
#      artifact), used regardless of TARGETPLATFORM. Kept for the DIY build
#      script in `diy/`.
#   3. Release: the published release tarball for ROUTER_RELEASE and
#      TARGETPLATFORM.

LOCAL_ARTIFACTS_DIR="${LOCAL_ARTIFACTS_DIR:-/router-artifacts}"

if [ -f "${LOCAL_ARTIFACTS_DIR}/sha256sums.txt" ]; then
    SOURCE="local"
elif [ -n "${ARTIFACT_URL}" ]; then
    SOURCE="artifact_url"
else
    SOURCE="release"
fi

# Validate required environment variables early
case "${SOURCE}" in
    "release")
        if [ -z "${ROUTER_RELEASE}" ]; then
            echo "Error: ROUTER_RELEASE environment variable is required for release builds"
            exit 1
        fi
        ;;
    "artifact_url")
        if [ -z "${CIRCLE_TOKEN}" ]; then
            echo "Error: CIRCLE_TOKEN environment variable is required when ARTIFACT_URL is set"
            exit 1
        fi
        # ARTIFACT_URL_SHA256SUM is optional but recommended
        ;;
esac

if [ "${SOURCE}" != "artifact_url" ]; then
    # Requires TARGETPLATFORM (a variable set by Docker buildx)
    if [ -z "${TARGETPLATFORM}" ]; then
        echo "Error: TARGETPLATFORM environment variable is required for ${SOURCE} builds"
        exit 1
    fi

    # Validate TARGETPLATFORM and map to architecture
    case "${TARGETPLATFORM}" in
        "linux/amd64")
            ARCH="x86_64-unknown-linux-gnu"
            ;;
        "linux/arm64")
            ARCH="aarch64-unknown-linux-gnu"
            ;;
        *)
            echo "Error: Unsupported TARGETPLATFORM '${TARGETPLATFORM}'. Only 'linux/amd64' and 'linux/arm64' are supported."
            exit 1
            ;;
    esac
fi

# Validates a tarball against its entry in a sha256sums.txt-style file.
#
# Usage: verify_sha256sum <tarball> <sums-file>
verify_sha256sum () {
    local tarball="$1"
    local sums_file="$2"
    local tarball_name expected actual

    tarball_name="$(basename "${tarball}")"
    # sha256sum writes "<hash>  <name>", or "<hash> *<name>" in binary mode
    expected=$(awk -v name="${tarball_name}" '$2 == name || $2 == "*" name { print $1; exit }' "${sums_file}")
    if [ -z "${expected}" ]; then
        echo "Error: Could not find checksum for ${tarball_name} in ${sums_file}"
        exit 1
    fi

    actual=$(sha256sum "${tarball}" | cut -d' ' -f1)
    if [ "${expected}" != "${actual}" ]; then
        echo "Error: Tarball checksum validation failed for ${tarball_name}!"
        echo "Expected: ${expected}"
        echo "Actual: ${actual}"
        exit 1
    fi

    echo "Tarball checksum validation passed for ${tarball_name}: ${actual}"
}

# MOTIVATION
#
# Once upon a time, we encountered a case where the Router binary which was
# inside the built Docker container was corrupted.  This script supports
# validating the Router tarball against a checksum which is calculated out of
# band (in CircleCI, typically) before it is extracted into the image.

case "${SOURCE}" in
"local")
    echo "Using local Router artifact for ${TARGETPLATFORM} from ${LOCAL_ARTIFACTS_DIR}"

    # Exactly one tarball per architecture is expected in the directory.
    shopt -s nullglob
    TARBALLS=("${LOCAL_ARTIFACTS_DIR}"/router-*-"${ARCH}".tar.gz)
    shopt -u nullglob
    if [ "${#TARBALLS[@]}" -ne 1 ]; then
        echo "Error: Expected exactly one router-*-${ARCH}.tar.gz in ${LOCAL_ARTIFACTS_DIR}, found ${#TARBALLS[@]}"
        ls -la "${LOCAL_ARTIFACTS_DIR}"
        exit 1
    fi

    verify_sha256sum "${TARBALLS[0]}" "${LOCAL_ARTIFACTS_DIR}/sha256sums.txt"

    # Extract the tarball, which literally drops a dist/ directory.
    tar -xzf "${TARBALLS[0]}"
    ;;

"release")
    echo "Downloading Router release: ${ROUTER_RELEASE}"
    # Download router tarball directly instead of using installer
    TARBALL_NAME="router-${ROUTER_RELEASE}-${ARCH}.tar.gz"

    # We use the rover-plugin service to download the Router tarball, rather
    # than the actual executable which is what our usual curl installer does.
    #
    # Expanding on that with a couple notes:
    #
    #   - There is, as of the time of this writing, NO fixed Apollo-controlled URL
    #     that lets you download a specific Router release tarball.
    #   - We currently only have the curl installer which downloads and extracts
    #     the tarballs.
    #   - This approach is acceptable and defensive since rover is guaranteed to
    #     have this in order for it to download the router, and that's not going
    #     away. They also go through the same Orbiter endpoint/code anyhow.
    #   - It IS possible to fix orbiter to also serve on the router domain and w
    #     could do that, but this seemed more than acceptable, and is a well-tested
    #     and monitored endpoint.
    #   - The architecture is determined from TARGETPLATFORM, an environment variable
    #     made available by Docker: https://docs.docker.com/build/building/multi-platform/
    #     These are usually from `--platform` values passed within CircleCI's config where
    #     this release process is invoked.

    # Download the router tarball from the rover service
    curl -sSL \
      "https://rover.apollo.dev/tar/router/${ARCH}/${ROUTER_RELEASE}" \
        -o "${TARBALL_NAME}"

    # Download and validate checksum
    curl -sSL \
      "https://github.com/apollographql/router/releases/download/${ROUTER_RELEASE}/sha256sums.txt" \
        -o sha256sums.txt

    verify_sha256sum "${TARBALL_NAME}" sha256sums.txt

    # Extract the tarball, which literally drops a dist/ directory.
    tar -xzf "${TARBALL_NAME}"
    ;;

"artifact_url")
    echo "Downloading Router artifact: ${ARTIFACT_URL}"

    curl -sSL -H "Circle-Token: ${CIRCLE_TOKEN}" -o "artifact.tar.gz" "${ARTIFACT_URL}"

    # Validate checksum if ARTIFACT_URL_SHA256SUM is provided
    if [ -n "${ARTIFACT_URL_SHA256SUM}" ]; then
        ACTUAL_SHA256SUM=$(sha256sum "artifact.tar.gz" | cut -d' ' -f1)
        if [ "${ARTIFACT_URL_SHA256SUM}" != "${ACTUAL_SHA256SUM}" ]; then
            echo "Error: Artifact tarball checksum validation failed!"
            echo "Expected: ${ARTIFACT_URL_SHA256SUM}"
            echo "Actual: ${ACTUAL_SHA256SUM}"
            exit 1
        fi
        echo "Artifact tarball checksum validation passed: ${ACTUAL_SHA256SUM}"
    else
        echo "WARN: No checksum provided for artifact validation"
    fi

    # Extract the tarball, which literally drops a dist/ directory.
    tar -xzf "artifact.tar.gz"
    ;;
esac

echo "Router download and validation completed successfully"
