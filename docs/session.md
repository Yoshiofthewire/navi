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

## Moving and resizing with the keyboard

- **Alt + Left/Right/Up/Down:** move the focused tile one grid cell, preserving its size and
  stopping at occupied cells or the current monitor's usable edge.
- **Super + Left/Right/Up/Down:** expand that side to the next window or usable monitor edge.
  Other windows stay put. Panels keep their space.

These work for native Wayland and X11 tiles and move/resize a tab group together. Changes are
remembered. Maximized/fullscreen windows keep their geometry. Floating dialogs and launcher
surfaces keep their app shortcuts. When testing nested, the host compositor may reserve these
chords before Navi receives them; a full Navi session receives them directly.

## Power and session menu

Click **Navi** at the upper left of the status bar for **Sleep**, **Lock Screen**, **Switch User**,
**Log Out**, **Restart**, and **Shutdown**. Use the pointer, Up/Down or Tab, and Enter. Each enabled
action opens a confirmation with **Cancel** selected; Escape cancels or closes the menu.

In a nested window **Log Out** closes that Navi instance; **Lock Screen** protects its window. A full session
uses login1 for sleep, restart, and shutdown, preserving its authorization and inhibitor checks.
Actions requiring an authorization prompt are unavailable until Navi has an authentication agent.
Switch User requires a display manager that advertises `CanSwitch` at this session's
`XDG_SEAT_PATH`; without that exact seat it is disabled.

Lock Screen uses the distro's PAM-enabled `/usr/bin/swaylock`. Install `swaylock` with your
package manager; Navi does not install or alter PAM configuration. **Super+L** locks immediately;
the upper-left Navi menu also offers Lock Screen with confirmation. Nested, Lock Screen and
Log Out are available; the lock protects Navi's window only, never the host desktop.

Sleep and Switch User wait until all Navi outputs have presented protected content. A failed
lock aborts those menu actions. Full sessions also listen for their login1 session's `Lock`
signal (`loginctl lock-session`) and `PrepareForSleep`, retaining a sleep delay inhibitor until
coverage is confirmed. On failure, Navi keeps its input/rendering shield and logs the problem;
logind may still enforce its configured maximum delay. Resume never unlocks automatically.
Listener/DBus failures are logged; verify them on your actual login session before relying on
lid-close or external suspend locking. There is no idle timer yet.

If swaylock crashes, every output stays protected, and Navi retries it (at most five starts per
minute). **Super+L** retries after that limit has elapsed. No app connection can take over the
lock. Ctrl+Alt+Backspace is disabled while locked; VT switching remains available for recovery.
If recovery fails, use another VT to repair/install swaylock, then return and retry; ending Navi
from that VT ends the session and loses unsaved work. Navi uses a fixed solid background and
blue ring and ignores swaylock's per-user config to keep the supervised process in the foreground.

Authentication stays entirely in swaylock/PAM. No fresh browser SSO, SCIM provisioning or IdP
configuration is added here. Central identities can use the OS identity/PAM bridge later; test
any multi-step SSO/MFA unlock flow separately against swaylock's conversation support.

Validation: protocol tests cover output coverage, rejected/early unlock, hotplug, password-key
routing and crash protection. The isolated real-swaylock test starts a private compositor,
maps its SHM buffers and kills only that child; it never submits a password or touches the host
session. DRM, PAM authentication, real logind suspend and physical multi-monitor behavior still
require hardware validation. Debug builds accept an absolute `NAVI_TEST_SWAYLOCK` path solely
for testing an extracted package with input replay; release Navi always uses `/usr/bin/swaylock`.
Failed menu requests show an error; details go to the session log.

## When it fails

If navi can't start (no seat session, no GPU, no monitor), it exits with
`navi: cannot start on real hardware: …` and the login screen comes back. Why is in
`~/.local/state/navi/navi.log`, or `journalctl -b` if that file is missing. Each start moves the
previous log to `navi.log.old`, so after one more failed login the first one's log is still
there.

navi itself never launches apps; the taskbar (navi-bar) does. A confirmed Log Out closes navi;
if navi-bar crashes, navi starts it
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
- X11 apps run through XWayland when the `Xwayland` executable is installed (Arch:
  `xorg-xwayland`, Debian/Ubuntu: `xwayland`). Missing or failed XWayland leaves native Wayland
  apps available; the reason is logged. X11 apps share Navi's tiling, controls, and dock.
- Clipboard text transfers between X11 and native Wayland apps. X11 clipboard access requires
  an X11 window to hold keyboard focus. Primary selection and cross-protocol drag-and-drop
  are not implemented. X11 apps share one X server and its usual mutual access.
- Navi gives apps its own `DISPLAY`, including when nested; it exports that display to D-Bus
  and systemd only in a real session. Nested runs leave the host's activation environment alone.
- XWayland isn't automatically restarted after a crash. Restart Navi to use X11 apps again.
- One GPU only, each monitor at its preferred mode. No VRR, HDR, rotation or mirroring.

## Unproven until Yoshi tries it

Nothing here has run on real hardware yet: login, the three monitors and their arrangement,
hotplug, VT switching, the taskbar hover, tap-to-click and scroll direction on the laptop's
touchpad, the keyboard layout from `environment.d`, the D-Bus export and the file picker.
