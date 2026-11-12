# Tuck

Clipboard history for GNOME, in the spirit of Win+V: one popup with
Clipboard, Emoji and Symbols tabs. Built with GTK 4 and libadwaita, so it
follows your theme, accent colour and fonts without any styling of its own.

Tuck stays resident, watches the clipboard without polling, and pastes into
the app you were using, terminals included.

## Building

Requires Rust (stable), GTK 4.14 or newer and libadwaita 1.5 or newer.

On Ubuntu 24.04 or Debian:

    sudo apt-get install libgtk-4-dev libadwaita-1-dev libxkbcommon-dev pkg-config

Then:

    cargo build --release
    cargo run

## Security note

On Wayland, Tuck types the paste shortcut through a virtual keyboard
(`/dev/uinput`). The packages install a udev rule that gives the logged-in
desktop user access to that device. This means any program running as that
user can also inject keystrokes. If you do not accept that, remove
`/usr/lib/udev/rules.d/70-tuck-uinput.rules`; on X11 Tuck uses XTest and does
not need the device.

Clipboard history is stored under your user data directory, readable only by
you (mode 0700 for directories and 0600 for files).

## License

GPL-3.0-or-later. See `LICENSE`.
