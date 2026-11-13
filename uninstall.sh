#!/bin/sh
# Removes Tuck completely: stops it, undoes the keyboard shortcut and the
# start-at-login entry it set up, removes the package, and deletes its history
# and settings.
#
#   curl -fsSL https://raw.githubusercontent.com/techbysakh963/tuck/main/uninstall.sh | sh
#
# Options (after `sh -s --` when piping):
#   --keep-data  keep your history and settings
#   --dry-run    only print what would be done
set -eu

say() { printf 'tuck: %s\n' "$*"; }
fail() { printf 'tuck: %s\n' "$*" >&2; exit 1; }

KEEP=0
DRY=0
for arg in "$@"; do
    case "$arg" in
        --keep-data) KEEP=1 ;;
        --dry-run) DRY=1 ;;
        *) fail "unknown option $arg" ;;
    esac
done

[ -n "${HOME:-}" ] || fail "HOME is not set"

run() {
    if [ "$DRY" = 1 ]; then
        printf 'would run: %s\n' "$*"
    else
        "$@"
    fi
}

SUDO=""
[ "$(id -u)" = 0 ] || SUDO="sudo"

data="${XDG_DATA_HOME:-$HOME/.local/share}/tuck"
config="${XDG_CONFIG_HOME:-$HOME/.config}/tuck"
autostart="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/io.github.techbysakh963.Tuck.desktop"

say "stopping Tuck"
run pkill -x tuck || true
[ "$DRY" = 1 ] || sleep 1

if command -v tuck >/dev/null 2>&1; then
    say "removing its keyboard shortcut and restoring GNOME's own"
    if ! run tuck --reset; then
        say "could not reset the shortcut automatically; open Settings > Keyboard > Shortcuts and remove the Tuck entry"
    fi
fi

if command -v dpkg >/dev/null 2>&1 && dpkg -s tuck >/dev/null 2>&1; then
    say "removing the package"
    run $SUDO apt-get purge -y tuck
elif command -v rpm >/dev/null 2>&1 && rpm -q tuck >/dev/null 2>&1; then
    say "removing the package"
    run $SUDO rpm -e tuck
else
    say "no installed package found, skipping"
fi

run rm -f "$autostart"

if [ "$KEEP" = 1 ]; then
    say "keeping your history ($data) and settings ($config)"
else
    say "deleting your history and settings"
    run rm -rf "$data" "$config"
fi

if [ "$DRY" = 1 ]; then
    say "dry run finished, nothing was changed"
else
    say "Tuck is removed"
fi
