#!/bin/sh
# Installs the latest Tuck release: downloads the .deb, checks its SHA-256
# checksum and installs it with apt.
#
#   curl -fsSL https://raw.githubusercontent.com/techbysakh963/tuck/main/install.sh | sh
#
# Option (after `sh -s --` when piping): --no-install downloads and verifies only.
set -eu

REPO="techbysakh963/tuck"
BASE="https://github.com/$REPO/releases/latest/download"

say() { printf 'tuck: %s\n' "$*"; }
fail() { printf 'tuck: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "'$1' is required but not installed"; }

INSTALL=1
for arg in "$@"; do
    case "$arg" in
        --no-install) INSTALL=0 ;;
        *) fail "unknown option $arg" ;;
    esac
done

need curl
need sha256sum
[ "$(uname -m)" = "x86_64" ] || fail "only 64-bit x86 (amd64) packages are published so far"

SUDO=""
if [ "$INSTALL" = 1 ]; then
    need apt-get
    if [ "$(id -u)" != 0 ]; then
        need sudo
        SUDO="sudo"
    fi
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "looking up the latest release"
curl -fsSL -o "$tmp/SHA256SUMS" "$BASE/SHA256SUMS" ||
    fail "cannot download the release, check your connection"
deb=$(awk '$2 ~ /^tuck_[0-9A-Za-z.+~-]+_amd64\.deb$/ { print $2; exit }' "$tmp/SHA256SUMS")
[ -n "$deb" ] || fail "the release has no .deb package"

say "downloading $deb"
curl -fsSL -o "$tmp/$deb" "$BASE/$deb" || fail "cannot download $deb"

say "checking the checksum"
(cd "$tmp" && sha256sum -c --ignore-missing SHA256SUMS >/dev/null) ||
    fail "the checksum does not match, nothing was installed"

if [ "$INSTALL" = 0 ]; then
    say "downloaded and verified $deb (not installed)"
    exit 0
fi

say "installing (this asks for your password)"
$SUDO apt-get install -y "$tmp/$deb"
say "done. Open Tuck from the app grid, or run: tuck"
