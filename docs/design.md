# navi design

navi is a tiling Wayland desktop for people with several big monitors who don't want to learn
Hyprland. Tiling is the product, not a mode. The mouse is enough; the keyboard is optional.

Status: implemented prototype. [Readme.md](../Readme.md) and [the session guide](session.md)
describe current capabilities and validation limits; this document also records planned behavior.

## Principles

These come from hands-on tests of COSMIC and niri. Each one names the thing that felt wrong.

1. **The space fits the app.** An app gets the size it needs; the layout arranges itself around
   that. Empty space is allowed. (COSMIC stretches every app to fill whatever slot it's given.)
2. **Windows never change orientation.** A window on the ultrawide's left stays on the left; it
   is never moved to the top or bottom because a split tree rebalanced itself.
3. **Tabs are the last resort.** Two windows share a tile only when there is no room anywhere
   else.
4. **Resizing is predictable.** Dragging an edge moves that edge, snaps to the grid, and changes
   only the windows that touch it.
5. **Tiling is not a toggle.** There's no "tiling mode" to turn on. Dialogs and popups float;
   everything else tiles.
6. **Every window stays on screen.** A window never scrolls or slides off its monitor. (niri
   puts windows on an endless strip that runs off the edge.)

## Layout model

Each monitor is a **12 × 6 grid** in logical pixels. A window covers a rectangle of whole cells.
Cells may be empty. There is no split tree, so nothing can flip orientation (principle 2).

12 divides by 2, 3, 4 and 6, so common splits are exact:

| Monitor | Cell | Example |
|---|---|---|
| 3440×1440 ultrawide | 286×240 | T3 8 cols + browser 4 cols; 3-wide (4 cols each) or 4-wide (3 cols each) |
| 3840×2160 4K | 320×360 | 3×3 of 4×2-cell tiles; a video in the top 2×2 = 2560×1440 |
| 1800×1125 @1.6 laptop | 94×117 | halves, thirds, quarters |

Cell sizes are rounded down to whole pixels; the last row and column take the leftover. Tiles have a
small gap around them. The gap at the top of each tile is 6–8 px, and only the compositor draws
there (see *Grab strip*).

### Remembered size

navi remembers each app's last size **in pixels**, along with the monitor it was on. The key is
the app ID, so two terminals share one memory. On any monitor, the size snaps to the nearest cell
rectangle. A terminal stays about the same physical size everywhere and never inflates to half a
4K screen (principle 1).

Memory survives restarts. navi keeps it in `$XDG_STATE_HOME/navi/memory` (default
`~/.local/state/navi/memory`), readable only by the user. It holds at most the 512 most recently
used apps, and navi ignores a damaged file.

If navi has no memory for an app, it gets half the monitor's width at full height. Wayland gives
an app no way to ask for a size before it's first placed.

### Placing a new window

These steps are tried in order. The first one that fits wins.

1. **Where the app was last time**, if those cells are empty on that monitor.
2. **The nearest empty rectangle** of the remembered size on the monitor with the pointer.
3. **Open smaller.** The nearest empty rectangle at less than the remembered size, but no
   smaller than the minimum (about 320×240 logical px, rounded up to whole cells).
4. **Shrink the neighbours.** The new window's target width is an equal share of the monitor's
   columns (12 ÷ the number of tiles in that row, counting the new one), or its remembered width
   if that is smaller, and never below the minimum. Tiles give back one column at a time, largest
   first, until the target fits. Tiles larger than their remembered size go first. Tiles may
   slide sideways, keeping their order, so the freed columns form one gap. No tile shrinks below
   the minimum. Shrinking never changes what navi remembers for an app. On the ultrawide, two
   half-width apps plus a third become 4 + 4 + 4 columns, and a fourth makes 3 + 3 + 3 + 3.
5. **Another monitor** that has room, by monitor priority (see *Monitors*).
6. **Tab with the nearest tile.** This happens only when every tile is already at its minimum
   (principle 3).

When a window closes, its cells stay empty. Other windows don't jump to fill them. You use the
space when you want it.

### Dragging

- **Move:** hover near a tile's top edge and a thin strip lights up. Left-drag it. The grid
  appears faintly, and one glowing preview shows where the window will land. The preview is the
  window's remembered size, snapped under the pointer, and it's clamped to stay on screen. Drop
  the window onto another tile to swap them. Hold the window over the middle of a tile for about
  half a second, and the drop forms tabs instead.
- **Resize:** drag any tile edge. It moves in whole cells. When two tiles share that edge, both
  change. Nothing else changes (principle 4). The edge stops when a tile reaches its minimum. The
  new size becomes that app's remembered size.
- **Diagonal resize:** drag a corner in the gaps around a window to change its width and height
  together. The 32 px nearest each corner of an edge is the corner target; it never takes pixels
  from the app, tabs or window buttons. At a four-window junction, both shared boundaries move.
- **Fill toward an edge:** double-click an edge (two clicks within 400 ms, moving less than 8 px)
  to extend that side to the monitor's usable edge. The opposite side stays put. It uses the same
  grid, neighbor and minimum-size rules as dragging, so another tile can stop it short. Panels
  keep their space. This is a resize and is remembered, rather than a maximize toggle.
- The strip is the 8 px gap above a window; the edges are the 4 px gaps beside and below it.
- A click on the strip without moving (under 8 px) moves nothing.
- Once a hold will form tabs, the preview turns more solid, so swap and tab look different.
- Dragging a tab moves only that tab; resizing a tab group's edge resizes the group.
- The "middle" of a tile is the inner half of it, in both directions.
- The top edge of a tile on the monitor's top border isn't draggable; the strip owns that gap.
- Drop a window back on itself to cancel a move. A drag also cancels, putting everything back,
  when the pointer leaves every monitor or the nested window loses focus.

### Keyboard window controls

- **Alt + Arrow** moves the focused tile one grid cell in that direction, preserving its size.
  It stops at another tile or the current monitor's usable edge.
- **Super + Arrow** expands only that side through free cells until another tile or the
  monitor's usable edge. It keeps the opposite side and neighboring windows fixed.
- Both act on the active tab group, keep keyboard focus, reserve minimized windows' cells,
  and remember successful changes. Moves preserve remembered size; expansion updates it.
- Maximized/fullscreen windows keep their mode and geometry. Floating dialogs and shell
  surfaces keep their app shortcuts; window shortcuts apply only to a focused tile. A mouse
  drag takes precedence. Adding Ctrl, Shift, or the other modifier leaves the chord to apps.

### Tabs

- A tab group shows one window at a time. Above it, a 24 px bar replaces the strip and holds one
  tab per window, left to right in the order they opened. The active tab is accent blue, inactive
  tabs are outlined, and minimized ones are a dimmed blue. Tabs have no titles or icons yet; those
  come later, since titles would need fonts in the compositor and fonts stay out of it. For now a
  tab tells you its window only by position.
- Click a tab to switch to it. Drag a tab out (8 px or more) to untab that window: it lands in the
  nearest free space at its remembered size, or tabs into another tile after a half-second hold.
  Dropping it on another tile without the hold, or back on its own group, cancels.

### Window controls

- Apps that draw their own title bar (Konsole, Firefox) keep it. Apps that ask for server-side
  decorations get a 24 px bar with **minimize, maximize and close** buttons on the right; the rest
  of the bar is the grab strip. Close asks the app to close; navi never kills it.
- **Maximize** covers the monitor's whole grid until clicked again; it isn't fullscreen. The
  window's cells and remembered size don't change, and the other windows on that monitor hide
  until it's undone. A new window on that monitor undoes it, so a new window is always visible.
  Clicking a maximized app's strip also undoes it, so navi can always undo a maximize even when
  the app has no button for it.
- **Minimize** hides the window but keeps its cells, so nothing moves into them. Its strip (or
  bar) stays, drawn in the same dimmed blue as a minimized tab; click it to bring the window
  back. A minimized tab stays in the bar, dimmed, and clicking it brings it back.
- An app's own maximize and minimize buttons do exactly what navi's do. Hidden windows can't
  maximize or minimize, so a window nobody can see can't hide the ones they can.
- Dialogs stay floating over their parent, hide while it's hidden, and re-centre when it moves.
- **Fullscreen** (the app asks for it) covers the whole monitor, taskbar included, on the
  monitor the window is on, and hides the other windows there; the taskbar can't be seen or
  clicked meanwhile, but keeps its space. It ends when the app leaves fullscreen, closes or is
  minimized, when a new window opens on that monitor, when another window there is
  activated, or when the user presses Super+Esc on that monitor (the focused window's, else
  the pointer's); the window goes back to its tile. Super+Esc is navi's: apps never see it,
  whether or not anything is fullscreen. A window left by Super+Esc can't go fullscreen again
  until the user types a non-modifier key that is no navi shortcut into it, or clicks it.
  Its dialogs still float over it.

## Monitors

- **Identity is the EDID** (make, model, serial), not the port name. `DP-11` changes from one
  dock to the next. Everything navi remembers is tied to the EDID. The identity is KWin's
  `edidIdentifier` with `_` for spaces, from the EDID's base block:
  `<PNP id>_<product>_<serial>_<week>_<year>_<model year>`, e.g. `ACR_1359_4294967295_0_0_2016`.
  A monitor with no valid EDID is known by its connector name (`DP-3`), with a warning, and two
  identical monitors get `_2`, `_3`… in connector-name order (never one a connected monitor
  holds). Memory, wallpapers, the arrangement and the taskbar key by it, so they follow the
  monitor. It stays inside navi: it carries the monitor's serial and manufacture date, which
  would let any app fingerprint the machine, so clients see the connector as the output's name
  (`DP-11`) and Smithay's `<make> - <model> - <connector>` description, model being the EDID's
  monitor name or product code.
- **Arrangement:** `~/.config/navi/monitors`, a `navi-monitors 1` line, then
  `<identity> <x> <y> <scale>` per monitor (logical px within ±65535, scale 0.5–4.0) and an
  optional `main <identity>`. Any bad line makes navi ignore the whole file, with one warning,
  and it is never rewritten while it's bad. A monitor not in the file, or one overlapping a
  monitor listed before it, goes right of everything, top-aligned with the main monitor, at
  scale 1, and is saved there. The first start on real hardware, with no file yet, imports
  KWin's `kwinoutputconfig.json`: the setup whose enabled outputs are exactly the connected
  monitors, lid open preferred. With no such setup, monitors go in one row at scale 1, the laptop
  panel leftmost. Later KWin changes aren't followed.
- **The main monitor** is the one in the middle of the layout: the one bordering the most other
  monitors (sharing an edge of positive length; corners don't count), with ties broken by size,
  then connector name. The `main` line overrides this. The laptop panel is never main while
  another monitor is connected.
- **Priority:** the main monitor first, then the others by size. The built-in laptop panel
  (`eDP`, `LVDS`, `DSI`) comes last whenever anything else is connected.
- **The pointer** never sits between monitors: it stops at the nearest edge of the monitors it
  can reach. It starts in the middle of the main monitor.
- **Unplugging** moves that monitor's windows to the remaining ones using the placement steps
  above, from the main monitor's centre, a tab group at a time; memory doesn't change. Each
  window remembers its `home` (monitor and cells). **Replugging** returns each window to its home
  cells, if they're free, as long as you haven't moved it since: a drag move, swap, tab, untab or
  edge resize forgets its home.

## Shell

The screen shows a wallpaper, a taskbar and your apps. There are no desktop icons and no widgets.
Desktop icons have no enablement option. Files in the user's Desktop directory are accessible
through the file manager, like any other folder.

Utility selection follows the [desktop application policy](../PRODUCT.md#desktop-application-policy):
non-terminal Navi utilities must match Navi's selected appearance. Navi uses the user's browser;
bundling a degoogled, resource-tested Chromium variant is reserved for a future distribution.

- **Wallpaper:** set per monitor, independent of the taskbar, in `~/.config/navi/wallpaper`. The
  first run imports the image Plasma shows on each screen. PNG or JPEG only, scaled and cropped to
  fill the monitor, and capped at 8192 px a side and 8K's pixel count. Otherwise, or if the image
  can't be read, the monitor is solid `#0B0D12`. The file is a `navi-wallpaper 1` line, then one
  `<monitor> <absolute path>` line per monitor, where monitor 0 is the main one and the rest
  follow by priority, e.g. `0 /home/me/Pictures/city.jpg`. Edits apply on restart. Delete the
  file to import from Plasma again.
- **Taskbar:** a compact, rounded dock centered 8 logical px above the bottom of its monitor.
  Its visible body is 40 px high, with 24 px icons in 36 px slots, a subtle outline, and a blue
  active-window indicator. The logo opens the launcher. Pinned apps come first, in pin order;
  other running apps follow in opening order. Each app has one icon, with up to three dots for
  its windows. Click an app to launch it if closed, focus or restore it if running, or cycle its
  windows when already focused. Minimized apps are dimmed.
- **Pins:** use the **Pin / Unpin** button beside an app in the launcher, press Insert on a
  selected app, or right-click an app's dock icon. Pins survive restarts in
  `$XDG_CONFIG_HOME/navi/pins` (default `~/.config/navi/pins`): `navi-pins 1`, then one installed
  desktop file ID per line, at most 32. Uninstalled pins are retained in the file and hidden until
  the app is installed again. Saves are private and atomic; a damaged file is left intact and
  reports an error when pinning is attempted.
- **Taskbar monitor:** hover the bottom edge of another monitor (its bottom 2 logical px) for
  300 ms, and the dock moves there and stays. Its monitor is remembered in
  `~/.local/state/navi/bar`; if unavailable, it uses the main monitor. Fullscreen hides it.
- **App menu:** opens to folders derived from the installed apps' Desktop Entry categories:
  Internet, Development, Office, Graphics, Sound & Video, Games, Education, Science, Settings,
  System, Utilities, and Other. Only nonempty folders appear; apps are alphabetized within each
  and appear in one folder. Click a folder to browse it; Back, Escape, Left, or Backspace with
  an empty search returns to folders. Typing searches all folders by app name or desktop ID.
  Up/Down selects, PageUp/PageDown move a page of rows, Home/End go to the first and last row,
  Enter opens the selected folder or launches the app, and Escape at the root or clicking
  elsewhere closes the launcher. Tapping Super on its own (no other key or button pressed before
  its release) opens or closes it on the taskbar's monitor; any Super chord never does, and apps
  still see the Super press and release. While open the launcher holds the keyboard; when it
  closes, the keyboard returns to the window that had it, if that's still shown. A tap does
  nothing while locked or while the taskbar's monitor shows a fullscreen window (Super+Esc
  first), and a window going fullscreen there closes it and returns the keyboard. With sticky
  keys on, a lone Super tap stays a latch. The mouse wheel and two-finger touchpad gestures
  scroll both folders and app lists; small continuous movements accumulate across frames, and
  gesture end, pointer leave or navigation clears any remainder. Short result lists shrink the
  launcher to fit. The search, rows and pin buttons have distinct targets, and transparent space
  around the dock and launcher takes no clicks.
  Folder rows use KDE's per-category icons (`applications-internet-symbolic`,
  `applications-development-symbolic`, `preferences-system-symbolic`, and the other matching
  category names), falling back to their non-symbolic name and then a generic folder.
- **Corners:** on the main monitor, and they stay there when the taskbar moves or the main
  monitor changes. A 32 px status
  bar runs across the top, with the same opaque dark surface and outline as the dock. Navi's logo
  and name align left, the local date and 24-hour clock sit in the center, and a battery glyph
  and percentage align right, labeled while charging. Battery capacity is the mean of the
  system's batteries, read every 30 s; mice and headsets don't count. The bar takes space from
  the grid and hides in fullscreen. Only the Navi button and its open session menu take clicks;
  the rest of the band is transparent to input.
  Notifications are top-left, below the band. Volume, network and tray come later. Volume has no
  small route: PipeWire's native API is C (`libpipewire`, which needs `unsafe` and a new system
  library), pipewire-pulse has no D-Bus volume interface, and running `wpctl` would break the
  no-shell rule.
- **Session menu:** click Navi at the upper left to open Sleep, Lock Screen, Switch User,
  Log Out, Restart, and Shutdown. Up/Down or Tab selects, Enter opens a confirmation with Cancel
  selected, and Escape cancels or closes. Clicking elsewhere closes the menu. All actions need
  explicit confirmation; holding Enter never accepts one. Errors appear in the menu and are
  logged. System services are queried off the UI thread with bounded method timeouts.
  Sleep/Restart/Shutdown use login1 and its normal permissions/inhibitors. Full sessions start
  an available installed Polkit authentication agent; actions still need service authorization.
  Switch User uses the display manager's `SwitchToGreeter` only for `XDG_SEAT_PATH` and only
  when its `CanSwitch` is true. Switching user and sleeping first wait for Navi to confirm
  protected presentation on every output. Lock Screen and Super+L use a privately connected swaylock, with PAM authentication.
  Nested, Lock Screen protects only Navi’s window and Log Out closes just Navi. A confirmed
  logout exits navi-bar successfully; Navi treats
  that private child exit as a graceful quit, while failed/crashed bars still respawn.
- **Notifications:** navi-bar serves `org.freedesktop.Notifications` 1.2 only when no other
  server owns the name. It never replaces or queues behind Plasma. Up to three bounded,
  plain-text cards use the existing 1–10 second lifetime and per-sender/global rate limits.
  Images, icon paths and sound hints remain ignored. Live actions emit only to the original
  unique sender; a default action runs on a card click, and other actions are explicit in
  Settings. Replacing/closing is still sender-scoped. Global enable, Do Not Disturb,
  per-app blocking and a 50-entry in-memory history are configurable. Blocked notifications
  never enter history. History/actions require an unlocked compositor settings check.
  Fullscreen and locking hide notification surfaces. Optional UnifiedPush is a separate,
  owned external distributor, disabled by default; payloads reach their participating apps,
  which decide whether to notify. See [Settings](settings.md) and [UnifiedPush](unifiedpush.md).
- **Laptop alone:** the same layout with a slimmer taskbar.
- **No virtual desktops.**
- **Look:** cyberpunk, with no animations: every state change is instant. (The half-second hold
  before a drop tabs is a state change, not an animation.) The accent is the logo's neon blue,
  `#2BA3D8`, on near-black, from one palette (`src/theme.rs`) that navi and navi-bar share.
  This dark/blue baseline remains the default. Appearance offers light mode and blue, green,
  purple, amber or pink accents; Accessibility offers high contrast. These are opt-in variants
  of the same system, preserving geometry, hierarchy and instant state changes. The
  window with the keyboard has a 2 px accent frame just outside it, in the gap; every other shown
  window has a dim 1 px one, so the gaps read as thin neon lines and the grab strips stay easy to
  find. A lit strip or edge, the bars and the minimized strips draw in front of the frames.
- **App menu button:** the navi logo, under the icon name `navi` (in `assets/icons/hicolor`). It's
  installed into `hicolor`, so an icon theme can replace it. A hand-simplified version with
  single lines is used at 16–32 px, where the full logo's double lines blur together.

## Themes

`src/theme.rs` owns the compositor/shell palette and its scheme-dependent accent values.
The default background, shell surface and text are `#0B0D12`, `#151922` and `#D0D4DC`;
light mode uses `#F5F6F8`, white and `#202734`, with a darker blue accent (`#0077AA`).
High contrast uses black/white surfaces and text in the selected scheme. Settings maps these
same roles onto native GTK controls in `app.py` and `extra_ui.py`, with separate control borders
and secondary-text values for legibility. Noto Sans remains the Settings/shell typeface; the
default text size is 14 logical px, adjustable from 12 to 24 without changing the layout model.

navi uses the user's installed themes and ships none of its own. Icons, cursors and sounds
follow freedesktop.org standards, so any theme from the distro's repos or the KDE Store works,
Breeze included. The themes stay on the user's system and are never bundled, so their licences
don't affect navi's MIT licence.

| Asset | Standard | Where navi looks |
|---|---|---|
| Icons | Icon Theme Spec | `~/.local/share/icons`, then `$XDG_DATA_DIRS/icons`, then `hicolor` |
| Cursors | XCursor themes, plus the `cursor-shape-v1` protocol so apps ask for cursors by name | `~/.local/share/icons`, then `~/.icons`, then `$XDG_DATA_DIRS/icons`; inside each, `<theme>/cursors` |
| Sounds | Sound Theme Spec | `~/.local/share/sounds`, then `$XDG_DATA_DIRS/sounds`, then `freedesktop` |
| Boot splash | Plymouth | not navi's job; it runs before login and works with any desktop |

- **Default icon theme:** `candy-icons` if it's installed, otherwise `hicolor`. An icon theme the
  user explicitly chose in KDE wins over this default: navi-bar reads `[Icons] Theme` from
  `$XDG_CONFIG_HOME/kdeglobals` (default `~/.config/kdeglobals`) on each start. The same theme and
  inheritance chain is used for apps, category icons, and session actions.
- **Default cursor theme:** `Sweet-cursors` if it's installed (it has a `cursors` dir in one of
  the places above), otherwise `Adwaita`, otherwise none, and the nested host's pointer stays.
  navi draws the `default` arrow (else `left_ptr`), found in the theme or what it inherits
  (`Inherits=` in `index.theme`, at most 4 levels, no cycles). Its size is the one in the file
  nearest 24 × the monitor's scale, the larger on a tie (Sweet ships 24, 36 and 48), and only its
  first frame is used. Files over 2 MiB, images over 256 px a side and hotspots outside the image
  are refused; then navi logs one warning and keeps the host's pointer. Built now: this arrow
  for every surface, with `XCURSOR_THEME` and `XCURSOR_SIZE` (24 × scale) set for navi-bar and
  the apps it launches. Still to come: apps' own cursors (`cursor-shape-v1`, cursor surfaces),
  importing the cursor chosen in KDE, and animated cursors. Nested, the arrow stays at the edge
  where the pointer left the window, since the winit backend doesn't report leaving.
