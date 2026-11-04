# navi design

navi is a tiling Wayland desktop for people with several big monitors who don't want to learn
Hyprland. Tiling is the product, not a mode. The mouse is enough; the keyboard is optional.

Status: draft, 2026-11-03. Nothing is built yet.

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
- The strip is the 8 px gap above a window; the edges are the 4 px gaps beside and below it.
- A click on the strip without moving (under 8 px) moves nothing.
- Once a hold will form tabs, the preview turns more solid, so swap and tab look different.
- Dragging a tab moves only that tab; resizing a tab group's edge resizes the group.
- The "middle" of a tile is the inner half of it, in both directions.
- The top edge of a tile on the monitor's top border isn't draggable; the strip owns that gap.
- Drop a window back on itself to cancel a move. A drag also cancels, putting everything back,
  when the pointer leaves every monitor or the nested window loses focus.

### Tabs

- A tab group shows one window at a time. Above it, a 24 px bar replaces the strip and holds one
  tab per window, left to right in the order they opened. The active tab is accent blue, inactive
  tabs are outlined, and minimized ones are a dimmed blue. Tabs have no titles or icons yet; those
  come with the Look job, so for now a tab tells you its window only by position.
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

## Monitors

- **Identity is the EDID** (make, model, serial), not the port name. `DP-11` changes from one
  dock to the next. Everything navi remembers is tied to the EDID.
- **The main monitor** is the one in the middle of the layout: the one bordering the most other
  monitors, with ties broken by size. You can override this, and the override is remembered.
- **Priority:** the main monitor first, then the others by size. The built-in laptop panel comes
  last whenever anything else is connected.
- **Unplugging** moves that monitor's windows to the remaining ones using the placement steps
  above. **Replugging** returns each window to its remembered cells on that monitor, as long as
  you haven't moved it since.

## Shell

The screen shows a wallpaper, a taskbar and your apps. There are no desktop icons and no widgets.

- **Wallpaper:** set per monitor, independent of the taskbar, in `~/.config/navi/wallpaper`. The
  first run imports the image Plasma shows on each screen. PNG or JPEG only, scaled and cropped to
  fill the monitor, and capped at 8192 px a side and 8K's pixel count. Otherwise, or if the image
  can't be read, the monitor is solid `#0B0D12`.
- **Taskbar:** centred at the bottom of the main monitor. It holds the app launcher and the
  running apps. Hover the bottom edge of another monitor for about 300 ms, and the taskbar moves
  there and stays. Where it last was is remembered. A fullscreen app hides it.
- **Corners:** on the main monitor. Notifications are top-left; status (clock, network, volume,
  battery, tray) is top-right. The corners stay where they are when the taskbar moves.
- **Laptop alone:** the same layout with a slimmer taskbar.
- **No virtual desktops.**
- **Look:** cyberpunk, with minimal animation. The accent is the logo's neon blue, `#2BA3D8`, on
  near-black. It's used in the gap and the grab strip.
- **App menu button:** the navi logo, under the icon name `navi` (in `assets/icons/hicolor`). It's
  installed into `hicolor`, so an icon theme can replace it. A hand-simplified version with
  single lines is used at 16–32 px, where the full logo's double lines blur together.

## Themes

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
  user explicitly chose in KDE wins over this default.
- **Default cursor theme:** `Sweet-cursors` if it's installed, otherwise `Adwaita`. A cursor
  theme the user explicitly chose in KDE wins over this default. The cursor's pixel size is
  24 × the monitor's scale, rounded to the nearest size the theme provides (Sweet ships 24, 36
  and 48). If the theme lacks a shape, navi uses the theme's `default` arrow.
- **First run imports the KDE settings** if they exist: icons from `kdeglobals` `[Icons]`, sounds
  from `kdeglobals` `[Sounds]`, cursor from `kcminputrc` `[Mouse]`. This is a one-time copy into
  navi's config, so Plasma users keep the look they chose. After that, navi doesn't follow
  changes made in Plasma.
- **Other apps match:** navi passes the chosen themes to apps through `XCURSOR_THEME`,
  `XCURSOR_SIZE` and the portal's appearance settings.
- **No KDE login splash.** KDE's splash themes are QML programs. Running them would pull in Qt,
  and would mean executing code that came from a theme download. navi starts fast enough to need
  no splash.
- **Theme files are untrusted input**, since anyone can upload a theme to the KDE Store. The
  compositor decodes icons and cursors with memory-safe Rust libraries (`resvg`, `xcursor`) and
  caps image size. Sounds are decoded and played in a separate process (`pw-play`), never inside
  the compositor.

## Security defaults

The compositor sees every key press and every pixel, so it's the most trusted program in the
session.

- **Privileged protocols** are only for navi's own shell, plus anything you explicitly allow:
  screen capture (screencopy and image-copy), clipboard managers (`data-control`), virtual
  keyboard and pointer, input method, and window lists (`foreign-toplevel`). Screenshots and
  screen sharing for other apps go through `xdg-desktop-portal`, which asks first.
- **Layer shell** is open to every client for now, until navi starts its own shell clients. A
  layer surface gets the keyboard only when clicked, and can't shrink the grid below half the
  monitor.
- **Locking** uses `ext-session-lock`. If the lock screen crashes, the screen stays locked with a
  solid colour; it never unlocks.
- **No network access**, no plugins, and no shell commands anywhere in the compositor or shell.
  Launching an app runs its `.desktop` `Exec=` line, broken into arguments, with an XDG
  activation token, inside its own `app-*.scope`.
- **Config** is plain data. A config file can never run a command.

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
  script through the same handlers as real input: one step per line, `wait <ms>`,
  `move <x> <y>` (global logical px), `press`, `release` (left button), `#` comments. A bad line
  logs one error and nothing is replayed. Release builds don't contain it.
- The shell (taskbar, corners, launcher) runs as separate `layer-shell` clients, so a taskbar
  crash can't take your windows down.

## Open questions

- Grid size: hard-coded at 12 × 6, or configurable? Leaning hard-coded.
- What counts as "minimal": the exact animations to keep.
- More look references, beyond the logo.
- Is this a daily driver? That decides whether laptop polish is in v1.
- Multi-monitor test of COSMIC in a real session, after the running T3 tasks finish.
