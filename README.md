# Tuck

Clipboard history for GNOME, in the spirit of Win+V: one popup with
Clipboard, Emoji and Symbols tabs. Built with GTK 4 and libadwaita, so it
follows your theme, accent colour and fonts without any styling of its own.

Tuck stays resident, watches the clipboard without polling, and pastes into
the app you were using, terminals included.

## Features

- Clipboard history for text, rich text and images, with search and pinned items.
- Emoji and symbol pickers in the same popup. Inserting one never lands in your
  history. Afterwards the previous clipboard item is put back, but only when it
  is a recorded history item that is still present; content that was never
  recorded (password-manager copies, excluded or paused copies, anything copied
  before Tuck started) is not restored.
- Pastes straight into the window you were working in, on Wayland and X11.
- Skips copies that carry the sensitive-content hint
  (`x-kde-passwordManagerHint`) set by supporting password managers, and
  anything matching your own patterns. Applications that do not set the hint
  cannot be detected.
- Light on resources: no polling while idle and no web view.

## Requirements

- GNOME 46 or newer (Ubuntu 24.04 or newer), Wayland or X11.
- GTK 4.14 or newer and libadwaita 1.5 or newer.
- GNOME on Wayland is the tested setup. Backends for KDE and wlroots-based
  compositors exist, but they are untested.

## Install

Download the `.deb` (Debian, Ubuntu) or `.rpm` (Fedora) from the
[Releases](https://github.com/techbysakh963/tuck/releases) page and install it
with your package manager, for example:

    sudo apt install ./tuck_0.1.0-1_amd64.deb

Compare the file with `SHA256SUMS` from the same release.

### Build from source

Requires Rust 1.92 or newer, GTK 4.14 or newer and libadwaita 1.5 or newer.

On Ubuntu 24.04 or Debian:

    sudo apt-get install libgtk-4-dev libadwaita-1-dev libxkbcommon-dev pkg-config

Then:

    cargo build --release
    cargo run

A plain build or `cargo run` does not install the uinput udev rule or the
desktop entry, and does not put `tuck` on your `PATH`. Pasting on Wayland needs
the udev rule, so install the package (or install those files by hand from
`data/`).

## First run

Start Tuck from the app grid. It asks whether to open with Super+V, which
replaces GNOME's notification-list shortcut, or with Ctrl+Alt+V, which takes
nothing away. Declining picks Ctrl+Alt+V. You can change it later in
Preferences. Tuck also starts at login; turn that off in Preferences.

## Using Tuck

| Key | Action |
| --- | --- |
| Super+V or Ctrl+Alt+V | Open the popup (whichever you chose) |
| Up, Down | Move through items |
| Enter | Paste the selected item |
| Shift+Enter | Paste with the terminal shortcut (Ctrl+Shift+V) |
| Ctrl+P | Pin or unpin the selected item |
| Delete | Remove the selected item |
| Esc | Close the popup |

Type to search. Switch between Clipboard, Emoji and Symbols with the tabs at
the top. Preferences are in the popup menu.

The optional top-bar icon needs the AppIndicator extension. Ubuntu enables it
by default; on other GNOME systems install `gnome-shell-extension-appindicator`.

## Privacy

- Copies that carry the sensitive-content hint (`x-kde-passwordManagerHint`)
  set by supporting password managers are not recorded. Applications that do
  not set the hint cannot be detected, so their copies are recorded like any
  other.
- In Preferences you can add exclusion patterns (regular expressions); a copy
  matching one is skipped.
- Recording can be paused from Preferences.
- History lives under `~/.local/share/tuck`, settings under `~/.config/tuck`.
  Directories are mode 0700 and files 0600. The log at
  `~/.local/share/tuck/logs/tuck.log` does not contain clipboard content.
- "Clear history" in the popup removes everything except pinned items. To
  remove everything, quit Tuck first (it saves the history again when it
  exits), then delete `~/.local/share/tuck`.

## Troubleshooting

### Paste does nothing on Wayland

Tuck types the paste shortcut through `/dev/uinput`. Check that your user can
access it:

    getfacl /dev/uinput

You should see an entry for your user (`user:<name>:rw-`). If not, log out and
back in so the udev rule from the package applies to your session. If it is
still missing, check that `/usr/lib/udev/rules.d/70-tuck-uinput.rules` exists
and reload with:

    sudo udevadm control --reload-rules
    sudo udevadm trigger --action=change --subsystem-match=misc --sysname-match=uinput

## Known limitations

- Flatpak is not supported. Tuck needs `/dev/uinput` to paste on Wayland, the
  X11 clipboard bridge and the accessibility bus (AT-SPI) to detect terminals,
  none of which the sandbox allows.
- In terminals that do not expose accessibility information, Enter pastes with
  Ctrl+V; use Shift+Enter instead.
- On Wayland the compositor decides where the popup appears.
- The interface is English only.
- The paste shortcut is typed for the keyboard layout that is first in your
  list. With several layouts, the key can be wrong while another layout is
  active. That stays so until Tuck can require GTK 4.18 or newer.

## Uninstall

Removing the package does not touch your GNOME settings or your home
directory, so do this in order:

1. Open Preferences > Keyboard and choose **Reset shortcut**, so a custom
   shortcut does not stay behind.
2. Switch off **Start at login** in Preferences (or delete
   `~/.config/autostart/io.github.techbysakh963.Tuck.desktop`).
3. Quit Tuck from the popup menu.
4. Remove the package with your package manager.
5. Optionally delete your data:

       rm -r ~/.local/share/tuck ~/.config/tuck

## Security note

On Wayland, Tuck types the paste shortcut through a virtual keyboard
(`/dev/uinput`). The packages install a udev rule that gives the logged-in
desktop user access to that device. This means any program running as that
user can also inject keystrokes. If you do not accept that, remove
`/usr/lib/udev/rules.d/70-tuck-uinput.rules`; on X11 Tuck uses XTest and does
not need the device.

Clipboard history is stored under your user data directory, readable only by
you (mode 0700 for directories and 0600 for files).

To report a vulnerability, see `SECURITY.md`.

## License

GPL-3.0-or-later. See `LICENSE`.