- **Planned theme imports:** sounds
  from `kdeglobals` `[Sounds]`, cursor from `kcminputrc` `[Mouse]`. This is a one-time copy into
  navi's config, so Plasma users keep the look they chose. Icons already read KDE's explicit
  choice on each startup, as described above.
- **Utility appearance:** Navi Files/Thunar uses the shared palette and native GTK module for
  live colors, accents, text, icons, cursors and window controls. Settings retains its native
  GTK 4 styling. See [utility integration](utilities.md) for scope and verification. Apps also
  receive `XCURSOR_THEME`/`XCURSOR_SIZE`; utility appearance is verified per supported application.
- **No KDE login splash.** KDE's splash themes are QML programs. Running them would pull in Qt,
  and would mean executing code that came from a theme download. navi starts fast enough to need
  no splash.
- **Theme files are untrusted input**, since anyone can upload a theme to the KDE Store. The
  compositor decodes icons and cursors with memory-safe Rust libraries (`resvg`, `xcursor`) and
  caps image size. Optional event sounds are off by default and play through a separate
  `canberra-gtk-play` process, never inside the compositor.

## Security defaults

The compositor sees every key press and every pixel, so it's the most trusted program in the
session.

- **Privileged protocols** are only for navi's own shell, plus anything you explicitly allow:
  screen capture (screencopy and image-copy), clipboard managers (`data-control`), virtual
  keyboard and pointer, input method, and window lists (`foreign-toplevel`). Screenshots and
  screen sharing for other apps go through `xdg-desktop-portal`, which asks first.
