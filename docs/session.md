# navi as a login session

## Install

From the repo, build as yourself and install with sudo:

```sh
cargo build --release --locked && sudo install -Dm755 -t /usr/local/bin target/release/navi target/release/navi-bar && sudo install -Dm644 -t /usr/share/wayland-sessions dist/navi.desktop && sudo install -Dm644 -t /usr/share/xdg-desktop-portal dist/navi-portals.conf
```

Then pick "navi" on the login screen.

Uninstall:

```sh
sudo rm /usr/local/bin/navi /usr/local/bin/navi-bar /usr/share/wayland-sessions/navi.desktop /usr/share/xdg-desktop-portal/navi-portals.conf
```

## Ways out

- **Ctrl+Alt+Backspace** quits navi and the login screen comes back.
- **Ctrl+Alt+F2** (F1…F12) switches to a text console.
- If navi can't start (no seat session, no GPU, no monitor), it exits with
  `navi: cannot start on real hardware: …` and the login screen comes back.

navi itself never launches apps; the taskbar (navi-bar) does. If navi-bar crashes, navi starts it
again, up to 5 times a minute. Past that there's no taskbar: switch to a console, or quit and log
in to Plasma.

## Files

- **Log:** `~/.local/state/navi/navi.log`, replaced at each start, also on stderr.
- **Monitors:** `~/.config/navi/monitors`, one `<identity> <x> <y> <scale>` line per monitor and
  an optional `main <identity>`. The first start imports KWin's `~/.config/kwinoutputconfig.json`
  once; a bad file is ignored, never rewritten. See *Monitors* in `design.md`.
- **Taskbar position:** `~/.local/state/navi/bar`.
- **Keyboard layout:** xkb's `XKB_DEFAULT_LAYOUT`, `XKB_DEFAULT_VARIANT`, `XKB_DEFAULT_OPTIONS`
  (and `_MODEL`, `_RULES`) in the session's environment. Plasma's layout isn't imported yet.
- **Touchpads** get tap-to-click.

## What works and what doesn't

- navi sets `XDG_CURRENT_DESKTOP=navi` (unless set) and `XDG_SESSION_TYPE=wayland` for apps, and
  exports them with `WAYLAND_DISPLAY` to D-Bus and systemd's user manager, so portals find it.
  `navi-portals.conf` picks the KDE portal, then GTK's, for file pickers.
- Screen sharing and screenshots through portals don't work: navi has no capture protocol yet.
- XWayland isn't in yet: X11-only apps won't start.
- One GPU only, each monitor at its preferred mode. No VRR, HDR, rotation or mirroring.

## Unproven until Yoshi tries it

Nothing here has run on real hardware yet: login, the three monitors and their arrangement,
hotplug, VT switching, the taskbar hover, tap-to-click, the D-Bus export and the file picker.
