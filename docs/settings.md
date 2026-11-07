# Navi Settings

Open **Navi Settings** in the launcher's Settings folder. It is a separate, on-demand GTK 4
application, bundled into the `navi-settings` executable. Python 3, PyGObject and GTK 4 are runtime
requirements only for this application. Closing it leaves no settings daemon.

## Available controls

- **Displays:** drag the monitor diagram or edit numeric positions; scale, primary output,
  resolution and refresh rate. Full hardware sessions also support rotation, disabling outputs,
  mirroring an independent display with matching resolution/scale/orientation, and advertised VRR.
  Preview changes, then Keep or Revert within 20 seconds. The compositor owns the timeout,
  restores failed transactions, and falls back to an enabled safe layout on topology changes.
  Nested previews preserve the host's mode and are never saved. HDR and ICC-managed output are
  explicitly unavailable; they need a color-managed compositor renderer.
- **Appearance and wallpaper:** live dark/light palettes, five accents, installed icon/cursor
  themes, cursor/text size, high contrast, dock edge-following and per-monitor PNG/JPEG wallpaper.
  Settings and shell update live. Newly launched GTK apps receive Adwaita light/dark and the
  cursor settings; applications with their own theme preferences may differ. The compact dock
  retains its existing geometry. A missing or unreadable cursor keeps a visible fallback.
- **Notifications:** global enable, Do Not Disturb, per-app blocking, up to 50 session-only
  history entries, explicit history clearing, and sender-directed actions while the notification
  is live. Banners stay hidden while locked. History/actions refuse access while
  locked. Blocked apps do not enter history. App labels/desktop-entry hints are policy labels,
  not authenticated application identities. Rate limits and bounded plain text still apply.
- **UnifiedPush:** disabled by default; explicit provider/server configuration for ntfy or Mozilla
  Autopush with HTTPS URLs, current connection status, registration listing and confirmed removal.
  Optional `kunifiedpush-distributor` runs as an owned child only in a full Navi session; viewing
  Settings never starts it. Existing distributors are left alone. Private provider state and
  registrations survive disable/enable. Provider changes use the native management API and can
  change app endpoints. Message bytes go to participating applications, which decrypt/interpret
  them and decide whether to issue a desktop notification. See [protocol and verification](unifiedpush.md).
- **System Sounds:** off by default; installed sound themes, independent event volume/mute,
  event selection and notification preview. Events include notifications, session warnings, Settings volume changes,
  low battery and session start/end. Uses optional `canberra-gtk-play`; missing sound tools/theme
  files leave the desktop usable. Do Not Disturb suppresses notification sounds.
- **Wi-Fi/network:** NetworkManager radio, scanning, saved/open/WPA Personal/WPA3/hidden networks,
  PEAP/MSCHAPv2 enterprise setup with mandatory authentication-server domain and verified system
  or selected CA trust, Ethernet and saved profile removal. Advanced saved-profile controls edit
  IPv4 addresses/prefix, gateway, DNS and HTTPS PAC proxy settings. Unrelated typed settings,
  IPv6 and stored credentials are preserved; if credentials cannot be obtained for preservation,
  the update is refused. Reconnect to apply profile edits. OpenVPN/WireGuard profile import and
  existing VPN activation use NetworkManager; matching VPN plugins are optional prerequisites.
  Full provider-specific VPN editors, IPv6 editing, other enterprise EAP methods and WEP setup
  remain outside this version.
- **Bluetooth:** native BlueZ power, bounded discovery, PIN/passkey/confirmation pairing,
  connect/disconnect, forget, trust and device battery. Pairing verifies BlueZ's unique owner and
  the requested device. Discovery stops on leaving, closing or after 20 seconds.
- **Sound:** default output/input devices, 0–100% volume/mute and per-app playback volume/mute
  through PipeWire-Pulse/PulseAudio and `pactl`.
- **Power & Locking:** idle locking, Lock Screen, screen-off and sleep timeouts, lid action,
  available power profiles and backlight brightness. Timeouts default off. Screen-off locks
  before powering down; automatic suspend requires confirmed protected presentation and never
  overrides system inhibitors or asks for unattended authorization. Full sessions acquire a lid
  inhibitor so Navi can apply its configured policy; system policy may prevail if unavailable.
  Timers count input only; application idle inhibitors are not implemented, so leave automatic
  timers disabled for unattended playback that must stay awake.
  Hardware controls explicitly affect the computer; nested Navi cannot enable automatic actions.