- **navi's own shell** is `navi-bar`, the binary next to navi's own, which navi starts itself
  over a socket pair it made. Only that connection sees layer shell and the window list
  (`wlr-foreign-toplevel-management`); other clients don't find them in the registry at all. A
  layer surface gets the keyboard only when clicked (`exclusive` is treated as on-demand), and
  can't shrink the grid below half the monitor. A successful `navi-bar` exit requests logout;
  other exits restart it after 1 s, at most 5 times in any minute; past that Navi runs without a bar.
- **Activation tokens** (`xdg-activation`) are made only for a request from the surface holding
  the keyboard, with a serial from its current focus (no older than its keyboard enter, no newer
  than navi's latest). Each works once, within 10 s, and only while the keyboard is still on the
  surface that asked or on the one being activated, so an app can't stash tokens and take focus
  back after you've moved on.
- **Locking** uses `ext-session-lock`, advertised only on the dedicated socket Navi gives its
  swaylock child. Applications, including the bar, cannot bind it. Normal windows, layers,
  wallpaper, decorations and cursors are excluded from locked output frames; normal keyboard,
  pointer, activation and X11 clipboard paths are isolated. Each output must present protected
  content before `locked` or a menu readiness reply. DRM waits for successful vblank submission;
  nested mode waits for a successful window swap. Output changes invalidate queued evidence.
  A crashed/disconnected locker leaves a solid protected screen, with bounded automatic restart
  and Super+L recovery. Only the active, confirmed lock resource can unlock; rejected and early
  unlock requests are guarded before Smithay dispatch. The distro PAM-enabled swaylock handles
  passwords; Navi never reads them. Its user config is bypassed to prevent daemonization.
  Full sessions listen for logind Lock and PrepareForSleep with a delay inhibitor, and report
  ordered LockedHint changes. A failed before-sleep lock retains protection/inhibitor until
  logind’s own timeout; listener failure is logged. Idle locking, screen-off and automatic sleep are configurable, with protected presentation required before power actions.
  The lock backdrop is the configured per-monitor wallpaper with the desktop's centered crop,
  darkened to 27% brightness and blurred. No Navi logo, name, avatar or clock is displayed.
  Small “Session locked”, password and Enter prompts remain. The circle is eight clockwise
  slices from 12 o’clock: blue to green over characters 1–8, green to purple over 9–16, then
  amber, pink and cyan, repeating. Backspace reverses and clear resets. A rendering-only patch
  reads the existing swaylock input-buffer length in Unicode codepoints, never copies/logs its
  contents, and leaves upstream storage/PAM unchanged. Verification/error/Caps Lock feedback
  remains native. Cargo builds the helper from pinned SHA-256-verified upstream source; only
  build-time Meson/Ninja/Cairo/PAM headers are added. Wallpaper decoding, a bounded 960 px thumbnail
  blur and PNG preparation run on a worker; input and rendering protection begin immediately.
  Three separable box passes approximate a soft Gaussian with linear cost, then the existing
  graphics library scales the backdrop and composites crisp prompt art at output size. Output
  images use the existing image-size limits. No screenshot protocol, additional process beyond
  the one locker, or font parsing in the compositor is added. Missing wallpaper falls back to BG.
  Per-output private 0600 images last until swaylock exits; a newly connected output uses the
  first blurred image until the next lock. The static prompt art is regenerated during
  development with `scripts/render-lock-prompts.sh` and is embedded in the binary.
  See [session validation limits](session.md).
- **No direct network access**, no plugins, and no shell command evaluation in the compositor or shell.
  The optional UnifiedPush child handles network transport only after opt-in in a full session.
  navi-bar, not navi, launches apps, so `.desktop` files, icon themes and fonts (all untrusted)
  are parsed outside the compositor. Launching runs the `Exec=` line broken into arguments (any
  shell syntax rejects the entry), with an XDG activation token, in its own process group and
  without navi-bar's socket. Running each app in its own `app-*.scope` comes later (it needs
  D-Bus).
