#! /bin/sh

###
# Build docker images from git commit hash or tag or from released tarballs.
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
###

###
# Terminate with a nice usage message
###
usage () {
   printf "Usage: build_docker_image.sh [-b] [<release>]\n"
   printf "\t-b build docker image from a repo, if not present build from a released tarball\n"
   printf "\t<release> a valid release. If [-b] is specified, this is optional\n"
   printf "\tExample 1: Building HEAD from the repo\n"
   printf "\t\tbuild_docker_image.sh -b\n"
   printf "\tExample 2: Building tag from the repo\n"
   printf "\t\tbuild_docker_image.sh -b v0.9.1\n"
   printf "\tExample 3: Building commit hash from the repo\n"
   printf "\t\tbuild_docker_image.sh -b 7f7d223f42af34fad35b898d976bc07d0f5440c5\n"
   printf "\tExample 4: Building tag v0.9.1 from the released tarball\n"
   printf "\t\tbuild_docker_image.sh v0.9.1\n"
   exit 2
}

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

###
# Process Command Line
###
if ! args=$(getopt bh "$@"); then
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
       -b)
               BUILD_IMAGE=true
               shift
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
    ROUTER_VERSION="${1}"
else
    if [ "${BUILD_IMAGE}" = false ]; then
        usage
    fi
fi

# We need a place to build
if ! BUILD_DIR=$(mktemp -d -t "router-build.XXXXXXXXXX"); then
    echo "Couldn't make build directory, terminating..."
    exit 1
fi

echo "Building in: ${BUILD_DIR}"

# Copy in our dockerfiles, we'll need them later
cp dockerfiles/* "${BUILD_DIR}" || terminate "Couldn't copy dockerfiles to ${BUILD_DIR}"
cp ../router.yaml "${BUILD_DIR}" || terminate "Couldn't copy ../router.yaml to ${BUILD_DIR}"

# Change to our build directory
cd "${BUILD_DIR}" || terminate "Couldn't cd to ${BUILD_DIR}";
 
# If we are building, clone our repo
if [ "${BUILD_IMAGE}" = true ]; then
    git clone https://github.com/apollographql/router.git > /dev/null 2>&1 || terminate "Couldn't clone repository"
    cd router || terminate "Couldn't cd to router"
    # Either unset or blank (equivalent for our purposes)
    if [ -z "${ROUTER_VERSION}" ]; then
        ROUTER_VERSION=$(git rev-parse HEAD)
    fi
    # Let the user know what we are going to do
    echo "Building image: ${ROUTER_VERSION}" from repo""
    git checkout "${ROUTER_VERSION}" > /dev/null 2>&1 || terminate "Couldn't checkout ${ROUTER_VERSION}"
    # Build our docker images
    docker build -q -t "router:${ROUTER_VERSION}" \
        --build-arg ROUTER_VERSION="${ROUTER_VERSION}" \
        --no-cache -f ../Dockerfile.repo . \
        || terminate "Couldn't build router image"
else
    # Let the user know what we are going to do
    echo "Building image: ${ROUTER_VERSION}" from released tarballs""
    ROUTER_RELEASE="$(echo "${ROUTER_VERSION}" | cut -c2-)"
    docker build -q -t "router:${ROUTER_VERSION}" \
        --build-arg ROUTER_RELEASE="${ROUTER_RELEASE}" \
        --no-cache -f Dockerfile.release . \
        || terminate "Couldn't build router image"
fi

echo "Image built!"

echo "Cleaning up build directory: ${BUILD_DIR}"

# Finally cleanup our BUILD_DIR
# let's be defensive...
if [ -n "${BUILD_DIR}" ]; then
    rm -rf "${BUILD_DIR}"
fi
