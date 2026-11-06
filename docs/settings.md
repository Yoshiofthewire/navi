# Navi Settings

Open **Navi Settings** in the launcher's Settings folder. It is a separate, on-demand GTK 4
application, bundled into the `navi-settings` executable. Python 3, PyGObject and GTK 4 are runtime
requirements only for this application. Closing it leaves no settings daemon.

## First release

- Displays: positions, scale, primary monitor, and connector-advertised resolution/refresh
  modes in a full session. Preview changes, then Keep or Revert within 20 seconds. The
  compositor restores the previous display configuration independently of the app. Nested
  previews never change host monitor modes and are not saved.
- Wallpaper: choose a PNG/JPEG per monitor using a native file picker; updates live.
- Wi-Fi/network: native NetworkManager radio toggle, scanning, saved/open/WPA Personal/WPA3
  connections, hidden networks, Ethernet connect/disconnect and forgetting saved Wi-Fi profiles.
  Passwords travel on D-Bus; remembering them is explicit and NetworkManager owns storage.
  Existing provisioned enterprise profiles can connect; new enterprise/VPN/IP/proxy profile
  editors and legacy WEP setup remain future work.
- Bluetooth: native BlueZ adapter power, bounded discovery, pairing with PIN/passkey and explicit
  confirmation/authorization, connect/disconnect, forget, trust and reported device battery.
  Agent requests verify BlueZ's current unique bus owner and the explicitly requested device.
  Discovery stops after 20 seconds, when leaving the page and on app closure.
- Sound: native PipeWire-Pulse/PulseAudio device/default selection, 0–100% output/input volume,
  mute, and per-application playback volume/mute using the `pactl` client. It needs `pactl`
  (`pulseaudio-utils` on Debian/Ubuntu; `libpulse` on Arch) and the audio service.
- Power/locking: idle lock in minutes (0 disables), and Lock Screen. Lock-before-sleep remains
  enforced by Navi. Screen-off, automatic sleep and power profiles are future controls.
- Mouse/keyboard: libinput pointer speed, natural scrolling and tap-to-click in a full session;
  repeat rate/delay apply to Navi's keyboard. Preferences survive restart and device hotplug.
- Defaults: browser (HTTP/HTTPS), email, directories, PDF and text file associations through GIO.
- About: Navi/OS/kernel versions, processor, memory, home storage and architecture; copy summary.

Wi-Fi/Bluetooth/audio controls are built into Navi Settings. Printers, accounts,
notifications, appearance/theme controls, accessibility expansion and advanced display features
remain subsequent work. This app does not provide a new credential store or authentication agent.

## Configuration and protocol

`~/.config/navi/preferences` is bounded JSON for implemented locking/input preferences. Display
positions retain `navi-monitors 1` and accept optional `mode <identity> <width> <height> <millihertz>`
lines. Disconnected displays' entries remain when connected displays are edited. Malformed
existing configuration is preserved and reported rather than replaced.

A mode unavailable at startup falls back to the preferred mode. Live requests accept only modes
currently advertised by the output; any failed change attempts restoration before reporting an
error. A laptop panel retains lower priority while an external display is connected.

The desktop-device controls are separate from compositor settings. NetworkManager and BlueZ use
the system bus with normal service permissions. A bounded worker pool keeps service requests off
GTK’s main loop; pairing prompts use BlueZ Agent1 and default to cancellation.

The compositor provides a mode-0600 socket `navi-<WAYLAND_DISPLAY>.settings` inside the user's
runtime directory. It accepts capped JSON data and returns bounded status; it accepts no shell
commands, arbitrary config destinations or unlock operation. Requests and image validation run
on one worker with I/O deadlines; geometry changes run on the compositor event loop. Changes
are refused while locked. The app uses a worker pool and GTK's main-loop callbacks so queries
do not block interaction. Only explicit controls change system devices. Audio uses fixed native commands and typed arguments;
network credentials never enter process arguments or application logs.

## Direction contract

**Mode / thesis:** Operate. A familiar searchable category list leads to a focused, directly
usable settings page. Status and recovery stay visible while changes are in progress.

**Own-world:** Inherit Navi's near-black background, dark shell surface, light text and blue
selection/action accent from `src/theme.rs`; use native GTK controls and Noto Sans.

**Story:** Find a task, inspect real current values, edit, apply, and see success or a useful
failure. Device pages explain that system changes affect the computer, including from a nested session.

**First viewport:** A 230px searchable category column beside a scrolling page column capped
at approximately 820px, with current connection status above the page. Below 760px, a single
line of native search/category controls replaces the sidebar. Rows stack labels above wider
controls when the content area is narrower than 540px; switches and single actions stay beside
their labels. Displays opens by default, with each output's values and an explicit preview
action. A countdown with Revert and Keep appears after applying.

**Form:** Conventional native settings sidebar and forms, selected from the already approved
settings task. Code-first, established Navi visual world; no concept roll was performed for this established
settings layout, and there is no seed reference.

**Finish:** Native finish review and documentation compare the built app with the incumbent
`docs/design.md` and shared palette in `src/theme.rs`. This ordinary extension preserves that
system; it introduces no separate `DESIGN.md`, palette or shipping raster assets. All nine pages
use the same restrained hierarchy: a page title and explanation, semantic groups on dark
surfaces, separated rows, and natural-width actions. Native buttons, fields, switches and
sliders share contrast and blue keyboard-focus treatments. Long dropdown values ellipsize;
system-information values remain selectable. State changes stay instant. Credential handling,
explicit pairing approval and the independent display revert retain their existing boundaries.

## Validation

The model checks use simulated service operations, real GLib D-Bus variant serialization and a
private Unix socket. `src/settings/check_ui.py` drives the actual GTK UI with simulated devices;
it refuses access to real system D-Bus and audio mutation. Run it only inside a disposable Navi
instance with private XDG directories and Navi-only screenshot instrumentation. Optional
`NAVI_UI_CHECK_RESIZE_HELPER`, `NAVI_UI_CHECK_NAVI_PID` and `NAVI_UI_CHECK_HOST_DISPLAY` supply a
host test-window resizer (arguments: PID, width, height) for the compact-window checks.

Verify native finish across all nine pages at wide and compact widths, no-match and task search,
unavailable services, long device names, keyboard focus, credential and pairing prompts, and
display preview/revert. Fixture captures establish layout and interaction evidence; they do not
establish real-device behavior. Browser-only visual detectors do not apply to this GTK app.

Captured device examples are simulated. Separate read-only queries can validate real service
availability without connecting, pairing or adjusting the host. Real new Wi-Fi connection,
physical Bluetooth pairing, audio changes and KMS mode changes remain supervised hardware checks.