- **Mouse & Keyboard:** pointer speed, natural scroll, tap-to-click, left-handed buttons,
  flat/adaptive acceleration, disable-while-typing, repeat rate/delay, XKB layout/variant/options,
  and swapping Alt/Super for move/expand shortcuts. Optional three-finger swipes move a focused
  tile one cell through free space. Device controls need a full session.
- **Accessibility:** larger text/cursor, high contrast, prominent visual notification borders,
  and one-shot sticky modifiers. Tapped Shift/Ctrl/Alt/Super applies to the next key; tapping it
  again cancels. Sticky state is cleared before locking and when preferences change. Animations
  stay disabled. Native Settings labels remain available to assistive technology; shell screen
  readers and compositor magnification remain unimplemented.
- **Printers:** CUPS queues/default printer, enable/pause/remove, jobs/cancel, and adding
  driverless IPP/IPPS printers. Requires CUPS client tools and service permissions.
- **Accounts:** AccountsService listing, display names, administrator/locked status, account
  creation and removal while preserving home files. Current/system accounts and the last
  unlocked administrator are protected. Password changes open the system `passwd` tool in a
  supported terminal, with `pkexec` when another account is selected; Navi receives no login
  passwords. New accounts start locked. SCIM provisioning and browser SSO remain future work.
- **Date, Time & Language:** native timedate1/localed time zone, NTP, installed system locale and
  manual timezone-aware date/time entry. These are explicit system-wide operations; authorization
  may be requested. Additional locales must be installed through the distribution.
- **Defaults/About:** native GIO associations and real system information with a copyable summary.

Navi starts an available installed Polkit authentication agent in full sessions and stops only
its own child at logout. Native service authorization still applies; this is not a privilege bypass.
No package, device or system setting changes occur merely by browsing a page. A failed multi-field
system-service operation may leave earlier accepted fields applied; inspect the resulting values
before retrying. Settings is an on-demand app, with no GTK daemon after closing it.

## Configuration and protocol

`~/.config/navi/preferences` is bounded JSON for implemented locking/input preferences. Display
positions retain `navi-monitors 1` and accept `mode`, `rotation`, `disabled`, `mirror` and `vrr`
lines. `~/.config/navi/desktop.json` stores the bounded appearance, notification, sound, input and
power policy. Unknown/malformed data is preserved and saving is refused until repaired.
The optional push child has private XDG roots under `navi/unifiedpush`; its QSettings file
contains subscription capabilities and is retained, not regenerated, when the child restarts. Disconnected displays' entries remain when connected displays are edited. Malformed
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
network credentials never enter process arguments or application logs. New remembered Wi-Fi passwords are staged in oo7 through owner-bound libsecret; NetworkManager receives agent-owned flags without password properties. A native NetworkManager applet agent retrieves them on reconnect. Existing profiles retain their storage; temporary connections remain unsaved. See [secrets setup and verification](secrets.md).

Shell history/actions use the per-instance `io.github.Yoshiofthewire.Navi.Shell.d<display-hex>`
D-Bus name on a connection separate from `org.freedesktop.Notifications`. A sandbox granted
only notification access cannot reach Settings through the notification name or its unique
owner. Trusted Settings remains available when another notification server owns the standard
name. Shell requests retain the compositor's independent lock check; this isolation does not
restrict trusted applications with unrestricted access to the user's session bus.

## Direction contract

**Mode / thesis:** Operate. A familiar searchable category list leads to a focused, directly
usable settings page. Status and recovery stay visible while changes are in progress.

**Own-world:** Preserve Navi's default near-black background, dark shell surface, light text and
blue selection/action accent from `src/theme.rs`; use native GTK controls and Noto Sans. The
optional light scheme, five accent choices and high contrast retain the same hierarchy and
geometry. Settings maps shared palette roles to GTK control states in `app.py` and `extra_ui.py`;
`docs/design.md` remains the established visual authority.

**Story:** Find a task, inspect real current values, edit, apply, and see success or a useful
failure. Device pages explain that system changes affect the computer, including from a nested session.

**First viewport:** A 230px searchable category column beside a scrolling page column capped
at approximately 820px, with current connection status above the page. Below 760px, a single
line of native search/category controls replaces the sidebar. Rows stack labels above wider
controls when the content area is narrower than 540px; switches and single actions stay beside
their labels. Wide scalar forms align controls after a shared 200px label column. Displays opens
by default, with each output's values and an explicit preview action. After applying, a fixed
footer on Displays holds the countdown, Revert and Keep Settings outside the scrolling form, so
recovery stays visible while scrolling at wide and compact widths.