- **The session bus** reaches navi-bar only through the notification server: one name, one
  object. Its environment export is on real hardware only: at start it sets `WAYLAND_DISPLAY`, `DISPLAY`,
  `XDG_CURRENT_DESKTOP` and `XDG_SESSION_TYPE` (nothing else) through the bus's
  `UpdateActivationEnvironment` and systemd's `SetEnvironment`, so portals and D-Bus-started
  apps reach navi. Nested it never does, so the host's user services keep pointing at the host. Every field a notification carries is untrusted and is never logged;
  only ids, senders and text lengths are. zbus reads a whole message before navi-bar's caps apply,
  so a hostile local app can make navi-bar allocate up to the bus's message limit per call; such
  an app already runs as the user. A D-Bus bug takes the taskbar down with it, since there is one
  privileged client.
- **System bus:** the full-session compositor listens for logind locking/sleep and reports lock
  state off the rendering thread. Session-menu capability queries and confirmed power/switch-user requests
  run in navi-bar, off the UI thread. Nested mode refuses system actions before connecting.
  Navi never forces past inhibitors, runs a system command, or terminates a login1 session.
- **Config** is plain data. A config file can never run a command.
- **Real hardware:** navi gets the GPU and input devices only through the seat session
  (libseat, so logind), never by opening `/dev` itself, and needs no root. On a VT switch the
  session revokes them and navi pauses. **Ctrl+Alt+Backspace** quits unlocked navi (back to the login
  screen) and Ctrl+Alt+F1…F12 switch VTs; navi keeps these keys before any client sees them, so
  no app can swallow the way out. Every startup failure (no session, no GPU, no monitor) exits 1
  with a message; the log is `~/.local/state/navi/navi.log`. EDID is untrusted device data:
  parsed in safe Rust, base block only, at most 32 KiB read, and turned into a single safe
  token.

