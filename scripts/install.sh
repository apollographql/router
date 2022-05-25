#!/bin/sh
#
# This script will download the latest version of the Apollo Router
# You can specify the version to download with the by setting the $VERSION environment variable.
# If not set the latest version will be downloaded.
#

set -u

download_binary() {
    need_cmd curl
    need_cmd chmod
    need_cmd mkdir
    need_cmd rm
    need_cmd rmdir
    need_cmd tar
    need_cmd which
    need_cmd dirname
    need_cmd awk
    need_cmd cut

    ARG_VERSION=${1:-"latest"}

    # ${VERSION:-} checks if version exists, and if doesn't uses the default
    if [ -z "${VERSION:-}" ]; then
        # VERSION is either not set or empty
        DOWNLOAD_VERSION=$ARG_VERSION
    else
        # VERSION set and not empty
        DOWNLOAD_VERSION=$VERSION
    fi

    get_architecture || return 1
    _arch="$RETVAL"
    assert_nz "$_arch" "arch"

    _ext=""
    case "$_arch" in
        *windows*)
            _ext=".exe"
            ;;
    esac

    ARG_ARCH=${2:-"$_arch"}

    ARG_OUT_FILE=${3:-"./router"}

    GITHUB_REPO="https://github.com/apollographql/router"

    # Validate token.
    curl -o /dev/null -s $GITHUB_REPO || { echo "Error: Invalid repo, token or network issue!";  exit 1; }

    #_tardir="router-$DOWNLOAD_VERSION-${_arch}"
    #_url="$BINARY_DOWNLOAD_PREFIX/$DOWNLOAD_VERSION/${_tardir}.tar.gz"
    _dir="$(mktemp -d 2>/dev/null || ensure mktemp -d -t router)"
    _file="$_dir/input.tar.gz"
    _router="$_dir/router$_ext"

    _release_download_url="$GITHUB_REPO/releases"
    _router_version=$DOWNLOAD_VERSION
    if [ "$DOWNLOAD_VERSION" = "latest" ]; then
        _response=$(curl -Ls -o /dev/null -w '%{url_effective}' $GITHUB_REPO/releases/latest)
        _router_version=$(echo "$_response" | cut -d'/' -f 8)
        [ "$_router_version" ] || { echo "Error: Failed to get asset version for '$ARG_ARCH', response: $_response" | awk 'length($0)<100' >&2; exit 1; }
    fi;

    say "Downloading release info for '$_release_download_url'"

    # Cut the 'v' prefix
    _name="router-$(echo "$_router_version" | cut -c2-)-$ARG_ARCH.tar.gz"

    _url="$GITHUB_REPO/releases/download/$_router_version/$_name"

    say "Found $_name" 1>&2

    ensure mkdir -p "$_dir"

    # Download asset file.
    say "Downloading router from $_url"
    
    curl -sSfL -H 'Accept: application/octet-stream' "$_url" -o "$_file"
    if [ $? != 0 ]; then
      say "Failed to download $_url"
      say "This may be a standard network error, but it may also indicate"
      say "that Router's release process is not working. When in doubt"
      say "please feel free to open an issue!"
      say "https://github.com/apollographql/router/issues/new/choose"
      exit 1
    fi
    ensure tar xf "$_file" --strip-components 1 -C "$_dir"

    say "Moving $_router to $ARG_OUT_FILE"
    mv "$_router" "$ARG_OUT_FILE"

    _version="$($ARG_OUT_FILE --version)"
    _retval=$?

    say "Moved router version: $_version to $ARG_OUT_FILE"
    say ""
    say "You can now run the Apollo Router using '$ARG_OUT_FILE'"


    chmod +x "$ARG_OUT_FILE"

    ignore rm -rf "$_dir"

    return "$_retval"
}

get_architecture() {
    _ostype="$(uname -s)"
    _cputype="$(uname -m)"

    if [ "$_ostype" = Darwin ] && [ "$_cputype" = i386 ]; then
        # Darwin `uname -s` lies
        if sysctl hw.optional.x86_64 | grep -q ': 1'; then
            _cputype=x86_64
        fi
    fi

    if [ "$_ostype" = Darwin ] && [ "$_cputype" = arm64 ]; then
        # Darwin `uname -s` doesn't seem to lie on Big Sur
        # but we want to serve x86_64 binaries anyway so that they can
        # then run in x86_64 emulation mode on their arm64 devices
        _cputype=x86_64
    fi


    # If we are building a linux container on an M1 chip, let's
    # download a86_64 binaries and assume the docker image is
    # for amd64. We do this because we don't have router binaries
    # for aarch64 for any OS right now. If this changes in the
    # future, we'll need to re-visit this hack.
    if [ "$_ostype" = "Linux" ] && [ "$_cputype" = "aarch64" ]; then
        _cputype="x86_64"
    fi

    case "$_ostype" in
        Linux)
            _ostype=linux
            ;;

        Darwin)
            _ostype=macos
            ;;

        MINGW* | MSYS* | CYGWIN*)
            _ostype=windows
            ;;

        *)
            err "no precompiled binaries available for OS: $_ostype"
            ;;
    esac

    case "$_cputype" in
        x86_64 | x86-64 | x64 | amd64)
            ;;
        *)
            err "no precompiled binaries available for CPU architecture: $_cputype"

    esac

    _arch="$_cputype-$_ostype"

    RETVAL="$_arch"
}

say() {
    green=$(tput setaf 2 2>/dev/null || echo '')
    reset=$(tput sgr0 2>/dev/null || echo '')
    echo "$1" 1>&2
}

err() {
    red=$(tput setaf 1 2>/dev/null || echo '')
    reset=$(tput sgr0 2>/dev/null || echo '')
    say "${red}ERROR${reset}: $1" >&2
    exit 1
}

need_cmd() {
    if ! check_cmd "$1"
    then err "Installation halted. Reason: [command not found '$1' - please install this command]"
    fi
}

check_cmd() {
    command -v "$1" > /dev/null 2>&1
    return $?
}

need_ok() {
    if [ $? != 0 ]; then err "$1"; fi
}

assert_nz() {
    if [ -z "$1" ]; then err "assert_nz $2"; fi
}

# Run a command that should never fail. If the command fails execution
# will immediately terminate with an error showing the failing
# command.
ensure() {
    "$@"
    need_ok "command failed: $*"
}

# This is just for indicating that commands' results are being
# intentionally ignored. Usually, because it's being executed
# as part of error handling.
ignore() {
    "$@"
}

download_binary "$@" || exit 1

