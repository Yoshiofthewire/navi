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
5. **Tiling is not a toggle.** There's no "tiling mode" to turn on. Dialogs, popups and windows
   too big for any tile float; everything else tiles.
6. **Every window stays on screen.** A window never scrolls or slides off its monitor. (niri
   puts windows on an endless strip that runs off the edge.)

## Layout model

Each monitor is a **24 × 12 grid** in logical pixels. A window covers a rectangle of whole cells.
Cells may be empty. There is no split tree, so nothing can flip orientation (principle 2).

24 divides by 2, 3, 4, 6, 8 and 12, so common splits are whole cells. The minimum tile is about
320×240 px, rounded up to whole cells:

| Monitor | Grid area | Cell | Minimum span (MIN 320 × 240) | Most side by side |
|---|---|---|---|---|
| Ultrawide 3440×1440 | 3428×1428 (no panels) | 142⅚×119 | 3 × 3 | 8 (3 cols each) |
| Ultrawide, dock 52 + band 44 | 3428×1332 | 142⅚×111 | 3 × 3 | 8 |
| 4K 3840×2160 @1 | 3828×2148 | 159½×179 | 3 × 2 | 8 |
| Laptop 1800×1125 @1.6 | 1113×691 | 46⅜×57⅗ | 7 × 5 | 3 |

Cell boundaries are spread evenly: boundary `k` is at `k × width ÷ 24` px, rounded down, so every
cell is within 1 px of the others and nothing collects at the last column. Tiles are 12 px
(`GAP`) apart, and 12 px from the screen edges and the panels. Each tile keeps 6 px of its cells
beside its window, 4 px below it and the 8 px grab strip above it, where only the compositor
draws (see *Dragging*); the grid is inset from what the panels leave by the rest: 6 px at the
sides, 4 at the top and 8 at the bottom. Sizes in memory still snap to whole cells.

### Remembered size

navi remembers each app's last size **in pixels**, along with the monitor it was on. The key is
the app ID, so two terminals share one memory. On any monitor, the size snaps to the nearest cell
rectangle. A terminal stays about the same physical size everywhere and never inflates to half a
4K screen (principle 1).

Memory survives restarts. navi keeps it in `$XDG_STATE_HOME/navi/memory` (default
`~/.local/state/navi/memory`), readable only by the user. It holds at most the 512 most recently
used apps, and navi ignores a damaged file. A file from the 12 × 6 grid (`navi-memory 1`) is
read with its cells doubled, which keeps the same layout, and is saved in the new format on the
next change.

If navi has no memory for an app, it gets half the monitor's width at full height. Either way,
the size is raised to the app's minimum and lowered to its maximum (see *Client sizes*), so a
fixed-size app wants exactly its size.

### Placing a new window

These steps are tried in order. The first one that fits wins. "The minimum" here is the
window's effective minimum span: navi's (about 320×240 logical px, rounded up to whole cells), or
the fewest cells that hold the app's own minimum size anywhere on the grid, below the bar if the
tile has one, where that is larger. The wanted size is the remembered one, raised and lowered to
the app's limits.

1. **Where the app was last time**, if those cells are empty on that monitor and hold its
   minimum size.
2. **The nearest empty rectangle** of the wanted size on the monitor with the pointer.
3. **Open smaller.** The nearest empty rectangle at less than the wanted size, but no
   smaller than the minimum.
4. **Shrink the neighbours.** The new window's target width is an equal share of the monitor's
   columns (24 ÷ the number of tiles in that row, counting the new one), or its remembered width
   if that is smaller, and never below the minimum. Tiles give back one column at a time, largest
   first, until the target fits. Tiles larger than their remembered size go first. Tiles may
   slide sideways, keeping their order, so the freed columns form one gap. No tile shrinks below
   its own minimum. Shrinking never changes what navi remembers for an app. On the ultrawide, two
   half-width apps plus a third become 8 + 8 + 8 columns, and a fourth makes 6 + 6 + 6 + 6.
5. **Another monitor** that has room, by monitor priority (see *Monitors*).
6. **Tab with the nearest tile.** This happens only when every tile is already at its minimum
   (principle 3), and only with a tile whose cells hold the new window and every member of the
   group below a tab bar.
7. **Float.** When nothing fits, the window floats centred on the monitor (see *Client sizes*).

When a window closes, its cells stay empty. Other windows don't jump to fill them. You use the
space when you want it.

**The window cap.** An app keeps at most 256 toplevels open, dialogs and not-yet-shown windows
included. Under Xwayland the cap is per X client, by the resource-ID base the X server assigned
(window ID & ~`0x1FFFFF`), never by `_NET_WM_PID`, `WM_CLASS` or app ID: one X11 app can't use up
another's. navi closes a window past the cap before it is placed or shown (`xdg_toplevel.close`;
X11: left unmapped, then `WM_DELETE_WINDOW`, or destroyed without it). The app's other windows
stay usable, and closing one lets the next in. navi-bar shows a *Window limit reached* card
(see *Notifications*). X11 menus keep their own shared cap, below.

### Dragging

