# Contributing

Bug reports and pull requests are welcome.

## Building

You need Rust 1.92 or newer (the repository pins a toolchain in
`rust-toolchain.toml`), GTK 4.14 or newer and libadwaita 1.5 or newer.

On Ubuntu 24.04 or Debian:

    sudo apt-get install libgtk-4-dev libadwaita-1-dev libxkbcommon-dev pkg-config

Then `cargo build` and `cargo run`.

## Checks

Every commit should pass all three:

    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

## Style

- Use libadwaita widgets and stock style classes. No custom CSS or colours.
- Tuck must stay light: no polling or timers while idle, and no animations or
  redraw loops when nothing changes.
- Commit subjects are short and imperative ("Add history store with pinned
  items"). Use the body to explain why. One logical change per commit.
- Add tests for logic that does not need a display.