## Build

- **Rust on Smithay, written fresh, MIT licence.** Both close alternatives were tried nested on
  2026-11-03 and rejected for their layout model, which is the part navi exists to replace:
  - **cosmic-comp:** its split tree fits the app to the space, flips orientation, offers tabs too
    readily, and resizes unpredictably.
  - **niri:** windows scroll off the monitor. In navi, everything on a monitor stays visible.

  With the layout gone, a fork would keep only the plumbing, at the price of GPL-3.0. Read
  cosmic-comp, niri and Smithay's `anvil` example as reference code; don't fork them.
- Go was ruled out: it has no usable compositor library.
- **Screenshots for testing:** `NAVI_SCREENSHOT=<path>` in navi's own environment writes its
  rendered frame there as a PNG (mode 0600), on the first frame and then once a second. Unset, it
  costs nothing. No client or config can turn it on.
- **Input replay for testing (debug builds only):** `NAVI_INPUT_REPLAY=<file>` plays a pointer
  and keyboard script through the same handlers as real input: one step per line, `wait <ms>`,
  `move <x> <y>` (global logical px), `press`, `release` (left button), `key <evdev keycode>`
  (press and release), `down <code>` and `up <code>` (to hold a modifier), `#` comments. A bad line logs one error and nothing is replayed. Release
  builds don't contain it.
