#! /bin/sh

###
# Build docker images from router repo outputs. Including:
#  -  router release artifact
#  -  arbitrary router artifact (-a)
#  -  git commit hash/tag (-b)
#
# See the usage message for more help
# docker_build_image.sh -h
#
# Requirements: To run successfully, you need quite a few utilities installed.
# Most of them are likely to be installed on your OS. Some will require
# installation, such as: docker, git, curl, mktemp, getopt
#
# Note: git is only required if you are building an image from source with the
#       -b flag.
# Note: This utility makes assumptions about the existence of files relative
#       to the directory where it is executed. To work correctly you must
#       execute in the "repo"/dockerfiles/diy directory.
# Note: A debug image is an image where heaptrack is installed. The router
#       is still a release build router, but all memory is being tracked
#       under heaptrack. (https://github.com/KDE/heaptrack)
# Note: When I first wrote this script, I was careful to make sure that various
#       invalid combinations of parameters were detected. As the functionality
#       has grown, I've become less careful. So, take care you don't do things
#       like trying to build an amd64 image on an arm64 machine. It may work,
#       but if it does you'll be waiting around for a long time for it to
#       finish...
#       I'm very happy to take argument verification patches...
#
###

###
# Terminate with a nice usage message
###
usage () {
   printf "Usage: build_docker_image.sh [-b [-r <repo>]] [-d] [-m <arch>] [-n <name>] [<release>]\n"
   printf "\t-a build docker image from a build artifact\n"
   printf "\t-b build docker image from the default repo, if not present build from a released version\n"
   printf "\t-d build debug image, router will run under control of heaptrack\n"
   printf "\t-m override machine architecture. valid options are: amd64 or arm64 (DEFAULT: machine architecture)\n"
   printf "\t-n override image name (DEFAULT: router)\n"
   printf "\t-r build docker image from a specified repo, only valid with -b flag\n"
   printf "\t<release> a valid release. If [-b] is specified, this is optional\n"
   printf "\tExample 1: Building HEAD from the github repo\n"
   printf "\t\tbuild_docker_image.sh -b\n"
   printf "\tExample 2: Building HEAD from a different repo\n"
   printf "\t\tbuild_docker_image.sh -b -r /Users/anon/dev/router\n"
   printf "\tExample 3: Building tag from the repo\n"
   printf "\t\tbuild_docker_image.sh -b v0.9.1\n"
   printf "\tExample 4: Building commit hash from the repo\n"
   printf "\t\tbuild_docker_image.sh -b 7f7d223f42af34fad35b898d976bc07d0f5440c5\n"
   printf "\tExample 5: Building tag v0.9.1 from the released version\n"
   printf "\t\tbuild_docker_image.sh v0.9.1\n"
   printf "\tExample 6: Building a debug image with tag v0.9.1 from the released version\n"
   printf "\t\tbuild_docker_image.sh -d v0.9.1\n"
   printf "\tExample 7: Building an amd64 image from a build artifact\n"
   printf "\t\tbuild_docker_image.sh -m amd64 -a https://github.com/apollographql/router/releases/download/v1.22.0/router-v1.22.0-x86_64-unknown-linux-gnu.tar.gz v1.22.0\n"
   printf "\tExample 8: Building an arm64 image from a build artifact with name my-test\n"
   printf "\t\tbuild_docker_image.sh -m arm64 -n my-test -a https://github.com/apollographql/router/releases/download/v1.22.0/router-v1.22.0-aarch64-unknown-linux-gnu.tar.gz v1.22.0\n"
   exit 2
}
#
###
# Terminate the build and clean up the build directory
###
terminate () {
    echo "${1}, terminating..."
    # let's be defensive...
    if [ -n "${BUILD_DIR}" ]; then
        rm -rf "${BUILD_DIR}"
    fi
    exit 1
}

###
# Globals
###
# If no ROUTER_VERSION specified, we are building HEAD from a repo
ROUTER_VERSION=
BUILD_IMAGE=false
DEBUG_IMAGE=false
DEFAULT_REPO="https://github.com/apollographql/router.git"
GIT_REPO=
ARTIFACT_URL=
IMAGE_NAME=router
PLATFORM="linux/$(uname -m)"

###
# Process Command Line
###
if ! args=$(getopt a:bdhm:n:r: "$@"); then
    usage
fi

# Note: We want word splitting, disable shellcheck warning
# shellcheck disable=SC2086
set -- $args