- **Move:** hover near a tile's top edge and a thin strip lights up. Left-drag it. The grid
  appears faintly, fainter still where cells are under 80 px (a small or high-scale screen), and
  one glowing preview shows where the window will land. The preview is the window's remembered
  size, and the point you grabbed stays under the pointer: the preview moves a cell for every
  cell the pointer does, from the first one, and is clamped to the grid of the monitor under the
  pointer. If that spot is taken by a tile the pointer isn't over, the preview is the nearest
  free spot to it, or none. Drop the window onto another tile to swap them. Hold the window over the middle of a tile for about
  half a second, and the drop forms tabs instead. A swap or tab that would put a window into
  cells below its minimum (a tab group's bar included) cancels.
- **Resize:** drag any tile edge. It moves in whole cells. When two tiles share that edge, both
  change. Nothing else changes (principle 4). The edge stops when a tile reaches its minimum,
  which is the app's own minimum where that is larger. The new size becomes that app's
  remembered size.
- **Diagonal resize:** drag a corner in the gaps around a window to change its width and height
  together. The corner target is the 32 px nearest each corner of the tile's cells in every
  direction: the ends of the side and bottom gaps, and the outer 26 px of each end of the strip,
  so a top corner is as easy to hit as a bottom one. A 320 px tile keeps 256 px of strip that
  moves. Corner targets never take pixels from the app, tabs or window buttons, and a hovered or
  pressed corner lights its edges (and its strip end), so it reads as a resize. On the monitor's
  top border a top corner moves the top down only. At a four-window junction, both shared
  boundaries move.
- **Fill toward an edge:** double-click an edge (two clicks within 400 ms, moving less than 8 px)
  to extend that side to the monitor's usable edge. The opposite side stays put. It uses the same
  grid, neighbor and minimum-size rules as dragging, so another tile can stop it short. Panels
  keep their space. This is a resize and is remembered, rather than a maximize toggle.
- The strip is the 8 px gap above a window, or above its bar; the edges are the 6 px gaps beside
  it and the 4 px gap below. The inset between the grid and a screen edge or panel is no target.
- A click on the strip without moving (under 8 px) moves nothing.
- Once a hold will form tabs, the preview turns more solid, so swap and tab look different.
- Dragging a tab moves only that tab; resizing a tab group's edge resizes the group.
- The "middle" of a tile is the inner half of it, in both directions.
- The top edge of a tile on the monitor's top border isn't draggable; the strip owns that gap.
- Drop a window within half a cell of where you grabbed it to cancel a move. A drag also cancels, putting everything back,
  when the pointer leaves every monitor or the nested window loses focus.
- A float moves by its bar and resizes by the 6 px band around it, px for px with no grid or
  preview (see *Client sizes → Floats*).
- **Cursors:** every target shows what it does. Edges show `w-resize`, `e-resize`, `n-resize` or
  `s-resize`, corners `nw-resize`, `ne-resize`, `sw-resize` or `se-resize` (a tile's top corners
  at its strip's ends, a float's at its band's), and the strip and a float's bar the open `grab`
  hand. A pressed strip and every move (tile, tab, float) show the closed `grabbing` hand. A
  resize keeps its zone's cursor wherever the pointer goes until release. Tabs, window buttons, a
  cancelled drag and the wallpaper show the arrow.

### Keyboard window controls

Every press changes exactly one grid cell.

- **Alt + Arrow** moves the focused tile one cell in that direction, preserving its size. Tiles
  behind it stay put.
- **Super + Arrow** grows that side by one cell; **Super + Shift + Arrow** shrinks it by one,
  never below the minimum size (the app's own minimum where that is larger, as everywhere in this
  section). Shrinking never changes another tile.
- Growing or moving into touching tiles takes the cell from them: each tile touching that side
  (minimized ones included) moves its touching edge one cell away, and its other edges stay. If
  any of them would drop below the minimum size, nothing changes. Neither crosses the monitor's
  usable edge. A press that does nothing means a touching tile is already at its minimum size
  (shrink or move that tile first) or the tile reached the edge or its own minimum; the debug log
  says which.
- All three act on the active tab group, keep keyboard focus, and remember the focused group's
  change: moves preserve its remembered size, growing and shrinking update it. Tiles that gave up
  a cell keep what navi remembers for them.
- A focused float moves or resizes by one cell of its monitor per press, or by what is left to
  its bounds; nothing else changes and nothing is remembered.
- Maximized/fullscreen windows keep their mode and geometry. Floating dialogs and shell
  surfaces keep their app shortcuts; window shortcuts apply only to a focused tile or float. A mouse
  drag takes precedence. Settings can swap the two modifiers; Shift goes with the grow one.
  Ctrl, the other modifier, or Shift with the move modifier leave the chord to apps.

### Client sizes

An app may state a minimum and a maximum window size (xdg-toplevel `set_min_size` and
`set_max_size`; 0 means no limit). navi treats both as untrusted: values are capped at 65535 px,
a maximum below the minimum is ignored, and hints equal to the stored ones change nothing, so an
app that resends them on every commit costs nothing. An X11 app's `WM_NORMAL_HINTS` `PMinSize`
and `PMaxSize` feed the same rules: they are read at map and whenever the property changes, and
are paced by the same one-refit-a-second rule. Other X11 size fields (base size, increments,
aspect, gravity, position) are ignored.

- **Placement** uses the minimum and the wanted size, as *Placing a new window* says. A tab group's
  minimum is its largest member's.
- **The rect.** A window is sized to its cells, clamped to its maximum and anchored at the top-left,
  maximized too; the leftover px stay empty, and the bar and frame hug the window. A fixed 1220 ×
  600 app on the ultrawide gets 9 × 6 cells and is configured to exactly 1220 × 600. Fullscreen
  isn't clamped.
- **Late hints.** An app may change its limits after it's placed. If its cells still hold the new
  minimum, nothing moves. Otherwise it is placed again through the placement order, its own cells
  counting as free and its centre standing in for the pointer, and navi remembers the new size; if
  nothing fits, it floats. A re-placed window leaves fullscreen and maximize. A relaxed minimum
  never moves anything: it only lowers the floor for later resizes and shrinking. A bar that
  appears later (the app switches to navi's decorations, or an X11 app changes its
  `_MOTIF_WM_HINTS` after mapping, which also removes the bar) is checked the same way. One window is
  refitted at most once a second; hints that change faster are applied as they stand then, so an
  app flipping its minimum on every commit can't make navi re-place it and save memory each frame.
- **Floats.** A window too big for any tile floats centred in what the panels leave of its
  monitor, 8 px from their edges, at its wanted size below navi's bar. Every float gets the bar
  and its ⌄ ⌃ ✕, even an app that draws its own title bar, so it can always be minimized or closed
  to see the tiles behind it; the dock brings it back. Where the app's minimum is larger than that
  area, the float takes what the minimum needs of the monitor below the top panel, past the
  bottom one, which stays on top of it; its bar never goes under the top panel. Only a minimum
  larger than that is cut, which is logged. It holds no cells, so tiles are placed under it.
  Minimize hides it, fullscreen covers the monitor and returns it to floating, and
  maximize does nothing. It never tabs and isn't a move or swap target. It draws, with its bar and
  frame, above every tile and its decorations and below the panels and the lock screen; its
  dialogs draw above it.
- **Float zones.** A shown float's bar moves it (a click focuses it) and its buttons work as on a
  tile. A 6 px band around its bar and window resizes it: each side is an edge, and the 32 px of a
  band nearest a corner is that corner. The band takes the pointer from whatever is under it, a
  tile or another float, and is drawn only while hovered or pressed, lit like a tile's edge. The
  window itself is the app's.
- **Float order.** Floats have one front-to-back order, used for drawing, navi's zones and the
  apps' input alike. Focusing a float (a click, the dock, a dialog of it) brings it to the
  front. A float in front covers the window, bar, buttons and zones of any float behind it, so
  what you see is always what you click. On an axis a float's minimum doesn't fit, a press on its
  bar or band logs that it can't move or resize that way.
- **Float bounds.** A float stays inside its *bounds*, the area the panels leave, 8 px in, on each
  axis its bar and window fit: no move, resize, hints change or panel change puts it under a panel
  or off its monitor. On an axis its minimum doesn't fit, it keeps the spot above and can't be
  moved or resized. Its window is never smaller than the app's minimum (else 320 × 240) or larger
  than the app's maximum or the bounds.
- **Moving and resizing a float.** After 8 px the float follows the pointer 1:1: the grabbed point
  stays under it, clamped to the bounds, with no grid and no snapping. Over another monitor it
  moves there if its minimum fits that monitor's bounds; otherwise it stops at its own monitor's
  edge. An edge moves only that side, a corner two, px for px, and the opposite sides stay put.
  Double-clicking an edge fills that side to the bounds or the app's maximum. A cancel puts it
  back. The keyboard steps it a cell (*Keyboard window controls*). Apps can't move or resize
  their own float (xdg `move`/`resize` and X11 `_NET_WM_MOVERESIZE` are ignored); every float has
  navi's bar for that. Its new rect is kept, clamped to new hints at once with its top-left in
  place, until it tiles or its monitor goes (it is centred again there). It is never remembered.
  Unplugging a monitor ends a drag on it first, and a float is never left on a monitor that is
  gone.
- **Floats tile when room frees.** After every layout event (a window closes, a drag or key
  ends, a monitor or its panels change, hints change, a window is restored) each shown float,
  most recent first, tiles if free cells on its own monitor hold it: the span of its current size
  nearest its centre, else the largest free span down to its minimum. It never shrinks or moves
  another window, never tabs, never picks another monitor, and waits while a drag is in progress,
  while it is minimized or fullscreen, and while its monitor has a maximized group. A float
  dropped on a monitor with room tiles there this way. Memory learns its spot, as for any tile.
  This is deliberate (principle 5), even when an app in the background frees the room.
  Otherwise a float tiles again when its hints change and a step fits, or when it reopens, and
  doesn't change what navi remembers. Unplugging its monitor runs the placement order again from
  the main monitor.
- **The clip.** Every window's own surfaces are drawn and take pointer input only inside its
  rect: the clamped tile rect, the float's rect, a dialog's rect, or the monitor while
  fullscreen. A popup (menu, tooltip) is kept inside its window's rect: navi slides, flips or
  shrinks it there when it's placed, and draws and hits it only there, whatever size the app asks
  for or attaches. So a popup, with or without a grab, can't cover or take input meant for a
  neighbouring tile. An app keeps at most 64 popups open and nests them at most 16 deep; navi
  dismisses any more, and any popup without a parent except navi-bar's. An X11 menu
  (override-redirect: menu, tooltip, drop-down) gets the same rect as its *owner*'s popups, the
  first window up its X11 parents that isn't a menu (at most 16 steps; none, a loop or a longer
  chain and it isn't shown). navi can't move an X11 menu, so it's drawn and hit only where it
  overlaps that rect, unmapped while that's empty, and stacks with its owner (with a float,
  above it). At most 64 X11 menus are tracked at once, one cap shared by every X11 app (navi
  sees all of Xwayland as one client): a flood from one X11 app can keep another's menus from
  showing until it closes some. A window that draws larger, such as a CSD shadow or a buffer
  bigger than its configure, is cropped there, and a click just outside it reaches what's below.
  So no app draws over, or takes clicks meant for, another tile or navi's zones, except through
  a dialog in front of that tile (below, and *Stacking order*).
- **Rounded tiles.** Each shown tile and float has one outline: its bar and window (the window
  alone without a bar), corner radius `RADIUS` (10 px, times the output scale), clamped to half
  its shorter side. navi's chrome and the app's own pixels are cut to it, antialiased over one
  px, so no square app corner pokes past the arc; under a bar the window's top corners are
  square and the bar's round. The focus or idle frame is a ring just outside it, outer radius
  `RADIUS` plus its width, so the app's cut doesn't change with focus. A maximized tile is
  rounded; a minimized strip and the move preview are rounded by their own rect. Fullscreen
  windows, dialogs and their frames, the lock surface and layer surfaces are never rounded.
  The cut only removes px: a popup or X11 menu is bounded by its owner's outline instead of its
  rect, so one flush with a corner loses the px past the arc and one inside is whole. A window
  takes the pointer exactly where it's drawn: a cut-off corner (about 21 px² at scale 1) belongs
  to what shows through it, the tile behind a float or nothing, and so does its cursor; the
  bar's tabs, buttons and strip there are no zone. Where a tile's or float's frame is drawn over
  its cut-off corner (out to the ring's outer arc, wider while focused), the px are its resize
  corner, as the frame px beside them in the gap are, so a click on frame it can see focuses
  that window, never the one behind or nobody. A maximized tile has no corners, there or in
  the gap. Otherwise the resize corners, edges and strip keep their geometry in the gaps: the
  cut is too small for a second corner rule, and letting the window keep its square hit rect
  would let a click on a px showing the tile behind go to the float. A hovered or pressed
  corner lights the same edge zones and strip end, but where they meet it is a quarter ring
  concentric with the frame: from the outline (covering the frame, as a lit straight run does)
  out to the frame's outer radius plus how far the zones reach past it, so the band's outer
  corner shows what is behind. Two GLES programs, compiled once per GL context, draw it in both
  backends: one fills chrome in the corner squares (everything else stays a plain rect, so an
  idle desktop still has no damage), one cuts app surfaces and window buttons that reach a
  corner. If they don't compile, navi logs one warning and draws square. A cut surface can't be
  scanned out directly on DRM; fullscreen windows aren't cut, so fullscreen video and games
  keep direct scanout.
- **X11 stacking.** X11 apps get navi's focus by the outline, but Xwayland sends X11 pointer
  events to the top window at the pointer in its own stack of square windows. So navi pushes
  its drawn order there after every restack (the tiles, then the floats, back to front), and a
  client's own restack request stays ignored: where X11 windows overlap, the one drawn in front
  gets the motion. A press or scroll lifts the X11 window navi gives it to the top of that
  stack until the pointer next moves with no button held, so clicks and the wheel follow the
  outline too. Only inside a front X11 window's cut-off corner (about 21 px²) over another X11
  window does Xwayland still give motion to the front one; X input shapes would close that.
- **Dialogs.** A dialog (an xdg toplevel with a parent, or an X11 `WM_TRANSIENT_FOR` window) has a
  *root*: the first tiled or floating window up its parents. The walk stops after 8 steps or on a
  loop; a chain that ends without a tiled window, is too deep or loops makes an *orphan*. Its
  *family* is every window with the same root; an orphan's is its own app's windows. A dialog
  stays inside its root's monitor's bounds, the same as a float's (the area, 8 px in); an orphan
  uses the monitor it was last shown on, else the main one. navi sends every dialog those bounds
  (`configure_bounds`) and never sizes it otherwise, but a dialog larger than its bounds on an
  axis is configured once with that axis lowered to the bound, at most once a second. That size
  stays its configured size until it asks for another, even if the bounds later grow. It is
  centred on its parent (a tile's or float's rect, or a parent dialog's), then slid into the
  bounds, again whenever the parent moves or resizes, the panels change, or it commits a new size.
  Its rect is its clip, and every shown dialog has navi's frame around it, so even a transparent
  one can be seen. Inside its monitor a dialog may cover neighbouring tiles, so a file picker
  larger than its window still works, but it is always visible where it takes clicks. While it is
  in front of a neighbour it covers that neighbour's bar, buttons and frame too, and takes the
  clicks there; clicking the neighbour's window outside it brings the neighbour back on top.
  A new dialog takes the keyboard only if a window of its own family holds it; otherwise it maps
  without it (logged), *waits*, and gets it when the user clicks it, or focuses its parent from
  the dock or Super+N and then clicks it. A waiting dialog's frame is full accent (1 px), its
  root's dock icon has an accent dot and, in a tab group, its root's tab an accent line along its
  bottom, until the dialog is focused. An orphan never takes the keyboard on map. A dialog hides
  while its root is hidden or minimized, and loses the keyboard; it comes back with it. When a
  dialog's parent closes, it hangs from that parent's own parent if it had one, else it is an
  orphan centred in its last bounds. A closed window that had the keyboard gives it back: a dialog
  to its parent, any other window to the window that had the keyboard right before it. It goes
  back only if the user gave that focus and the keyboard hasn't moved since, or, for a dialog, if
  the parent held the keyboard right before the dialog took it and the keyboard hasn't moved since
  (the parent gets back only what it held). Either way that window must still be visible (not
  hidden, minimized or wholly covered) on the same monitor, outside the lock; otherwise the
  keyboard goes to nobody. That includes a force close: keys typed at a hung app never land in
  another window the moment it dies. Every hide (a dialog with its parent, a re-parent to a hidden window,
  an X11 menu with its parent) takes the keyboard off the hidden window before the next key.
- **Stacking order.** Windows stack by one rank: the last time they were raised, by the user (a
  click, the dock, Super+N, or an activation whose token came from the dock or from a window the
  user focused) or by opening and taking the keyboard. Any other activation (a token minted by a
  window that took the keyboard on its own, or one redeemed after the keyboard moved) neither
  raises the window nor takes the keyboard: it only marks the window's dock entry and tab, as a
  waiting dialog does, until the user switches to it. navi's own refocus (giving the keyboard back
  after a close or a hide) raises nothing. A family stacks by its newest raise, its dialogs above
  its root, and an X11 menu (override-redirect) above its X11 parent's family, so a window the
  user raises later covers it. An orphan dialog's family is its own parent chain: it keeps only
  what the user raised of its old family, else goes just below the window on top when it becomes
  an orphan, so a window that opens and closes at once hands nothing on. Nothing else moves a
  window up: a commit, a re-parent, a map or a re-show (after a minimize, a fullscreen or
  maximize, a tab switch, an unplug) puts every window back at its rank, so no app can put itself
  above a window the user raised later. navi's chrome stacks in the same order: each shown tile
  group's bar, tabs, buttons, frame and lit zone draw right above the group's window, so a dialog,
  orphan or X11 menu above that window covers them, and a neighbour the user raises later covers
  the dialog, chrome included. Minimized strips draw below every tiled window and dialog, the
  move grid and preview above them; floats, with their chrome, stay above all of it. Zones follow
  the drawing: a tile's zone takes the pointer only where no window above its group's window is,
  so a click over a dialog's rect reaches the dialog, whatever chrome lies beneath it.
- **Logs.** One `info` line per change: `client size hints` (app, id, min, max and the outcome),
  then `re-placed`, `floating: no tile fits` (and `floating: its minimum is bigger than the
  monitor` when it is) or `tiled again` with the cells or rect (`by="room freed"` when a float
  takes freed cells), `float moved` / `float resized` with the rect, and `clipped`
  with both sizes when a window's geometry is larger than its clip rect. An app that alternates its
  hints logs at most one refit a second. `dialog: capped to its monitor` (its size, the cap and
  its rect, once per dialog per bounds), `dialog: focus refused` (app and root) when a new dialog
  maps without the keyboard, and `dialog: re-resolved` (old and new root) when a parent closes.
  `X11 menu not shown: too many open` (the cap) at most once a second while X11 menus are
  refused at the cap. `toplevel refused: too many open` (app, cap; X11 also the client base) at
  most once a second while windows are refused at the window cap.

### Tabs

- A tab group shows one window at a time. Above it, a 24 px bar sits between the strip and the
  window and holds one tab per window, left to right in the order they opened. The active tab is accent blue, inactive
  tabs are outlined, and minimized ones are a dimmed blue. Tabs have no titles or icons yet; those
  come later, since titles would need fonts in the compositor and fonts stay out of it. For now a
  tab tells you its window only by position.
- Click a tab to switch to it. Drag a tab out (8 px or more) to untab that window: it lands where
  its preview shows, at its wanted size, as a moved window does, or tabs into another tile
  after a half-second hold. Dropping it on another tile without the hold, or within half a cell
  of where you grabbed it, cancels.

### Window controls

- Apps that draw their own title bar (Konsole, Firefox) keep it. Apps that ask for server-side
  decorations, or bind xdg-decoration and leave the choice to navi, get a 24 px bar with **minimize, maximize and close** buttons on the right; the rest
  of the bar moves the window, like the strip above it. The buttons are Breeze-like line
  glyphs in the text colour, as in Navi Settings and the utilities: a chevron down, a chevron up
  and a plain cross, antialiased at the output's scale, on a rounded selected fill under the
  pointer. They are 24 px squares, flush right and touching, as tight as KWin's. Navi Settings'
  and the GTK utilities' header buttons use the same 24 px boxes with no space between them.
  Close asks the app to close; navi never kills it.
- A float always has this bar, whatever the app draws, and the bar moves it.
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
- Dialogs stay floating over their parent, inside its monitor, framed; they hide while their
  root is hidden and re-centre when the parent moves (*Dialogs* above). Focusing a window brings
  its dialogs in front of it. Apps can't move or resize their own dialogs.
- **Fullscreen** (the app asks for it) covers the whole monitor, taskbar included, on the
  monitor the window is on, and hides the other windows there; the taskbar can't be seen or
  clicked meanwhile, but keeps its space. It ends when the app leaves fullscreen, closes or is
  minimized, when a new window opens on that monitor, when another window there is
  activated, or when the user presses Super+Esc on that monitor (the focused window's, else
  the pointer's); the window goes back to its tile. Super+Esc is navi's: apps never see it,
  whether or not anything is fullscreen. A window left by Super+Esc can't go fullscreen again
  until the user types a non-modifier key that is no navi shortcut into it, or clicks it.
  The same guard holds a fullscreen window ended because the user activated another window
  through the taskbar (a click or a dock key), so the app can't cover the user's choice again.
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
- **Taskbar:** a compact floating dock centered 12 logical px above the bottom of its monitor,
  reserving 52 px. Its visible body is a 40 px high panel (see *Look*), with 24 px icons in 36 px
  slots and a blue active-window indicator. The launcher opens 12 px above it. The logo opens the launcher. Pinned apps come first, in pin order;
  other running apps follow in opening order. Each app has one icon, with up to three dots for
  its windows. Click an app to launch it if closed, focus or restore it if running, or cycle its
  windows when already focused. Minimized apps are dimmed. An app with a window waiting for the
  user (a dialog refused the keyboard, *Dialogs*, or an activation that wasn't the user's,
  *Stacking order*) has an accent dot at its icon's corner until that window is focused, so it
  can be found even while minimized or hidden.
  **Super+1…9** focuses or restores the Nth running app (pinned apps that aren't running take no
  number) at its first visible window, else its first; on the focused app it does nothing.
  **Super+Tab** and **Super+Shift+Tab** step focus to the next or previous running app in dock
  order, wrapping, and skip apps whose windows are all minimized. Both are navi's (apps never see
  them), work over fullscreen, and follow the activation rule: another window on a fullscreen
  monitor ends that fullscreen. The top-row digit keys count on any layout; the keypad doesn't.
  Right-click an app's icon for its menu (*Context menus*).
- **Pins:** pin or unpin an app from its right-click menu, in the dock or the launcher, or press
  Insert on a selected app in the launcher. Pins survive restarts in
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
  launcher to fit. An app row is its icon and name, with `Pinned` in the secondary text colour
  at its end when it's pinned; a folder row ends with its app count. Right-click an app row, or
  press **Menu** or **Shift+F10** on the selected one, for its menu (*Context menus*). The search
  and rows have distinct targets, and transparent space around the dock and launcher takes no
  clicks.
  Folder rows use KDE's per-category icons (`applications-internet-symbolic`,
  `applications-development-symbolic`, `preferences-system-symbolic`, and the other matching
  category names), falling back to their non-symbolic name and then a generic folder.
- **Corners:** on the main monitor, and they stay there when the taskbar moves or the main
  monitor changes. A 32 px status
  band floats 12 px below the top edge and 12 px in from the sides, the same panel as the dock;
  with its margin it reserves 44 px. Navi's logo
  and name align left, the local date and 24-hour clock sit in the center, and a battery glyph
  and percentage align right, labeled while charging. Battery capacity is the mean of the
  system's batteries, read every 30 s; mice and headsets don't count. The bar takes space from
  the grid and hides in fullscreen. Only the Navi button and its open session menu take clicks;
  the rest of the band is transparent to input.
  Notifications are top-left, 24 px below the band and from the left edge, so they float clear
  of the corner of the tile under them: panels with the accent as their rim (3 px with visual
  alerts). They wait, hidden and without expiring, while the session menu, which shares that
  corner, is open, and come back with the time they had left. Volume, network and tray come later. Volume has no
  small route: PipeWire's native API is C (`libpipewire`, which needs `unsafe` and a new system
  library), pipewire-pulse has no D-Bus volume interface, and running `wpctl` would break the
  no-shell rule.