- The shell (taskbar, corners, launcher) runs as separate `layer-shell` clients, so a taskbar
  crash can't take your windows down.

- **Backends:** nested (a window in another compositor) when `WAYLAND_DISPLAY` or `DISPLAY` is
  set, else real hardware: DRM/KMS on the seat's primary GPU only, libinput, libseat. Both draw
  each output from the same element list (`src/render.rs`). See `docs/session.md`.

- **XWayland:** Smithay's rootless server and XWM manage X11 windows in the same layout as
  native apps. Embedded NULs in `WM_CLASS` and window titles become U+FFFD; the resulting text
  is capped at 2048 bytes at a UTF-8 boundary before layout or Wayland window-list events use it.
  This keeps complete events within the backend's 4096-byte buffer. The metadata reaches the
  dock. X11 keyboard focus is set as well as Wayland focus. Transient dialogs follow their
  parents and hide with them; override-redirect
  menus keep their X11 geometry and require an X11 parent. Clipboard transfers are bridged,
  with X11 access gated on keyboard focus. Clipboard MIME names containing NULs or exceeding
  2048 bytes are rejected; accepted names are preserved exactly. Primary selection, cross-protocol drag-and-drop,
  and automatic XWayland crash recovery remain unimplemented. Install the `Xwayland` executable
  to enable it; native apps still work without it. The headless integration check is
  `cargo test --locked xwayland::tests::x11_lifecycle_and_input -- --ignored`.