# You cannot use the set command with a backquoted getopt directly,
# since the exit code from getopt would be shadowed by those of set,
# which is zero by definition.
while :; do
       case "$1" in
       -a)
               ARTIFACT_URL="${2}"
               shift; shift
               ;;
       -b)
               BUILD_IMAGE=true
               shift
               ;;
       -d)
               DEBUG_IMAGE=true
               shift
               ;;
       -m)
               PLATFORM="linux/${2}"
               shift; shift
               ;;
       -n)
               IMAGE_NAME="${2}"
               shift; shift
               ;;
       -r)
               GIT_REPO="${2}"
               shift; shift
               ;;
       -h)
               usage
               ;;
       --)
               shift; break
               ;;
       esac
done

# We only expect 0 or 1 arguments
if [ $# -gt 1 ]; then
    usage
fi

# If we aren't building, we need a ROUTER_VERSION
if [ $# -gt 0 ]; then
    if [ "${BUILD_IMAGE}" = false ] && [ -n "${GIT_REPO}" ]; then
        usage
    fi
    ROUTER_VERSION="${1}"
else
    if [ "${BUILD_IMAGE}" = false ]; then
        usage
    fi
fi

if [ -z "${GIT_REPO}" ]; then
    GIT_REPO="${DEFAULT_REPO}"
fi

# We need a place to build
if ! BUILD_DIR=$(mktemp -d -t "router-build.XXXXXXXXXX"); then
    echo "Couldn't make build directory, terminating..."
    exit 1
fi

echo "Building in: ${BUILD_DIR}"

# Create a subshell to avoid having to cd back
(
cd "$(dirname "${0}")" || terminate "Couldn't cd to source location";

# Copy in our dockerfiles, we'll need them later
mkdir "${BUILD_DIR}/dockerfiles"
cp dockerfiles/Dockerfile.repo "${BUILD_DIR}" || terminate "Couldn't copy dockerfiles to ${BUILD_DIR}"
cp ../Dockerfile.router "${BUILD_DIR}" || terminate "Couldn't copy dockerfiles to ${BUILD_DIR}"
cp ../router.yaml "${BUILD_DIR}/dockerfiles" || terminate "Couldn't copy ../router.yaml to ${BUILD_DIR}"

# Change to our build directory
cd "${BUILD_DIR}" || terminate "Couldn't cd to ${BUILD_DIR}";

# If we are building, clone our repo
if [ "${BUILD_IMAGE}" = true ]; then
    git clone "${GIT_REPO}" router > /dev/null 2>&1 || terminate "Couldn't clone repository"
    cd router || terminate "Couldn't cd to router"
    # Either unset or blank (equivalent for our purposes)
    if [ -z "${ROUTER_VERSION}" ]; then
        ROUTER_VERSION=$(git rev-parse HEAD)
    fi
    # Let the user know what we are going to do
    echo "Building image: ${ROUTER_VERSION} from repo: ${GIT_REPO}"
    git checkout "${ROUTER_VERSION}" > /dev/null 2>&1 || terminate "Couldn't checkout ${ROUTER_VERSION}"
    # Build our docker images
    docker build --platform="${PLATFORM}" -q -t "${IMAGE_NAME}:${ROUTER_VERSION}" \
        --build-arg DEBUG_IMAGE="${DEBUG_IMAGE}" \
        --build-arg ROUTER_VERSION="${ROUTER_VERSION}" \
        --no-cache -f ../Dockerfile.repo . \
        || terminate "Couldn't build router image"
else
    # Let the user know what we are going to do
    if [ -z "${ARTIFACT_URL}" ]; then
        echo "Building image: ${ROUTER_VERSION} from release"
    else
        echo "Building image: ${ROUTER_VERSION} from artifact: ${ARTIFACT_URL}"
    fi
    docker build --platform="${PLATFORM}" -q -t "${IMAGE_NAME}:${ROUTER_VERSION}" \
        --build-arg DEBUG_IMAGE="${DEBUG_IMAGE}" \
        --build-arg ROUTER_RELEASE="${ROUTER_VERSION}" \
        --build-arg ARTIFACT_URL="${ARTIFACT_URL}" \
        --no-cache -f Dockerfile.router . \
        || terminate "Couldn't build router image"
fi
) || terminate "sub-shell execution failed"

echo "Image built!"

echo "Cleaning up build directory: ${BUILD_DIR}"

# Finally cleanup our BUILD_DIR
# let's be defensive...
if [ -n "${BUILD_DIR}" ]; then
    rm -rf "${BUILD_DIR}"
fi