- **Session menu:** click Navi at the upper left to open Sleep, Lock Screen, Switch User,
  Log Out, Restart, and Shutdown, in a panel 12 px below the band, aligned with its left edge.
  Up/Down or Tab selects, Enter opens a confirmation with Cancel
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
  Settings. A card click first asks navi for an activation token for that press and sends it to
  the sender as `ActivationToken`, then `ActionInvoked`, so the app can raise itself as a dock
  click would; with no token within 1 s it sends `ActionInvoked` alone, which only marks the
  window. Actions from Settings never carry a token: that click is in another process. The
  card's `NotificationClosed` follows the action. A click within 500 ms of the cards appearing
  or changing gets no token, so an app can't slip a card under a click aimed elsewhere.
  Replacing/closing is still sender-scoped. Global enable, Do Not Disturb, per-app blocking and
  a 50-entry in-memory history are configurable. Blocked notifications never enter history.
  History/actions require an unlocked compositor settings check. Fullscreen and locking hide
  notification surfaces. Optional UnifiedPush is a separate, owned external distributor,
  disabled by default; payloads reach their participating apps, which decide whether to notify.
  See [Settings](settings.md) and [UnifiedPush](unifiedpush.md). navi-bar's own *Window limit
  reached* card (sender `navi`, no actions, the default lifetime) says navi closed a window past
  the window cap. navi asks for it at most once a second, over the private control channel and
  with no app text. A repeat replaces the shown card silently, so a flood shows one card. No bus
  sender can close or replace it.
