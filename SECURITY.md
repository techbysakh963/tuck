# Security policy

## Reporting a vulnerability

Please report security problems privately. Open the repository's **Security**
tab and choose **Report a vulnerability**. Do not open a public issue for
anything that could be exploited.

Include the version (`tuck --version` or the package version), your distribution
and session type, and the steps to reproduce.

## Scope

Tuck stores what you copy, so the issues that matter most are:

- **Stored clipboard data.** Anything that makes history, images or logs readable
  by another user, or leaks outside the owner-only files under
  `~/.local/share/tuck`.
- **The uinput rule.** The packages install a udev rule that gives the logged-in
  desktop user access to `/dev/uinput` so Tuck can type the paste shortcut on
  Wayland. Problems with that rule or with how Tuck uses the device are in scope.
- Capturing copies that should have been skipped, such as those marked by
  password managers.

## Supported versions

Only the latest release receives fixes.
