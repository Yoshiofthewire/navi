# navi as a login session

## Install

From the repo, build as yourself and install with sudo:

```sh
cargo build --release --locked && sudo install -Dm755 -t /usr/local/bin target/release/navi target/release/navi-bar && sudo install -Dm644 -t /usr/share/wayland-sessions dist/navi.desktop && sudo install -Dm644 -t /usr/share/xdg-desktop-portal dist/navi-portals.conf
```

The same, a step at a time, with the build capped so it can't take the desktop down with it:

```sh
systemd-run --user --scope -p MemoryMax=6G -p MemorySwapMax=0 -- cargo build --release --locked
sudo install -Dm755 -t /usr/local/bin target/release/navi target/release/navi-bar
sudo install -Dm644 -t /usr/share/wayland-sessions dist/navi.desktop
sudo install -Dm644 -t /usr/share/xdg-desktop-portal dist/navi-portals.conf
```

Uninstall:

```sh
sudo rm /usr/local/bin/navi /usr/local/bin/navi-bar /usr/share/wayland-sessions/navi.desktop /usr/share/xdg-desktop-portal/navi-portals.conf
```

Your settings in `~/.config/navi` and `~/.local/state/navi` stay; delete them for a fresh start.

## First try: from a text console

Safer than the login screen: switch to a console (Ctrl+Alt+F3), log in, and run `navi`. There is
no `WAYLAND_DISPLAY` or `DISPLAY` there, so it takes the real-hardware path. Errors print right
there, and Ctrl+Alt+Backspace brings the console back. Then pick "navi" on the login screen.

## Ways out

- **Ctrl+Alt+Backspace** quits navi; you're back at the login screen (or the console you ran it
  from).
- **Ctrl+Alt+F2** (F1…F12) switches to a text console. **Ctrl+Alt+F1**, or the F-key of the VT
  the session runs on, goes back to navi, which picks up where it was.
- If the keys do nothing (navi hung, or found no keyboard): log in over SSH, or from another
  machine, and `kill` navi; else hold the power button. Ctrl+Alt+SysRq+R, E, I, S, U, B is the
  last resort, where SysRq is enabled.

## When it fails

If navi can't start (no seat session, no GPU, no monitor), it exits with
`navi: cannot start on real hardware: …` and the login screen comes back. Why is in
`~/.local/state/navi/navi.log`, or `journalctl -b` if that file is missing. Each start moves the
previous log to `navi.log.old`, so after one more failed login the first one's log is still
there.

navi itself never launches apps; the taskbar (navi-bar) does. If navi-bar crashes, navi starts it
again, up to 5 times a minute. Past that the log says `navi-bar keeps crashing, running without
a bar`, and there's no taskbar: press Ctrl+Alt+Backspace to get back to the login screen.

## Files

- **Log:** `~/.local/state/navi/navi.log` (also on stderr) and the last run's `navi.log.old`.
- **Monitors:** `~/.config/navi/monitors`, one `<identity> <x> <y> <scale>` line per monitor and
  an optional `main <identity>`. The first start imports KWin's `~/.config/kwinoutputconfig.json`
  once; a bad file is ignored, never rewritten. See *Monitors* in `design.md`.
- **Taskbar position:** `~/.local/state/navi/bar`.
- **Keyboard layout:** US unless set. navi reads xkb's `XKB_DEFAULT_LAYOUT`, `XKB_DEFAULT_VARIANT`,
  `XKB_DEFAULT_OPTIONS` (and `_MODEL`, `_RULES`) from its environment. Set them in
  `~/.config/environment.d/navi.conf`, e.g. `XKB_DEFAULT_LAYOUT=de`. systemd's user manager reads
  that file; whether SDDM hands it on to navi is unproven. From a console,
  `XKB_DEFAULT_LAYOUT=de navi` always works. Plasma's layout isn't imported yet.
- **Touchpads** get tap-to-click.

## What works and what doesn't

- navi sets `XDG_CURRENT_DESKTOP=navi` (unless set) and `XDG_SESSION_TYPE=wayland` for apps, and
  exports them with `WAYLAND_DISPLAY` to D-Bus and systemd's user manager, so portals find it.
  `navi-portals.conf` picks the KDE portal, then GTK's, for file pickers.
- Apps see each monitor by its connector (`DP-11`), never its serial number.
- Screen sharing and screenshots through portals don't work: navi has no capture protocol yet.
- XWayland isn't in yet: X11-only apps won't start.
- One GPU only, each monitor at its preferred mode. No VRR, HDR, rotation or mirroring.

## Unproven until Yoshi tries it

Nothing here has run on real hardware yet: login, the three monitors and their arrangement,
hotplug, VT switching, the taskbar hover, tap-to-click and scroll direction on the laptop's
touchpad, the keyboard layout from `environment.d`, the D-Bus export and the file picker.