**Form:** Conventional native settings sidebar and forms, selected from the already approved
settings task. Code-first, established Navi visual world; no concept roll was performed for this established
settings layout, and there is no seed reference. User-supplied KDE About, Bluetooth, Sound,
Mouse and Wallpaper screenshots serve as comparisons for hierarchy, density and task placement.
Navi retains its own palette, native GTK controls and implemented capabilities.

**Finish:** Native finish review and documentation compare the built app with the incumbent
`docs/design.md` and shared palette in `src/theme.rs`. This ordinary extension preserves that
system; it introduces no separate `DESIGN.md`, design system or shipping raster assets. All
Settings pages share a page title and explanation, consistent spacing and natural-width actions.
Settings and fact rows use semantic groups on scheme-appropriate surfaces with subtle separators.
Native buttons, fields, switches and sliders share contrast and selected-accent keyboard-focus
treatments. Long dropdown values ellipsize;
system-information values remain selectable. State changes stay instant. Credential handling,
explicit pairing approval and the independent display revert retain their existing boundaries.

Sound places volume and an explicitly labeled Muted control together under each device selector;
Bluetooth keeps scanning beside its adapter radio. About leads with Navi's version and session,
then Software and Hardware facts; Copy System Information sits beside the heading and stacks
below it at narrow widths. Wallpaper identifies each target with a display schematic containing
its actual connector and dimensions, alongside Choose Image and format/crop guidance. The
schematic represents the display; image selection continues through the native file picker.
Long arrangement-diagram labels ellipsize inside their monitor outlines, including narrow
portrait shapes; complete output names remain in the form headings.

## Validation

The model checks use simulated service operations, real GLib D-Bus variant serialization and a
private Unix socket. `src/settings/check_ui.py` drives the actual GTK UI with simulated devices;
it refuses access to real system D-Bus and audio mutation. Run it only inside a disposable Navi
instance with private XDG directories and Navi-only screenshot instrumentation. Optional
`NAVI_UI_CHECK_RESIZE_HELPER`, `NAVI_UI_CHECK_NAVI_PID` and `NAVI_UI_CHECK_HOST_DISPLAY` supply a
host test-window resizer (arguments: PID, width, height) for the compact-window checks.

Verify native finish across all 16 pages at wide and compact widths, no-match and task search,
unavailable services, long device names, keyboard focus, credential and pairing prompts, and
display preview/revert. Check that the countdown and recovery actions remain in bounds while
scrolling the form at compact widths, and that compact no-match search shows one message.
Compare the supplied KDE examples for task hierarchy and density, including complete Output
and Input controls in the first 1280 × 800 Sound viewport. Verify contextual accessible names
through AT-SPI for repeated device, volume, mute and dialog controls as well as compact navigation.
Fixture captures establish layout and interaction evidence; they do not establish real-device
behavior. Browser-only visual detectors do not apply to this GTK app.

Captured device examples are simulated. Separate read-only queries can validate real service
availability without connecting, pairing or adjusting the host. Real new Wi-Fi connection,
physical Bluetooth pairing, audio changes and KMS mode changes remain supervised hardware checks.


## Expanded verification

CI explicitly runs the headless notification isolation regression in debug and release:

```sh
cargo test --locked dbus::tests::notification_only_proxy_isolation -- --ignored
cargo test --release --locked dbus::tests::notification_only_proxy_isolation -- --ignored
```

It requires `dbus-run-session`, `xdg-dbus-proxy`, and Python GI. It uses the real D-Bus exports,
a private bus, and a fixture compositor lock response. Notification-only clients must fail
history/action calls via both well-known and unique names, while notifications and trusted
Settings actions still work. Locked requests must still be denied.

Run the existing full Rust checks and Python tests. Native fixture verification covers all 16
pages at wide/compact sizes, light/dark, large text, keyboard focus, service failures and timed
recovery. Fixtures never contact the host system bus or mutate audio/devices. Run these optional
integration checks from a checkout with built binaries:

```sh
python3 scripts/check-unifiedpush.py --output /tmp/navi-push-check
CARGO_TARGET_DIR=/home/yoshi/git/navi/target python3 scripts/check-shell-settings.py --output /tmp/navi-shell-check
```

The first uses private D-Bus and loopback-only traffic to verify the installed distributor's
receipt, registration persistence, provider changes and unregister. Exit 77 means an optional
prerequisite is missing. The second needs a host X11/XWayland display and launches only its own
nested Navi, repeating the notification-only proxy isolation check against the real shell,
notification actions/DND/blocking/history, nested push refusal,
malformed-config preservation, and the packaged Settings entrypoint. Neither proves real
hardware mode/VRR/DPMS/suspend, public-provider TLS/delivery, printer/device operations, local
account administration or locale changes. Those remain supervised full-session checks.