- **Context menus:** a right-button press on a dock icon or a launcher app row opens that app's
  menu next to the pointer; the release is ignored. No other part of the shell has one.

  | Target | Items |
  |---|---|
  | Dock app with windows | header, **New window**, the app's desktop actions, **Pin to dock** / **Unpin from dock**, **Close window** / **Close all N windows** |
  | Pinned dock app, no windows | header, **Open**, desktop actions, **Unpin from dock** |
  | Dock app with no desktop entry | header, **Close window** / **Close all N windows** |
  | Launcher app row | header, **Open**, desktop actions, **Pin to dock** / **Unpin from dock** |

  The header (the app's name) and the lines between groups can't be selected. Desktop actions come
  from the entry's `Actions=` (at most six, each with a name and an `Exec` the launcher would run).
  A left press or Enter runs an item; Up/Down move (wrapping), Home/End go to the ends, and every
  other key is swallowed. Hovering an item selects it. Esc closes only the menu; a left press
  elsewhere on the shell closes it and does nothing else. A click into a window, the lock, a window
  going fullscreen on the bar's monitor, the taskbar moving and the app going away all close it.
  Opening it closes the session menu, and from the dock the launcher. It holds the keyboard while
  open and returns it as the launcher does. A dock menu sits 12 px above the dock, its left edge at
  the pointer, flipped left at the end of the dock, and centred over a dock narrower than the menu;
  a launcher menu stays inside the launcher, and its row stays highlighted. The menu scales with the
  text size: 260 px wide at 14 px text and wider with bigger text, up to the launcher's 420 px, with
  rows growing to match, so every label and the whole confirmation fit at any size Settings allows.
  Names too long for a row are cut with `…`; the confirmation's heading cuts only the name, so
  "Force close" and the "?" stay. Only the launcher's menu opens from the keyboard (Menu,
  Shift+F10); the dock's opens with the mouse, then takes the keyboard like the other. **Close** is
  polite: the same request as a window's ✕ (`xdg_toplevel.close`, or `WM_DELETE_WINDOW` for X11),
  for each of the app's windows; the app decides, and may ask about unsaved work. **Force close:**
  if a window's first close (✕ or the dock) has gone unanswered for 90 s, its dock menu offers
  **Force close…** below Close, and Close itself now asks to force close. So does another ✕ on
  navi's title bar: it asks the app again and opens that app's dock menu straight in the
  confirmation, over its dock slot. The 90 s count from the first close and never restart, so a
  window kept open by cancelling a "Save changes?" prompt is offered it too. The confirmation names
  the app and says that force close ends its process (an X11 app: its X connection) with every
  window it has open, which may include windows the dock shows under another app, and that unsaved
  work is lost. Cancel is selected, and only the arrow keys and Tab move the selection. Hovering a
  button only tints it (`HOVER`, no accent mark): it never selects, so Enter still acts on the
  selected one. Its buttons sit under the heading, away from the rows that open it. For 500 ms after
  it appears it acts on nothing (Enter then is swallowed), and a button press counts only once the
  pointer has really moved, not for the motion navi sends when the panel resizes, so neither a
  double-click nor a double Enter can confirm. Opened from a ✕, the pointer's first spot on the
  panel is its reference. An output, scale, configure or text-size change while it's open restarts
  the 500 ms, and the pointer's next spot becomes its new reference. Confirmed, navi checks the 90 s
  itself, then ends that window's client: a Wayland app is sent SIGKILL through a pidfd taken when
  it connected and disconnected; an X11 app's X connection is ended with XKillClient, never a
  signal, over a connection that gives up after 2 s. navi, navi-bar, the locker and Xwayland are
  never force closed, and once Xwayland is gone no X11 force close is tried. Nothing is automatic.
  If navi refuses, fails or doesn't answer for a window that is still open, navi-bar posts one card
  naming the app that says it wasn't force closed and gives the path of the log navi writes,
  `$XDG_STATE_HOME/navi/navi.log` (default `~/.local/state/navi/navi.log`), or "navi's log" with no
  usable state dir; it follows the notification settings like the window-limit card. A Lock from the
  session menu waits for at most one force close.