## Open questions

- Grid size: hard-coded at 12 × 6, or configurable? Leaning hard-coded.
- More look references, beyond the logo.
- Is this a daily driver? That decides whether laptop polish is in v1.
- Multi-monitor test of COSMIC in a real session, after the running T3 tasks finish.
- The dock's window slots need a pointer: there's no keyboard access to them yet, and the bar
  exposes nothing to screen readers.

## Settings application

The on-demand native GTK 4 Settings app inherits Navi's palette and uses a searchable category
sidebar, scrolling forms, native controls and explicit status/recovery. GTK/Python remain outside
the compositor. Displays support live arrangement/scaling, advertised KMS modes, primary output
and an independent 20-second revert; wallpaper updates live. Idle locking and libinput/repeat
preferences are persisted. The expanded categories include appearance, notification history/DND,
optional UnifiedPush, event sounds, accessibility, printers, local accounts and regional settings.
Networking and Bluetooth use native NetworkManager/BlueZ
controls, while audio uses native pactl operations; defaults use GIO and About reports real system information. See [Settings](settings.md)
for scope, protocol boundaries, unavailable controls and the surface direction contract.

### Secret storage

Application secrets use the external oo7 service through standard libsecret. Full-session
helpers and newly remembered Wi-Fi storage follow [Secrets](secrets.md); credentials never
enter compositor settings/JSON, argv or logs. The Wi-Fi dialog distinguishes remembering a
network from an unsaved connection and states that saved passwords use oo7. Existing
providers and saved network profiles are not migrated by Navi.
