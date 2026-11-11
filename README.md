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

## License

GPL-3.0-or-later. See `LICENSE`.
