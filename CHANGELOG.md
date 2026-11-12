# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `rust-toolchain.toml` and a declared minimum Rust version of 1.92.

### Changed

- The deb package now computes its library dependencies and recommends the
  AppIndicator extension for the top-bar icon.
- More terminal emulators are recognised on X11, so Shift+Enter is not needed
  for them.

## [0.1.0] - 2026-11-12

First release.

### Added

- Clipboard history with text, rich text and images, pinned items and search.
- Emoji and symbol pickers in the same popup. Inserting one does not enter the
  history and the previous clipboard content is restored afterwards.
- Pasting into the previously active window, terminals included, on Wayland
  (virtual keyboard through `/dev/uinput`) and X11 (XTest).
- Clipboard capture without polling on X11 and on Wayland compositors that offer
  a data-control protocol. The KDE and wlroots paths are untested.
- Skips copies marked by password managers and anything matching your own
  regular-expression exclusion patterns; recording can be paused.
- Preferences for history size, automatic clearing, shortcut, start at login and
  an optional top-bar icon.
- `.deb` and `.rpm` packages.
