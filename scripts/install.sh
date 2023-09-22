#!/bin/sh
#
# This script will download the latest version of the Apollo Router
# You can specify the version to download with the by setting the $VERSION environment variable.
# If not set the latest version will be downloaded.
#

set -u

BINARY_DOWNLOAD_PREFIX="https://github.com/apollographql/router/releases/download"

# Router version defined in apollo-router's Cargo.toml
# Note: Change this line manually during the release steps.
PACKAGE_VERSION="v1.30.1"

download_binary() {
    downloader --check
    need_cmd mktemp
    need_cmd chmod
    need_cmd mkdir
    need_cmd rm
    need_cmd rmdir
    need_cmd tar
    need_cmd which
    need_cmd dirname
    need_cmd awk
    need_cmd cut

    # if $VERSION isn't provided or has 0 length, use version apollo-router's cargo.toml
    # ${VERSION:-} checks if version exists, and if doesn't uses the default
    # which is after the :-, which in this case is empty. -z checks for empty str
    if [ -z "${VERSION:-}" ]; then
        # VERSION is either not set or empty
        DOWNLOAD_VERSION=$PACKAGE_VERSION
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

    _tardir="router-$DOWNLOAD_VERSION-${_arch}"
    _url="$BINARY_DOWNLOAD_PREFIX/$DOWNLOAD_VERSION/${_tardir}.tar.gz"
    _dir="$(mktemp -d 2>/dev/null || ensure mktemp -d -t router)"
    _file="$_dir/input.tar.gz"
    _router="$_dir/router$_ext"

    say "Downloading router from $_url ..." 1>&2

    ensure mkdir -p "$_dir"
    downloader "$_url" "$_file"
    if [ $? != 0 ]; then
      say "Failed to download $_url"
      say "This may be a standard network error, but it may also indicate"
      say "that Router's release process is not working. When in doubt"
      say "please feel free to open an issue!"
      say "https://github.com/apollographql/router/issues/new/choose"
      exit 1
    fi

    ensure tar xf "$_file" --strip-components 1 -C "$_dir"

    outfile="./router"

    say "Moving $_router to $outfile ..."
    mv "$_router" "$outfile"

    _version="$($outfile --version)"
    _retval=$?

    say ""
    say "You can now run the Apollo Router using '$outfile'"

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


    case "$_ostype" in
        Linux)
            _ostype=unknown-linux-gnu
            ;;

        Darwin)
            _ostype=apple-darwin
            ;;

        MINGW* | MSYS* | CYGWIN*)
            _ostype=pc-windows-msvc
            ;;

        *)
            err "no precompiled binaries available for OS: $_ostype"
            ;;
    esac

    case "$_cputype" in
        x86_64 | x86-64 | x64 | amd64 | aarch64)
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
    echo "$1"
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

# This wraps curl or wget. Try curl first, if not installed,
# use wget instead.
downloader() {
    if check_cmd curl
    then _dld=curl
    elif check_cmd wget
    then _dld=wget
    else _dld='curl or wget' # to be used in error message of need_cmd
    fi

    if [ "$1" = --check ]
    then need_cmd "$_dld"
    elif [ "$_dld" = curl ]
    then curl -sSfL "$1" -o "$2"
    elif [ "$_dld" = wget ]
    then wget "$1" -O "$2"
    else err "Unknown downloader"   # should not reach here
    fi
}

download_binary "$@" || exit 1