- **Laptop alone:** the same layout with a slimmer taskbar.
- **No virtual desktops.**
- **Look:** cyberpunk, with no animations: every state change is instant. (The half-second hold
  before a drop tabs is a state change, not an animation.) The accent is the logo's neon blue,
  `#2BA3D8`, on near-black, from one palette (`src/theme.rs`) that navi and navi-bar share.
  This dark/blue baseline remains the default. Appearance offers light mode and blue, green,
  purple, amber or pink accents; Accessibility offers high contrast. These are opt-in variants
  of the same system, preserving geometry, hierarchy and instant state changes. The
  window with the keyboard has a 2 px accent frame just outside it, in the gap; every other shown
  window, dialogs included, has a dim 1 px one, so the gaps read as thin neon lines and the grab
  strips stay easy to find. A tile's lit strip or edge and its bar draw in front of its frame,
  a window stacked above the tile (a dialog) in front of all three, and the shell's panels in
  front of everything.
  The shell's panels (dock, launcher, status band, session menu, context menus and notification
  cards) float:
  12 px (`INSET`) from the screen edges and from what they sit next to, with 10 px corners
  (`RADIUS`), the shell fill and a 1 px rim. The dock and band, which sit over the wallpaper,
  are 85 % opaque; that alone keeps their text 4.5:1 and marks 3:1 over any wallpaper, white
  or black, and high contrast makes them opaque. The pop-ups (launcher, session menu, context
  menus and cards) sit over windows and are opaque, so nothing under them shows through.
  Panels pad their content 12 px. Folder and session glyphs draw in the text colour, so they
  hold 3:1 in every scheme; app icons keep their own colours. The launcher's search field is
  the selected fill with a rim, so it has a shape even in high contrast. A context menu's
  selected item has the selected fill and a 3 px accent mark at its left edge, which holds 3:1
  where the fill alone doesn't; the force-close button has an accent rim. The band is 32 px
  (one line of text) and the dock 40 px (24 px icons in 32 px targets); they share radius and
  rim. Pixels outside a panel are clear and take no clicks. Hover and
  selected fills inside a panel stay opaque. Windows stay solid, with no translucency or shadows,
  and share the panels' corners (*Rounded tiles*). No desktop widgets, dock
  magnification, reflection or bounce: it isn't Mac-like.
