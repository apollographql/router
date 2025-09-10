#!/bin/bash
set -e

# Download and validate Router binary This script handles both release downloads
# and artifact downloads with checksum validation NOTE: This script is only
# intended to be executed from inside Dockerfile.router which lives besides
# this.

# Validate required environment variables early
if [ -z "${ARTIFACT_URL}" ]; then
    # Release build path - requires ROUTER_RELEASE
    if [ -z "${ROUTER_RELEASE}" ]; then
        echo "Error: ROUTER_RELEASE environment variable is required for release builds"
        exit 1
    fi

    # Release build path - requires TARGETPLATFORM (a variable set by Docker buildx)
    if [ -z "${TARGETPLATFORM}" ]; then
        echo "Error: TARGETPLATFORM environment variable is required for release builds"
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
else
    # Artifact build path - requires CIRCLE_TOKEN.
    if [ -z "${CIRCLE_TOKEN}" ]; then
        echo "Error: CIRCLE_TOKEN environment variable is required when ARTIFACT_URL is set"
        exit 1
    fi
    # ARTIFACT_URL_SHA256SUM is optional but recommended
fi

# MOTIVATION
#
# Once upon a time, we encountered a case where the Router binary which was
# inside the built Docker container was corrupted.  This script supports
# downloading the Router binary and validating it against a provided checksum
# which is calculated out of band (in CircleCI, typically) and passed into the
# Docker build as an optional environment variables.

# ARTIFACT_URL is used for nightly builds, which are built in CircleCI and
# downloaded from the CircleCI API.  If ARTIFACT_URL is not set, we assume we
# are building a release and will download the Router binary from the official
# release URL.

if [ -z "${ARTIFACT_URL}" ]; then
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

    # Extract the expected checksum for the tarball
    EXPECTED_SHA256SUM=$(grep "${TARBALL_NAME}" sha256sums.txt | cut -d' ' -f1)
    if [ -z "${EXPECTED_SHA256SUM}" ]; then
        echo "ERROR: Could not find checksum for ${TARBALL_NAME} in sha256sums.txt"
        exit 1
    fi

    # Calculate actual checksum of downloaded tarball
    ACTUAL_SHA256SUM=$(sha256sum "${TARBALL_NAME}" | cut -d' ' -f1)

    if [ "${EXPECTED_SHA256SUM}" != "${ACTUAL_SHA256SUM}" ]; then
        echo "Error: Tarball checksum validation failed!"
        echo "Expected: ${EXPECTED_SHA256SUM}"
        echo "Actual: ${ACTUAL_SHA256SUM}"
        exit 1
    fi

    echo "Tarball checksum validation passed: ${ACTUAL_SHA256SUM}"

    # Extract the tarball, which literally drops a dist/ directory.
    tar -xzf "${TARBALL_NAME}"
else
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
fi

echo "Router download and validation completed successfully"
