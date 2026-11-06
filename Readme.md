# Navi

![Navi logo](assets/icons/hicolor/scalable/apps/navi.svg)

[![CI](https://github.com/Yoshiofthewire/navi/actions/workflows/ci.yml/badge.svg)](https://github.com/Yoshiofthewire/navi/actions/workflows/ci.yml)
[![Release build](https://github.com/Yoshiofthewire/navi/actions/workflows/release.yml/badge.svg)](https://github.com/Yoshiofthewire/navi/actions/workflows/release.yml)

A mouse-first tiling Wayland desktop written in Rust with [Smithay](https://smithay.github.io/).
Navi aims to make several large monitors manageable without requiring a keyboard workflow:
just your wallpaper, apps, a compact dock, and a slim status bar.

**Status: working prototype.** Nested operation is tested; real login sessions, monitor hotplug,
and physical input still need hardware validation. See [limitations](#limitations).

## What works

- A 12 × 6 grid on each monitor, with predictable placement and remembered app sizes/positions.
- Drag to move, resize edges or corners, swap tiles, and group windows into tabs.
- Keyboard moves and directional expansion with Alt/Super + Arrow.
- A rounded dock with persistent app pins, categorized app folders, search, and scrolling.
- A top status bar with a clock and battery indicator, plus basic notifications.
- Native Wayland apps and X11 apps through XWayland, including clipboard text transfer.
- Swaylock screen locking from the session menu or Super+L, including lock-before-sleep.
- Nested operation inside another desktop and a DRM/libinput/libseat hardware backend.

The layout and monitor policies are described in [the design document](docs/design.md).

## Requirements

- Linux, Git, and the stable Rust toolchain installed through [rustup](https://rustup.rs/).
- Wayland, xkbcommon, udev, libinput, libseat, GBM, DRM, and an EGL implementation.
- A supported font: Noto Sans, DejaVu Sans, or Liberation Sans.
- The `Xwayland` executable for X11 apps and the XWayland integration test.
- A PAM-enabled `swaylock` package at `/usr/bin/swaylock` for screen locking; install your
  distro package so its PAM service is configured. Navi never handles your password.

On Debian/Ubuntu:

```sh
sudo apt-get update
sudo apt-get install -y build-essential git pkg-config libwayland-dev libxkbcommon-dev \
  libudev-dev libinput-dev libseat-dev libgbm-dev libdrm-dev libegl1 xwayland swaylock fonts-noto-core \
  meson ninja-build wayland-protocols libcairo2-dev libpam0g-dev curl patch
```

On Arch Linux/CachyOS:

```sh
sudo pacman -S --needed base-devel git pkgconf wayland libxkbcommon systemd-libs \
  libinput seatd mesa libglvnd libdrm xorg-xwayland swaylock noto-fonts meson ninja wayland-protocols cairo pam
```

## Build and try it

```sh
git clone https://github.com/Yoshiofthewire/navi.git
cd navi
cargo build --release --locked
./target/release/navi
```

Run that last command from an existing Wayland or X11 desktop to open Navi as a nested window.
Keep `navi`, `navi-bar`, `navi-swaylock` and `navi-settings` together. Navi starts the bar automatically; Settings opens on demand.
Cargo also builds the small locker helper from pinned swaylock 1.8.6 source, verified by SHA-256,
with a rendering-only patch for the eight-slice indicator. The first build needs network access;
the verified source and native build are cached under `target/navi-locker`.
Build as your normal user. For a debug build, use `cargo build --locked` and `./target/debug/navi`.

## Controls

| Action | Control |
| --- | --- |
| Move a window | Drag its top grab strip |
| Resize | Drag an edge or corner |
| Extend an edge | Double-click that edge |
| Swap windows | Drop one tile onto another |
| Make a tab group | Hold a dragged tile over another tile's middle, then drop |
| Move one grid cell | **Alt + Left / Right / Up / Down** |
| Expand to the next window or usable monitor edge | **Super + Left / Right / Up / Down** |
| Lock screen / retry a crashed locker | **Super + L** |
| Quit Navi while unlocked | **Ctrl + Alt + Backspace** |
| Switch VT in a hardware session | **Ctrl + Alt + F1…F12** |

Alt+Arrow preserves size and stops at occupied cells. Super+Arrow expands the selected side
without shrinking neighbors. Both act on the focused tab group and remember successful changes.
Maximized and fullscreen windows keep their geometry. Floating dialogs and the launcher retain
their app shortcuts. A host compositor may intercept shortcuts while Navi is nested.

Click the Navi button to open the app launcher. Browse folders or type to search, use the
Pin/Unpin controls to customize the dock, and scroll with a wheel or touchpad.

## Install as a login session

After building the release binaries:

```sh
sudo install -Dm755 -t /usr/local/bin target/release/navi target/release/navi-bar target/release/navi-swaylock target/release/navi-settings
sudo install -Dm644 -t /usr/share/wayland-sessions dist/navi.desktop
sudo install -Dm644 -t /usr/share/xdg-desktop-portal dist/navi-portals.conf
```

Choose **navi** in your display manager. The hardware backend requires an active seat session;
it is selected when both `WAYLAND_DISPLAY` and `DISPLAY` are absent or empty.
File pickers use an installed KDE or GTK desktop-portal backend.

Read the [session guide](docs/session.md) for console testing, recovery, keyboard layout,
uninstall instructions, and the hardware features that still need validation.

## Settings and state

Paths follow `XDG_CONFIG_HOME` and `XDG_STATE_HOME`, with the usual home-directory defaults.

| File | Purpose |
| --- | --- |
| `~/.config/navi/monitors` | Monitor arrangement, scale, and main output |
| `~/.config/navi/wallpaper` | Per-monitor wallpaper |
| `~/.config/navi/pins` | Pinned desktop entries |
| `~/.local/state/navi/memory` | Remembered app sizes and positions |
| `~/.local/state/navi/bar` | Dock's selected monitor |
| `~/.local/state/navi/navi.log` | Hardware-session log; previous run is `navi.log.old` |

The first hardware start can import KWin's monitor arrangement. Wallpaper settings can be
imported from Plasma. Original KDE configuration files are left intact.

## Development and CI

The [CI workflow](https://github.com/Yoshiofthewire/navi/blob/main/.github/workflows/ci.yml) checks formatting, Clippy, debug tests/build,
real headless XWayland integration, and startup failure without a display or seat.
The [release workflow](https://github.com/Yoshiofthewire/navi/blob/main/.github/workflows/release.yml) runs release-profile tests and the same
integration/fail-safe checks, then builds all four binaries and uploads `navi-linux-x86_64.tar.gz`.
Both workflows run for pull requests and pushes to `main`, and support manual runs.

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo test --locked xwayland::tests::x11_lifecycle_and_input -- --ignored
cargo test --locked locking::tests::swaylock_real_client_handshake -- --ignored
cargo build --locked
```

The explicit XWayland test starts a real X server without a host display or GPU. It checks
window lifecycle, input, focus, clipboard transfer, and hostile metadata/clipboard offers.
The monitor-probe test remains manual because it reads the host's hardware.

Release artifacts are built on Ubuntu 24.04 for Linux x86_64 and require compatible system
libraries. Download them from successful **Release build** runs in GitHub Actions. The archive
contains `navi`, `navi-bar`, `navi-swaylock`, `navi-settings`, the `dist/` session files, `docs/`, icons, this README, and the license. Workflows
cache compiled dependencies; tests still execute on every run. CI does not publish GitHub releases.
Extract the archive, keep all four binaries together, and run `./navi` from an existing desktop.

## Limitations

- Real login sessions, multi-monitor hotplug, VT switching, and physical touchpad behavior
  remain unverified on hardware.
- Screen-sharing/screenshot portals are not implemented.
- Locking, PAM unlock, sleep/resume and logind handling still need hardware validation.
  Nested locking protects only Navi’s window; your host desktop manages the rest.
- One GPU, connector-advertised display modes, and no HDR, VRR, rotation, or mirroring.
- XWayland primary selection, cross-protocol drag-and-drop, and automatic XWayland restart
  are not implemented.
- Shell accessibility integration, virtual desktops, and volume/network/system-tray indicators
  are pending. Network, Bluetooth and sound controls are available in Settings.
- Nested rendering sleeps between changes; hardware idle CPU and full-session benchmarks
  remain unverified. See [performance measurements](docs/performance.md).

## License

[MIT](LICENSE.txt).

## Settings

Open **Navi Settings** in the launcher's Settings folder. It runs on demand and uses native
GTK 4 controls. Install runtime dependencies on Debian/Ubuntu with
`sudo apt-get install python3-gi gir1.2-gtk-4.0`, or on Arch with
`sudo pacman -S python-gobject gtk4`. Python/GTK are not compositor runtime dependencies.
Install `dist/navi-settings.desktop` into `/usr/local/share/applications` for other launchers.
See [Settings](docs/settings.md) for built-in controls, advanced feature limits and validation.

Built-in network and Bluetooth controls need NetworkManager and BlueZ. Sound controls need
PipeWire-Pulse or PulseAudio and `pactl` (`pulseaudio-utils` on Debian/Ubuntu, `libpulse` on Arch).


Settings also includes Appearance, Notifications (with optional UnifiedPush **off by default**),
System Sounds, Accessibility, Printers, Accounts, and Date/Time/Language. Display changes retain
20-second recovery. See [the controls and limits](docs/settings.md) before hardware validation.
Optional services/tools: `kunifiedpush`, `canberra-gtk-play`, CUPS clients, AccountsService,
power-profiles-daemon and a Polkit agent; install only what you use. Navi does not install or
activate these system services for you. The UnifiedPush child contacts a provider only after
explicit opt-in in a full session.