- **App menu button:** the navi logo, under the icon name `navi` (in `assets/icons/hicolor`). It's
  installed into `hicolor`, so an icon theme can replace it. A hand-simplified version with
  single lines is used at 16–32 px, where the full logo's double lines blur together.

## Themes

`src/theme.rs` owns the compositor/shell palette and its scheme-dependent accent values. The default
background, shell surface and text are `#0B0D12`, `#151922` and `#D0D4DC`; light mode uses
`#F5F6F8`, white and `#202734`, with a darker blue accent (`#0077AA`). High contrast uses
black/white surfaces and text in the selected scheme. Shell panels use `RADIUS` 10 and `INSET` 12,
the `BAR_BG` fill at `panel_alpha()` (0.85 for the dock and band, 1.0 for pop-ups and with
high contrast), `DIM` `#A3ABBC` (light `#525E70`) for secondary text, and `PANEL_EDGE` `#848C9E`
(light `#6B7689`) for their rims and the dock's pinned mark; `theme` tests prove both 4.5:1 text and
3:1 marks on a panel composited over white and black, for every scheme and accent. The GTK utilities
keep `#7A8396` for `DIM` and `SHELL_EDGE` for their borders. Settings maps these same roles onto
native GTK controls in `app.py` and `extra_ui.py`, with separate control borders and secondary-text
values for legibility. Noto Sans remains the Settings/shell typeface; the default text size is 14
logical px, adjustable from 12 to 24 without changing the layout model.

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
  are refused; then navi logs one warning and keeps the host's pointer. Other shapes (navi's
  resize and grab cursors, apps' named cursors) are found the same way under their CSS name, then
  their legacy X11 names (`top_left_corner`, `openhand`, `closedhand`, `xterm`…), with the same
  caps; a missing or refused shape draws the arrow. Each file is read at most once per theme
  load, hit or miss, and only names from navi's fixed table and `cursor-shape-v1`'s list are ever
  looked up, never a client's string.
  Built now: navi's zone and drag cursors (see *Dragging*), apps' named cursors through
  `cursor-shape-v1` (an I-beam over text and so on), apps' own cursor surfaces
  (`wl_pointer.set_cursor` with a buffer: every X11 app through Xwayland, and Wayland apps
  without `cursor-shape-v1` such as GTK 3), and `XCURSOR_THEME` and `XCURSOR_SIZE`
  (24 × scale) set for navi-bar and the apps it launches. What shows, top rule first: while
  locked, the lock screen's request; during a drag, the drag's cursor; over a Top or Overlay
  panel, that panel's request; over a navi target, its cursor; else the request of the app under
  the pointer. A cursor surface draws only in that last case, at the pointer minus its hotspot
  (`wl_surface.offset` moves it); locked or over a panel it is the arrow. It must be at most
  256 × 256 logical px and 1024 px a side in its buffer, at least 1 × 1, with its hotspot inside
  and no subsurfaces; one that breaks a cap, has no buffer or is destroyed draws the arrow, never
  nothing. It gets frame callbacks only from frames that drew it, so an animated cursor runs no
  faster than navi draws it. An app's request counts only while the pointer is over it: each new pointer focus
  starts from the arrow, and a request from an app without pointer focus or with a stale enter
  serial is dropped. Pointer focus follows the topmost surface under the pointer whenever the
  scene changes (a window, dialog, popup, panel or lock surface maps, unmaps, moves or is
  raised), not only on motion, except while a button or grab holds it; it never moves the
  keyboard. An app may hide the pointer only over its own surface, never over navi's
  targets or during a drag. Still to come: importing the cursor chosen in KDE, animated theme
  cursors, and cursors for navi-bar's own UI. Nested without a cursor theme the host draws the
  pointer, so apps' cursor surfaces don't show. Nested, the pointer stays at the edge
  where it left the window, since the winit backend doesn't report leaving.
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
  back after you've moved on. Only an activation by the user (a token from the dock, or from a
  window the user focused, redeemed before the keyboard moved) restores, raises and focuses the
  window; any other only marks its dock entry and tab (*Stacking order*). This replaces job 006's
  rule that every accepted activation moves the keyboard. One exception to the keyboard rule:
  the notification cards never take the keyboard, so navi-bar may also get a token with the
  serial of the latest left-button press navi delivered to that same navi-bar surface (never
  another client's, an older or made-up serial, or while locked; one token per press), and only
  once that surface has been on screen 500 ms: cards revealed when fullscreen or the lock ends
  restart that clock, so a click as they appear mints nothing. It works once, within 10 s, and
  only while the keyboard hasn't moved and no other click came since it was made, or the
  keyboard is already on the window being activated; then it focuses as the user's. The window
  you were typing in keeps the keyboard until the app takes it.
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
- **Request floods (known, unfixed):** one client can still freeze navi (no frames, no input, no
  lock screen) by writing requests faster than navi reads them, because pure-Rust
  wayland-backend reads a client's socket until it is empty before serving anyone else. navi
  keeps the Rust request parser over libwayland-server for memory safety (2026-11-12). What is
  capped: a surface holds at most 64 damage rectangles (past that, one covering the whole
  buffer) and 16 frame callbacks (older extras get `done` at once), so a flood or a never-drawn
  surface can't grow memory through them, and navi recovers once the flood stops. Still
  unbounded: the freeze itself, object-creation floods (`create_region`, `wl_surface.frame`
  without a commit, any request that makes an object), `wl_region` rectangle lists, the cached
  state of a sync subsurface whose parent never commits, and CPU.
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
  (press and release), `down <code>` and `up <code>` (to hold a modifier), `wheel up` and
  `wheel down` (one notch), `#` comments. A bad line logs one error and nothing is replayed.
  Release builds don't contain it.
- The shell (taskbar, corners, launcher) runs as separate `layer-shell` clients, so a taskbar
  crash can't take your windows down.

- **Backends:** nested (a window in another compositor) when `WAYLAND_DISPLAY` or `DISPLAY` is
  set, else real hardware: DRM/KMS on the seat's primary GPU only, libinput, libseat. Both draw
  each output from the same element list (`src/render.rs`). See `docs/session.md`.

- **XWayland:** Smithay's rootless server and XWM manage X11 windows in the same layout as
  native apps. Embedded NULs in `WM_CLASS` and window titles become U+FFFD; the resulting text
  is capped at 2048 bytes at a UTF-8 boundary before layout or Wayland window-list events use it.
  This keeps complete events within the backend's 4096-byte buffer. The metadata reaches the
  dock. X11 keyboard focus is set as well as Wayland focus. Tiled X11 windows get their minimum
  and maximum size from `WM_NORMAL_HINTS`, as *Client sizes* says; transient dialogs and menus
  ignore those hints. Transient dialogs follow the *Dialogs* rule: the same bounds, cap (in their
  X11 configure), frame and focus rule as Wayland dialogs. A transient naming another X client's
  window isn't refused: X11 clients on one Xwayland already see and inject each other's input,
  so it gains nothing navi could deny, and it can never name a Wayland window, so the focus rule
  still keeps it from taking keys from Wayland apps. Override-redirect menus keep their X11
  geometry and are cut to their owner's rect (*The clip*); they show only while their owner
  does. Clipboard transfers are bridged, with X11 access gated on keyboard focus. Clipboard MIME
  names containing NULs or exceeding 2048 bytes are rejected; accepted names are preserved
  exactly. Primary selection, cross-protocol drag-and-drop, and automatic XWayland crash
  recovery remain unimplemented. Install the `Xwayland` executable to enable it; native apps
  still work without it. The headless integration check is
  `cargo test --locked xwayland::tests::x11_lifecycle_and_input -- --ignored`.

## Open questions

- Grid size: hard-coded at 24 × 12; no per-monitor or configurable grid.
- More look references, beyond the logo.
- Is this a daily driver? That decides whether laptop polish is in v1.
- Planned look work, after the floating panels:
  - **Frosted panels.** Blur behind navi-bar's panels via `ext-background-effect-v1`, advertised
    only to navi-bar like layer-shell, with each panel's region capped at 16 rectangles, clamped
    to the surface and rounded by `RADIUS`. Only the wallpaper is blurred, once per output and
    wallpaper change on a worker with the lock backdrop's bounded box blur, so a frame costs one
    textured quad. Off in high contrast and over fullscreen.
- Screen readers: navi-bar draws its own pixels with no toolkit, so it has no accessibility
  tree, and the dock and launcher button are invisible to them. Exposing them means building
  one, e.g. an AT-SPI adapter such as `accesskit_unix` (a new crate tree and D-Bus surface).

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
