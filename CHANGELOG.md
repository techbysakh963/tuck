# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-11-13

First release.

### Added

- Clipboard history with text, rich text, images and copied files, pinned items
  and search. Copied files are kept as a file list and paste back as files.
- Emoji and symbol pickers in the same popup. Inserting one does not enter the
  history; afterwards the previous clipboard item is restored if it is still in
  the history.
- Pasting into the previously active window, terminals included, on Wayland
  (virtual keyboard through `/dev/uinput`) and X11 (XTest). Many X11 terminal
  emulators are recognised, so Shift+Enter is rarely needed for them.
- Clipboard capture without polling on X11 and on Wayland compositors that offer
  a data-control protocol. The KDE and wlroots paths are untested.
- Skips copies that carry the `x-kde-passwordManagerHint` marker set by
  supporting password managers, and anything matching your own
  regular-expression exclusion patterns; recording can be paused.
- Preferences for history size, automatic clearing, shortcut, start at login and
  an optional top-bar icon.
- `.deb` and `.rpm` packages. The deb computes its library dependencies and
  recommends the AppIndicator extension for the top-bar icon.
- `rust-toolchain.toml` and a declared minimum Rust version of 1.92.

### Fixed

- A copy whose password-manager marker cannot be read is skipped rather than
  recorded.
- Stored data is bounded: copies over 32 MiB are skipped, images are decoded
  under size limits, and the oldest unpinned items are removed once stored
  images and large items pass 256 MB.
- The clipboard watcher reconnects on its own after losing the connection and
  tells you while it is down.
- The top-bar icon appears when the tray host starts after Tuck.
- Launching Tuck from the app grid opens the popup, also when Tuck is not
  running yet (the package now installs a D-Bus service file).
- The popup stays open while its menu or one of its dialogs is showing, and
  closes only when you really switch to another application.
- Emoji that the installed fonts cannot draw are left out of the grid instead of
  showing as empty boxes.
