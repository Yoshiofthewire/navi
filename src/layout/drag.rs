//! Moving and resizing tiles with the mouse and keyboard, as docs/design.md describes.

use super::{
    COLS, Cells, Cut, GAP, Layout, MIN_H, MIN_W, Memory, Output, ROWS, Rect, STRIP, Shape, Tile,
    WinId,
    bar::{self, Button},
    dist2,
    float::FloatDrag,
};
use crate::theme::{ACCENT, MINIMIZED, RADIUS, rgba};

/// A press on a strip becomes a move once the pointer travels this far, in px.
pub(super) const ARM_PX: u64 = 8;
pub(super) const CORNER_PX: i32 = 32;
const DOUBLE_CLICK_MS: u64 = 400;
/// Holding a move over a tile's middle this long, in ms, tabs instead of swapping.
const HOLD_MS: u64 = 500;
const GRID_ALPHA: f32 = 0.2;
/// Cells smaller than this, in logical px, fade the grid so it doesn't become a mesh.
const GRID_FULL_PX: i32 = 80;
const GRID_MIN_ALPHA: f32 = 0.06;
const PREVIEW_ALPHA: f32 = 0.35;
/// Once a hold over a tile's middle will tab, the preview turns this solid.
const TAB_ALPHA: f32 = 0.55;
/// The frame around the window with the keyboard, in px, at full accent.
const FOCUS_PX: i32 = 2;
/// The frame around every other shown tile, so the gaps read as dim neon lines.
const IDLE_PX: i32 = 1;
const IDLE_ALPHA: f32 = 0.3;

/// A one-cell keyboard change to the focused group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nudge {
    Move,
    Grow,
    Shrink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

/// A navi zone in the gaps: a tile's grab strip (or the free part of its bar), an edge or corner,
/// a tab, or a window button of the group's active tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Strip(WinId),
    Edge(WinId, Side),
    Corner(WinId, Side, Side),
    Tab(WinId),
    Button(WinId, Button),
}

impl Hit {
    pub fn id(self) -> WinId {
        let (Hit::Strip(id)
        | Hit::Edge(id, _)
        | Hit::Corner(id, _, _)
        | Hit::Tab(id)
        | Hit::Button(id, _)) = self;
        id
    }

    /// The cursor over this zone: which way it resizes, or that it moves.
    pub fn cursor(self) -> &'static str {
        match self {
            Hit::Edge(_, Side::Left) => "w-resize",
            Hit::Edge(_, Side::Right) => "e-resize",
            Hit::Edge(_, Side::Top) => "n-resize",
            Hit::Edge(_, Side::Bottom) => "s-resize",
            Hit::Corner(_, Side::Left, Side::Top) => "nw-resize",
            Hit::Corner(_, Side::Right, Side::Top) => "ne-resize",
            Hit::Corner(_, Side::Left, _) => "sw-resize",
            Hit::Corner(_, _, _) => "se-resize",
            Hit::Strip(_) => "grab",
            Hit::Tab(_) | Hit::Button(..) => "default",
        }
    }

    /// The cursor while this zone is pressed.
    fn pressed(self) -> &'static str {
        match self {
            Hit::Strip(_) => "grabbing",
            hit => hit.cursor(),
        }
    }
}

/// A click on navi's controls, as `Drag::release` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Activate,
    Restore,
    Minimize,
    Maximize,
    Unmaximize,
    /// Not performed by the layout: glue asks the app to close.
    Close,
}

impl Action {
    pub fn log(self) -> &'static str {
        match self {
            Action::Activate => "tab switched",
            Action::Restore => "restored",
            Action::Minimize => "minimized",
            Action::Maximize => "maximized",
            Action::Unmaximize => "unmaximized",
            Action::Close => "close requested",
        }
    }
}

/// What a drag did.
#[derive(Debug, PartialEq)]
pub enum Done {
    /// Tiles moved or resized: what happened, and the tiles that changed.
    Layout(&'static str, Vec<Tile>),
    /// A click, applied to the layout already except `Close`.
    Click(Action, WinId),
}

impl Layout {
    pub fn hit(&self, p: (i32, i32)) -> Option<Hit> {
        self.hit_where(p, |_| false)
    }

    /// `hit(p)` without the zones of the groups `covered` names (by their active tile).
    pub(super) fn hit_where(&self, p: (i32, i32), covered: impl Fn(WinId) -> bool) -> Option<Hit> {
        // Floats, and their bands, are above every tile's zones.
        if let Some(hit) = self.float_hit(p) {
            return hit;
        }
        // One set of zones per group, from its active tile; none for a fullscreen tile or one it covers.
        let ids = || {
            self.tiles
                .iter()
                .filter(|t| !t.floating && self.active(t.id) == Some(t.id))
                .filter(|t| !self.covered(t) && !t.fullscreen && !covered(t.id))
                .map(|t| t.id)
        };
        // Top zones go last: where one meets the bottom zone of a tile above, that one wins.
        let hit = ids()
            .flat_map(|id| {
                [
                    Hit::Strip(id),
                    Hit::Edge(id, Side::Left),
                    Hit::Edge(id, Side::Right),
                    Hit::Edge(id, Side::Bottom),
                ]
            })
            .chain(ids().map(|id| Hit::Edge(id, Side::Top)))
            .find(|&h| self.zone(h).is_some_and(|z| z.contains(p)) && !self.cut_off(h.id(), p))?;
        let Hit::Strip(id) = hit else {
            return Some(self.corner_hit(hit, p));
        };
        if let Some(side) = self.strip_corner(id, p) {
            return Some(Hit::Corner(id, side, Side::Top));
        }
        let Some(bar) = self.bar(id) else {
            return Some(hit);
        };
        if let Some(&(b, _)) = bar.buttons.iter().find(|(_, r)| r.contains(p)) {
            return Some(Hit::Button(id, b));
        }
        match bar.tabs.is_empty() || p.1 < bar.rect.y {
            true => Some(hit),
            // The 2 px between tabs are no zone.
            false => bar
                .tabs
                .iter()
                .find(|(_, r)| r.contains(p))
                .map(|&(t, _)| Hit::Tab(t)),
        }
    }

    /// Whether `p` is in `id`'s outline rect but cut off by its rounding: the strip's px on a
    /// bar's corner belong to what shows through.
    pub(super) fn cut_off(&self, id: WinId, p: (i32, i32)) -> bool {
        self.shape(id)
            .is_some_and(|s| s.rect.contains(p) && !s.contains(p))
    }

    /// Corners use only the existing gaps, leaving client pixels, tabs and buttons alone.
    fn corner_hit(&self, hit: Hit, p: (i32, i32)) -> Hit {
        let Hit::Edge(id, side) = hit else { return hit };
        let Some((c, _)) = self.frame(id) else {
            return hit;
        };
        match side {
            Side::Left | Side::Right if p.1 < c.y + CORNER_PX => Hit::Corner(id, side, Side::Top),
            Side::Left | Side::Right if p.1 >= c.y + c.h - CORNER_PX => {
                Hit::Corner(id, side, Side::Bottom)
            }
            Side::Top | Side::Bottom if p.0 < c.x + CORNER_PX => Hit::Corner(id, Side::Left, side),
            Side::Top | Side::Bottom if p.0 >= c.x + c.w - CORNER_PX => {
                Hit::Corner(id, Side::Right, side)
            }
            _ => hit,
        }
    }

    /// The end of `id`'s strip row that `p` is in, within `CORNER_PX` of the cells' side: a top
    /// corner, so the strip's ends resize as the bottom corners do.
    fn strip_corner(&self, id: WinId, p: (i32, i32)) -> Option<Side> {
        let (c, _) = self.frame(id).filter(|_| !self.is_maximized(id))?;
        match p {
            (_, y) if y >= c.y + STRIP => None,
            (x, _) if x < c.x + CORNER_PX => Some(Side::Left),
            (x, _) if x >= c.x + c.w - CORNER_PX => Some(Side::Right),
            _ => None,
        }
    }

    /// The strip-row px a top corner owns; `None` for a bottom corner.
    fn corner_strip(&self, id: WinId, x: Side, y: Side) -> Option<Rect> {
        let floating = self.tile(id)?.floating;
        let (c, r) = self
            .frame(id)
            .filter(|_| y == Side::Top && !self.is_maximized(id) && !floating)?;
        let strip = Rect { h: STRIP, ..c };
        Some(match x {
            Side::Left => Rect {
                x: r.x,
                w: c.x + CORNER_PX - r.x,
                ..strip
            },
            _ => Rect {
                x: c.x + c.w - CORNER_PX,
                w: r.x + r.w - (c.x + c.w - CORNER_PX),
                ..strip
            },
        })
    }

    /// The px of a zone, all inside the gaps around the tile's window.
    pub(super) fn zone(&self, hit: Hit) -> Option<Rect> {
        let t = self.tile(hit.id())?;
        let (c, r) = self.frame(t.id)?;
        Some(match hit {
            Hit::Corner(..) => return None, // The overlay lights its two edge zones instead.
            Hit::Strip(_) => Rect {
                x: r.x,
                w: r.w,
                h: r.y - c.y,
                ..c
            },
            Hit::Tab(id) => self.bar(id)?.tabs.into_iter().find(|&(u, _)| u == id)?.1,
            Hit::Button(id, b) => self.bar(id)?.buttons.into_iter().find(|&(u, _)| u == b)?.1,
            Hit::Edge(id, side) if t.floating => return self.float_zone(id, side),
            Hit::Edge(..) if self.maximized(t) => return None,
            Hit::Edge(_, Side::Left) => Rect { w: r.x - c.x, ..c },
            Hit::Edge(_, Side::Right) => Rect {
                x: r.x + r.w,
                w: c.x + c.w - r.x - r.w,
                ..c
            },
            Hit::Edge(_, Side::Bottom) => Rect {
                y: r.y + r.h,
                h: c.y + c.h - r.y - r.h,
                ..c
            },
            // The strip owns the gap on the monitor's top border.
            Hit::Edge(_, Side::Top) if t.cells.row == 0 => return None,
            Hit::Edge(_, Side::Top) => Rect {
                x: r.x,
                y: c.y - (GAP - STRIP),
                w: r.w,
                h: GAP - STRIP,
            },
        })
    }

    /// Puts the tiles still here back in their `before` cells. Returns whether any moved.
    fn restore(&mut self, before: &[Tile]) -> bool {
        let mut changed = false;
        for t in &mut self.tiles {
            if let Some(b) = before.iter().find(|b| b.id == t.id)
                && b.cells != t.cells
            {
                t.cells = b.cells;
                changed = true;
            }
        }
        changed
    }

    fn put(&mut self, ids: &[WinId], output: &str, cells: Cells) {
        for t in self.tiles.iter_mut().filter(|t| ids.contains(&t.id)) {
            t.output = output.to_string();
            t.cells = cells;
        }
    }

    /// The user put `ids` where they are: a returning output doesn't take them back.
    fn stay(&mut self, ids: &[WinId]) {
        for t in self.tiles.iter_mut().filter(|t| ids.contains(&t.id)) {
            t.home = None;
        }
    }

    pub(super) fn tiles_in(&self, ids: &[WinId]) -> Vec<Tile> {
        let has = |t: &&Tile| ids.contains(&t.id);
        self.tiles.iter().filter(has).cloned().collect()
    }

    /// Remembers where `tiles` are now, and their size too when `resized`.
    pub(super) fn learn(&mut self, tiles: &[Tile], resized: bool) {
        for t in tiles {
            let Some(o) = self.output(&t.output) else {
                continue;
            };
            let r = o.cells_rect(t.cells);
            self.clock += 1;
            let fresh = Memory {
                output: t.output.clone(),
                cells: t.cells,
                size: (r.w, r.h),
                stamp: self.clock,
            };
            let m = self.memory.entry(t.app.clone()).or_insert(fresh.clone());
            m.output = fresh.output;
            m.cells = fresh.cells;
            m.stamp = fresh.stamp;
            if resized {
                m.size = fresh.size;
            }
        }
    }

    /// Where a move of `moving` (`id`'s group, or `id` alone out of it), grabbed `grab` px from
    /// its cells' origin, lands with the pointer at `p`, on which output.
    fn move_target(
        &self,
        id: WinId,
        moving: &[WinId],
        p: (i32, i32),
        grab: (i32, i32),
    ) -> Option<(String, Target)> {
        let t = self.tile(id)?;
        let o = self
            .outputs
            .iter()
            .find(|o| o.area.contains(p))
            .or_else(|| self.output(&t.output))?;
        let group = self.group(id);
        let under = self
            .tiles
            .iter()
            .find(|u| u.output == o.name && !u.floating && o.cells_rect(u.cells).contains(p));
        if let Some(u) = under.filter(|u| !group.contains(&u.id)) {
            return Some((o.name.clone(), Target::Onto(u.id, u.cells)));
        }
        // The moving tiles' cells count as free.
        let rest = Layout {
            tiles: self
                .tiles
                .iter()
                .filter(|u| !moving.contains(&u.id))
                .cloned()
                .collect(),
            ..Layout::default()
        };
        let bar = match moving {
            [one] if *one == id && group.len() > 1 => t.decorated,
            _ => self.has_bar(id),
        };
        let Some((span, min)) = self
            .need(id, moving, bar, (MIN_W, MIN_H))
            .and_then(|n| n.spans(o))
        else {
            return Some((o.name.clone(), Target::Nowhere));
        };
        let want = candidate(o, span, p, grab);
        let own = (t.output == o.name).then_some(t.cells);
        // Under half a cell from the press, whatever the remembered size: no move.
        if own.is_some_and(|c| (c.col, c.row) == (want.col, want.row)) {
            return Some((o.name.clone(), Target::Own(t.cells)));
        }
        let centre = o.cells_rect(want).center();
        let cells = match rest.is_free(&o.name, want) {
            true => Some(want),
            false => rest
                .nearest_free(o, span, centre)
                .or_else(|| rest.smaller_free(o, span, min, centre)),
        };
        let target = match cells {
            Some(c) if Some(c) == own => Target::Own(c),
            Some(c) => Target::Free(c),
            None => Target::Nowhere,
        };
        Some((o.name.clone(), target))
    }

    /// Moves the dragged edge to the cell boundary nearest `p`, from the `before` snapshot, as far
    /// as every changed tile stays at least its floor, on the grid, and clear of other tiles.
    /// Returns whether any cells changed.
    fn resize_to(&mut self, r: &Resize, edge: &ResizeEdge, p: (i32, i32)) -> bool {
        let Some(o) = self.output(&r.output) else {
            return false;
        };
        let x = matches!(edge.side, Side::Left | Side::Right);
        let n = if x { COLS } else { ROWS };
        let floors: Vec<(WinId, i32)> = edge
            .changed
            .iter()
            .map(|&(id, _)| (id, span_of(self.floor(id), x)))
            .collect();
        let min = |id: WinId| floors.iter().find(|f| f.0 == id).map_or(n, |f| f.1);
        let off = |k: i32| {
            let (edge, at) = if x {
                (o.x_edge(k), p.0)
            } else {
                (o.y_edge(k), p.1)
            };
            (i64::from(edge) - i64::from(at)).abs()
        };
        let to = (0..=n).min_by_key(|&k| off(k)).unwrap_or(edge.from);
        let is_changed = |id: WinId| edge.changed.iter().any(|&(c, _)| c == id);
        let cells_at = |at: i32| -> Option<Vec<(WinId, Cells)>> {
            edge.changed
                .iter()
                .map(|&(id, far)| {
                    let b = r.before.iter().find(|t| t.id == id)?.cells;
                    let (start, len) = span(b, x);
                    let (start, len) = match far {
                        true => (start, at - start),
                        false => (at, start + len - at),
                    };
                    // Preserve the other axis, which a corner drag may have resized already.
                    let c = with_span(self.tile(id)?.cells, x, start, len);
                    let clear = !self
                        .tiles
                        .iter()
                        .any(|t| t.output == r.output && !is_changed(t.id) && t.cells.overlaps(&c));
                    (len >= min(id) && start >= 0 && start + len <= n && clear).then_some((id, c))
                })
                .collect()
        };
        let step = (to - edge.from).signum();
        let (mut at, mut cells) = (edge.from, cells_at(edge.from).unwrap_or_default());
        while at != to {
            let Some(next) = cells_at(at + step) else {
                break;
            };
            at += step;
            cells = next;
        }
        let mut changed = false;
        for (id, c) in cells {
            if let Some(t) = self.tiles.iter_mut().find(|t| t.id == id)
                && t.cells != c
            {
                t.cells = c;
                changed = true;
            }
        }
        changed
    }

    fn finish_resize(&mut self, before: &[Tile], what: &'static str) -> Option<Done> {
        let moved = |t: &&Tile| before.iter().any(|b| b.id == t.id && b.cells != t.cells);
        let ids: Vec<WinId> = self.tiles.iter().filter(moved).map(|t| t.id).collect();
        self.stay(&ids);
        let tiles = self.tiles_in(&ids);
        self.learn(&tiles, true);
        (!tiles.is_empty()).then_some(Done::Layout(what, tiles))
    }

    /// Expand only the clicked side, using the same collision and minimum-size rules as a drag.
    fn expand_edge(&mut self, hit: Hit) -> Option<Done> {
        let Hit::Edge(id, side) = hit else {
            return None;
        };
        if self.tile(id)?.floating {
            return self.float_fill(id, side);
        }
        let r = Resize::new(self, hit, (0, 0))?;
        let o = self.output(&r.output)?;
        let p = match side {
            Side::Left => (o.area.x, 0),
            Side::Right => (o.area.x + o.area.w, 0),
            Side::Top => (0, o.area.y),
            Side::Bottom => (0, o.area.y + o.area.h),
        };
        self.resize_to(&r, &r.edges[0], p);
        self.finish_resize(&r.before, "expanded")
    }

    /// Moves the focused group one cell, or grows or shrinks its `side` by one. Growing or moving
    /// takes the cell from every tile touching that side, or does nothing if one can't spare it.
    /// `Err` says why nothing changed.
    pub fn keyboard_adjust(
        &mut self,
        id: WinId,
        side: Side,
        op: Nudge,
    ) -> Result<Done, &'static str> {
        let t = self.tile(id).ok_or("no tile")?;
        if t.floating && self.visible(id) && !t.fullscreen {
            return self.float_nudge(id, side, op);
        }
        if !self.visible(id) || self.maximized(t) || t.fullscreen || t.floating {
            return Err("not a shown tile");
        }
        self.output(&t.output).ok_or("no output")?;
        let x = matches!(side, Side::Left | Side::Right);
        let far = matches!(side, Side::Right | Side::Bottom);
        let n = if x { COLS } else { ROWS };
        let min = |id: WinId| span_of(self.floor(id), x);
        let (output, before, group) = (t.output.clone(), t.cells, self.group(id));
        let (start, len) = span(before, x);
        let (start, len) = match (op, far) {
            (Nudge::Shrink, false) => (start + 1, len - 1),
            (Nudge::Shrink, true) => (start, len - 1),
            (Nudge::Grow, false) => (start - 1, len + 1),
            (Nudge::Grow, true) => (start, len + 1),
            (Nudge::Move, false) => (start - 1, len),
            (Nudge::Move, true) => (start + 1, len),
        };
        if op == Nudge::Shrink && len < min(id) {
            return Err("tile at minimum");
        }
        if start < 0 || start + len > n {
            return Err("monitor edge");
        }
        let mut changes = vec![(group.clone(), with_span(before, x, start, len))];
        if op != Nudge::Shrink {
            // The one cell just outside `side`, across the group's span on the other axis.
            let edge = if far { start + len - 1 } else { start };
            let strip = with_span(before, x, edge, 1);
            for u in self.tiles.iter().filter(|u| {
                u.output == output && !group.contains(&u.id) && u.cells.overlaps(&strip)
            }) {
                let (s, l) = span(u.cells, x);
                if l - 1 < min(u.id) {
                    return Err("neighbour at minimum");
                }
                let s = if far { s + 1 } else { s };
                changes.push((vec![u.id], with_span(u.cells, x, s, l - 1)));
            }
        }
        let mut ids = Vec::new();
        for (who, cells) in changes {
            self.put(&who, &output, cells);
            ids.extend(who);
        }
        self.stay(&group);
        self.learn(&self.tiles_in(&group), op != Nudge::Move);
        let what = match op {
            Nudge::Move => "moved",
            Nudge::Grow => "expanded",
            Nudge::Shrink => "shrunk",
        };
        Ok(Done::Layout(what, self.tiles_in(&ids)))
    }
}

/// Only two stationary left clicks on the same edge form a double click.
#[derive(Default)]
pub struct EdgeClicks(Option<(Hit, (i32, i32), u64)>);

impl EdgeClicks {
    pub fn motion(&mut self, p: (i32, i32)) {
        if self
            .0
            .is_some_and(|(_, from, _)| dist2(from, p) >= ARM_PX * ARM_PX)
        {
            self.0 = None;
        }
    }

    pub fn release(
        &mut self,
        l: &mut Layout,
        hit: Option<Hit>,
        p: (i32, i32),
        now: u64,
    ) -> Option<Done> {
        let previous = self.0.take();
        let hit = hit?;
        if previous.is_some_and(|(h, from, time)| {
            h == hit
                && now.saturating_sub(time) <= DOUBLE_CLICK_MS
                && dist2(from, p) < ARM_PX * ARM_PX
        }) {
            l.expand_edge(hit)
        } else {
            self.0 = Some((hit, p, now));
            None
        }
    }
}

/// `(start, len)` of `c` along x, or along y.
fn span(c: Cells, x: bool) -> (i32, i32) {
    match x {
        true => (c.col, c.cols),
        false => (c.row, c.rows),
    }
}

/// The cols, or the rows, of a span.
fn span_of((cols, rows): (i32, i32), x: bool) -> i32 {
    if x { cols } else { rows }
}

fn with_span(c: Cells, x: bool, start: i32, len: i32) -> Cells {
    match x {
        true => Cells {
            col: start,
            cols: len,
            ..c
        },
        false => Cells {
            row: start,
            rows: len,
            ..c
        },
    }
}

/// The `cols` x `rows` cells on `o` whose origin is the cell boundary nearest `p - grab`: the
/// grabbed point stays under the pointer, a cell at a time, clamped to the grid.
fn candidate(o: &Output, (cols, rows): (i32, i32), p: (i32, i32), grab: (i32, i32)) -> Cells {
    Cells {
        col: nearest_start(|k| o.x_edge(k), COLS, cols, p.0, grab.0),
        row: nearest_start(|k| o.y_edge(k), ROWS, rows, p.1, grab.1),
        cols,
        rows,
    }
}

/// The start in `0..=n - span` whose boundary is nearest `at - grab`. A `grab` beyond the span
/// (a smaller remembered size) is clamped into it, and then starts that keep `at` inside win.
fn nearest_start(edge: impl Fn(i32) -> i32, n: i32, span: i32, at: i32, grab: i32) -> i32 {
    let e = |k| i64::from(edge(k));
    let (at, grab) = (i64::from(at), i64::from(grab));
    let inside = grab.clamp(0, (e(span) - e(0) - 1).max(0));
    let want = at - inside;
    (0..=n - span)
        .min_by_key(|&k| {
            let off = inside != grab && !(e(k) <= at && at < e(k + span));
            (off, (e(k) - want).abs())
        })
        .unwrap_or(0)
}

/// Whether the pointer has been in a tile's middle long enough for the drop to tab.
fn held(middle: Option<(WinId, u64)>, now_ms: u64) -> bool {
    middle.is_some_and(|(_, since)| now_ms.saturating_sub(since) >= HOLD_MS)
}

/// The inner half of `r` in both axes.
fn middle(r: Rect) -> Rect {
    Rect {
        x: r.x + r.w / 4,
        y: r.y + r.h / 4,
        w: r.w / 2,
        h: r.h / 2,
    }
}

fn accent(alpha: f32) -> [f32; 4] {
    rgba(ACCENT, alpha)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    /// Over the group's own cells: the drop cancels.
    Own(Cells),
    /// Over another tile, with its cells.
    Onto(WinId, Cells),
    Free(Cells),
    /// No room: the drop cancels.
    Nowhere,
}

impl Target {
    fn cells(self) -> Option<Cells> {
        match self {
            Target::Own(c) | Target::Onto(_, c) | Target::Free(c) => Some(c),
            Target::Nowhere => None,
        }
    }
}

struct Resize {
    hit: Hit,
    output: String,
    press: (i32, i32),
    dragged: bool,
    before: Vec<Tile>,
    edges: Vec<ResizeEdge>,
}

struct ResizeEdge {
    side: Side,
    /// The edge's cell boundary at the press.
    from: i32,
    /// The tiles that move with the edge, and whether it is their far side (right or bottom).
    changed: Vec<(WinId, bool)>,
}

impl Resize {
    fn new(l: &Layout, hit: Hit, press: (i32, i32)) -> Option<Self> {
        let t = l.tile(hit.id())?;
        let (sides, corner) = match hit {
            Hit::Edge(_, side) => (vec![side], None),
            Hit::Corner(_, x, y) => (
                vec![x, y],
                Some((
                    if x == Side::Left {
                        t.cells.col
                    } else {
                        t.cells.col + t.cells.cols
                    },
                    if y == Side::Top {
                        t.cells.row
                    } else {
                        t.cells.row + t.cells.rows
                    },
                )),
            ),
            _ => return None,
        };
        let edges = sides
            .into_iter()
            .map(|side| {
                let x = matches!(side, Side::Left | Side::Right);
                let far = matches!(side, Side::Right | Side::Bottom);
                let (start, len) = span(t.cells, x);
                let from = if far { start + len } else { start };
                let (a, alen) = span(t.cells, !x);
                let changed = l
                    .tiles
                    .iter()
                    .filter(|u| u.output == t.output)
                    .filter_map(|u| {
                        if u.cells == t.cells {
                            return Some((u.id, far));
                        }
                        let (s, n) = span(u.cells, x);
                        let (b, blen) = span(u.cells, !x);
                        // At a junction, include every tile meeting the corner on this boundary.
                        let meets_corner = corner.is_some_and(|(cx, cy)| {
                            let at = if x { cy } else { cx };
                            b <= at && at <= b + blen
                        });
                        if meets_corner && (s == from || s + n == from) {
                            return Some((u.id, s + n == from));
                        }
                        let near = if far { s } else { s + n };
                        (near == from && a < b + blen && b < a + alen).then_some((u.id, !far))
                    })
                    .collect();
                ResizeEdge {
                    side,
                    from,
                    changed,
                }
            })
            .collect();
        Some(Self {
            hit,
            output: t.output.clone(),
            press,
            dragged: false,
            before: l.tiles.clone(),
            edges,
        })
    }

    fn motion(&mut self, l: &mut Layout, p: (i32, i32)) -> bool {
        self.dragged |= dist2(self.press, p) >= ARM_PX * ARM_PX;
        let before: Vec<Cells> = l.tiles.iter().map(|t| t.cells).collect();
        l.restore(&self.before);
        for edge in &self.edges {
            l.resize_to(self, edge, p);
        }
        l.tiles.iter().map(|t| t.cells).ne(before)
    }
}

enum State {
    /// A press on a strip, tab or button: a click until the pointer travels `ARM_PX`.
    Armed {
        hit: Hit,
        from: (i32, i32),
    },
    Move {
        id: WinId,
        /// A tab dragged out of its group, alone.
        tab: bool,
        /// The press point minus the group's cells origin, kept under the pointer.
        grab: (i32, i32),
        output: String,
        target: Target,
        /// The tile whose middle the pointer is in, and since when.
        middle: Option<(WinId, u64)>,
    },
    Resize(Resize),
    Float(FloatDrag),
    Cancelled,
}

/// A left-button drag that started in a navi zone. Lives from press to release.
pub struct Drag(State);

impl Drag {
    /// `press_on` the zone at `p`, `None` without one.
    #[cfg(test)]
    pub fn press(l: &Layout, p: (i32, i32)) -> Option<Drag> {
        Drag::press_on(l, l.hit(p)?, p)
    }

    /// A left press at `p` on zone `hit` (as `Navi::zone_at` found it): arms a click or move on
    /// a strip, tab or button, starts a resize on an edge or corner.
    pub fn press_on(l: &Layout, hit: Hit, p: (i32, i32)) -> Option<Drag> {
        let state = match hit {
            hit @ (Hit::Strip(_) | Hit::Tab(_) | Hit::Button(..)) => State::Armed { hit, from: p },
            hit if l.tile(hit.id())?.floating => State::Float(FloatDrag::new(l, hit, p, false)?),
            hit @ (Hit::Edge(..) | Hit::Corner(..)) => State::Resize(Resize::new(l, hit, p)?),
        };
        Some(Drag(state))
    }

    /// The cursor for the whole drag, wherever the pointer is: the pressed zone's.
    pub fn cursor(&self) -> &'static str {
        match &self.0 {
            State::Armed { hit, .. } => hit.pressed(),
            State::Move { .. } => "grabbing",
            State::Resize(r) => r.hit.cursor(),
            State::Float(f) => f.hit.pressed(),
            State::Cancelled => "default",
        }
    }

    /// A stationary edge press/release; moving away and back must still count as a drag.
    pub fn edge_click(&self, l: &Layout, p: (i32, i32)) -> Option<Hit> {
        match &self.0 {
            State::Resize(r)
                if matches!(r.hit, Hit::Edge(..))
                    && !r.dragged
                    && dist2(r.press, p) < ARM_PX * ARM_PX
                    && l.hit(p) == Some(r.hit) =>
            {
                Some(r.hit)
            }
            State::Float(f)
                if matches!(f.hit, Hit::Edge(..))
                    && !f.dragged
                    && dist2(f.press, p) < ARM_PX * ARM_PX
                    && l.hit(p) == Some(f.hit) =>
            {
                Some(f.hit)
            }
            _ => None,
        }
    }

    /// Follows the pointer to `p`. Returns whether any tile's cells changed.
    pub fn motion(&mut self, l: &mut Layout, p: (i32, i32), now_ms: u64) -> bool {
        if let State::Armed { hit, from } = self.0
            && dist2(from, p) >= ARM_PX * ARM_PX
        {
            let (id, tab) = match hit {
                Hit::Strip(id) => (id, false),
                Hit::Tab(id) => (id, true),
                _ => (hit.id(), false),
            };
            let grab = l.frame(id).map_or((0, 0), |(c, _)| {
                (from.0.saturating_sub(c.x), from.1.saturating_sub(c.y))
            });
            self.0 = match hit {
                // A maximized group doesn't move; a button press stays a click.
                Hit::Strip(_) | Hit::Tab(_) if l.is_maximized(id) => State::Cancelled,
                Hit::Strip(_) if l.tile(id).is_some_and(|t| t.floating) => {
                    FloatDrag::new(l, hit, from, true).map_or(State::Cancelled, State::Float)
                }
                Hit::Strip(_) | Hit::Tab(_) => State::Move {
                    id,
                    tab,
                    grab,
                    output: String::new(),
                    target: Target::Nowhere,
                    middle: None,
                },
                _ => State::Armed { hit, from },
            };
        }
        match &mut self.0 {
            State::Armed { hit, .. } if l.tile(hit.id()).is_none() => self.0 = State::Cancelled,
            State::Move {
                id,
                tab,
                grab,
                output,
                target,
                middle,
            } => match l.move_target(*id, &l.moving(*id, *tab), p, *grab) {
                Some((o, t)) => {
                    let mid = match t {
                        Target::Onto(u, c) => l
                            .output(&o)
                            .is_some_and(|o| self::middle(o.cells_rect(c)).contains(p))
                            .then_some(u),
                        _ => None,
                    };
                    *middle = match *middle {
                        Some((u, since)) if mid == Some(u) => Some((u, since)),
                        _ => mid.map(|u| (u, now_ms)),
                    };
                    *output = o;
                    *target = t;
                }
                None => self.0 = State::Cancelled,
            },
            State::Resize(r) if l.tile(r.hit.id()).is_none() => {
                // Undo, so a window closing mid-drag leaves the rest as they were.
                let changed = l.restore(&r.before);
                self.0 = State::Cancelled;
                return changed;
            }
            State::Resize(r) => return r.motion(l, p),
            // Gone, or tiled by its hints mid-drag.
            State::Float(f) if !l.tile(f.id()).is_some_and(|t| t.floating) => {
                self.0 = State::Cancelled;
            }
            State::Float(f) => return f.motion(l, p),
            _ => {}
        }
        false
    }

    /// Ends the drag without a drop: a resize puts every tile back. Returns whether cells changed.
    pub fn cancel(self, l: &mut Layout) -> bool {
        match self.0 {
            State::Resize(r) => l.restore(&r.before),
            State::Float(f) => f.cancel(l),
            _ => false,
        }
    }

    /// Ends the drag at `p`. Returns what happened, `None` if nothing did.
    pub fn release(mut self, l: &mut Layout, p: (i32, i32), now_ms: u64) -> Option<Done> {
        self.motion(l, p, now_ms);
        match self.0 {
            State::Move {
                id,
                tab,
                output,
                target,
                middle,
                ..
            } => {
                let moving = l.moving(id, tab);
                let held = held(middle, now_ms);
                let (what, ids) = match target {
                    Target::Own(_) | Target::Nowhere => return None,
                    Target::Free(c) => {
                        l.put(&moving, &output, c);
                        (if tab { "untabbed" } else { "moved" }, moving)
                    }
                    // Every window must still fit, below the bar a tab group has.
                    Target::Onto(u, c) if held => {
                        let o = l.output(&output)?;
                        if !l.fit_in(&moving, o, c, true) || !l.fit_in(&l.group(u), o, c, true) {
                            return None;
                        }
                        l.put(&moving, &output, c);
                        ("tabbed", moving)
                    }
                    // The tabs left behind would overlap the swapped tile.
                    Target::Onto(..) if tab => return None,
                    Target::Onto(u, c) => {
                        let theirs = l.group(u);
                        let (own_output, own) = l.tile(id).map(|t| (t.output.clone(), t.cells))?;
                        let (o, back) = (l.output(&output)?, l.output(&own_output)?);
                        if !l.fit_in(&moving, o, c, l.has_bar(id))
                            || !l.fit_in(&theirs, back, own, l.has_bar(u))
                        {
                            return None;
                        }
                        l.put(&moving, &output, c);
                        l.put(&theirs, &own_output, own);
                        ("swapped", [moving, theirs].concat())
                    }
                };
                // What lands is shown, and a new tile there must be visible.
                l.clear_maximized(&output);
                l.touch(id);
                l.stay(&ids);
                let tiles = l.tiles_in(&ids);
                l.learn(&tiles, false);
                Some(Done::Layout(what, tiles))
            }
            State::Resize(r) => l.finish_resize(&r.before, "resized"),
            State::Float(f) => f.finish(l),
            State::Armed { hit, .. } => l.click(hit, p).map(|(a, id)| Done::Click(a, id)),
            State::Cancelled => None,
        }
    }
}

impl Layout {
    /// What a move from `id`'s strip or tab carries.
    fn moving(&self, id: WinId, tab: bool) -> Vec<WinId> {
        match tab {
            true => vec![id],
            false => self.group(id),
        }
    }

    /// A press and release on `hit` with the pointer ending at `p`.
    fn click(&mut self, hit: Hit, p: (i32, i32)) -> Option<(Action, WinId)> {
        let done = |a: Action, changed: bool| changed.then_some((a, hit.id()));
        match hit {
            Hit::Tab(id) => done(Action::Activate, self.activate(id)),
            Hit::Strip(id) if self.tile(id)?.minimized => done(Action::Restore, self.activate(id)),
            // So navi can always undo a maximize, even with no maximize button.
            Hit::Strip(id) => done(Action::Unmaximize, self.unmaximize(id)),
            Hit::Button(..) if self.hit(p) != Some(hit) => None,
            Hit::Button(id, Button::Minimize) => done(Action::Minimize, self.minimize(id)),
            Hit::Button(id, Button::Maximize) if self.is_maximized(id) => {
                done(Action::Unmaximize, self.unmaximize(id))
            }
            Hit::Button(id, Button::Maximize) => done(Action::Maximize, self.maximize(id)),
            Hit::Button(_, Button::Close) => done(Action::Close, true),
            Hit::Edge(..) | Hit::Corner(..) => None,
        }
    }
}

/// A solid rect of a tile's or float's chrome: whose group (its active tile), global logical px,
/// premultiplied RGBA, and what it is cut to.
type Chrome = (WinId, Rect, [f32; 4], Option<Cut>);
/// `Chrome` tagged `None` where it goes above every tile-layer window.
pub type Drawn = (Option<WinId>, Rect, [f32; 4], Option<Cut>);

/// The tiles' solid rects, front to back, each tagged with its group; `None` for what goes above
/// every tile-layer window. During a move, the grid and a preview of the landing tile (`None`),
/// more solid once the drop would tab; otherwise the hovered or pressed zone. Then every bar, the
/// dimmed strip of each minimized untabbed tile, and last a frame around each shown window (and
/// its bar): bright around `focused`, dim around the rest. `under` is the zone at the pointer
/// (`Navi::zone_at`).
pub fn overlay(
    l: &Layout,
    drag: Option<&Drag>,
    under: Option<Hit>,
    now_ms: u64,
    focused: Option<WinId>,
) -> Vec<Drawn> {
    let mut out = Vec::new();
    let hover = match drag.map(|d| &d.0) {
        None => under,
        Some(State::Armed { hit, .. }) => Some(*hit),
        Some(State::Resize(r)) => Some(r.hit),
        Some(State::Float(_)) => None,
        Some(State::Move {
            id,
            tab,
            output,
            target,
            middle,
            ..
        }) => {
            let held = held(*middle, now_ms);
            // The preview is where the window lands: below a bar if it will have one.
            let bar = match target {
                Target::Onto(..) if held => true,
                _ if *tab => l.tile(*id).is_some_and(|t| t.decorated),
                _ => l.has_bar(*id),
            };
            let top = STRIP + if bar { super::BAR } else { 0 };
            let alpha = if held { TAB_ALPHA } else { PREVIEW_ALPHA };
            // A tab can't swap, so over a tile only a hold that will tab shows where it lands.
            let target = match target {
                Target::Onto(..) if *tab && !held => Target::Nowhere,
                t => *t,
            };
            if let Some(o) = l.output(output) {
                out.extend(
                    move_overlay(o, target, top, alpha)
                        .into_iter()
                        .map(|(r, k, cut)| (None, r, k, cut)),
                );
            }
            None
        }
        Some(State::Cancelled) => None,
    };
    // A float's zones light in its own layer.
    let hover = hover.filter(|h| !l.tile(h.id()).is_some_and(|t| t.floating));
    let mut tiles = Vec::new();
    light(l, hover, &mut tiles);
    decorations(l, |t| !t.floating, hover, focused, &mut tiles);
    out.extend(
        tiles
            .into_iter()
            .map(|(id, r, k, cut)| (Some(id), r, k, cut)),
    );
    out
}

/// The hovered or pressed zone: a strip up to its buttons, an edge, or a corner's two edges and
/// strip end.
fn light(l: &Layout, hover: Option<Hit>, out: &mut Vec<Chrome>) {
    let lit = match hover {
        Some(Hit::Strip(id)) => l.zone(Hit::Strip(id)).map(|z| {
            let buttons = l.bar(id).map_or(0, |b| b.buttons.len() as i32);
            Rect {
                w: z.w - buttons * super::BAR,
                ..z
            }
        }),
        Some(h @ Hit::Edge(..)) => l.zone(h),
        _ => None,
    };
    let Some(id) = hover.map(Hit::id) else {
        return;
    };
    // A lit strip runs over the bar, so it is cut as the bar is.
    let cut = match hover {
        Some(Hit::Strip(_)) => l.shape(id).map(Cut::solid),
        _ => None,
    };
    out.extend(lit.map(|z| (id, z, accent(1.0), cut)));
    if let Some(Hit::Corner(_, x, y)) = hover {
        for side in [x, y] {
            out.extend(
                l.zone(Hit::Edge(id, side))
                    .map(|z| (id, z, accent(1.0), None)),
            );
        }
        out.extend(l.corner_strip(id, x, y).map(|z| (id, z, accent(1.0), None)));
    }
}

/// What `overlay` draws for floats, which go above every tile and its decorations, by float,
/// front float first (`Layout::floats`): its hovered or pressed zone, then its bar and frame.
/// Each float's go right above its own window, so a float in front covers them.
pub fn float_overlay(
    l: &Layout,
    drag: Option<&Drag>,
    under: Option<Hit>,
    focused: Option<WinId>,
) -> Vec<Chrome> {
    let mut out = Vec::new();
    let hover = match drag.map(|d| &d.0) {
        None => under,
        Some(State::Armed { hit, .. }) => Some(*hit),
        Some(State::Float(f)) => Some(f.hit),
        Some(_) => None,
    };
    let hover = hover.filter(|h| l.tile(h.id()).is_some_and(|t| t.floating));
    for id in l.floats() {
        light(l, hover.filter(|h| h.id() == id), &mut out);
        decorations(l, |t| t.id == id, hover, focused, &mut out);
    }
    out
}

/// Appends the bars, minimized strips and frames of the tiles, or of the floats.
fn decorations(
    l: &Layout,
    which: impl Fn(&Tile) -> bool,
    hover: Option<Hit>,
    focused: Option<WinId>,
    out: &mut Vec<Chrome>,
) {
    let tiles = || l.tiles.iter().filter(|t| which(t));
    // Where a frame's top goes when it runs along the top of a bar, in front of it.
    let front = out.len();
    for t in tiles() {
        // A minimized float is hidden whole.
        if l.active(t.id) != Some(t.id) || l.covered(t) || t.fullscreen || t.floating && t.minimized
        {
            continue;
        }
        let tabbed = l.group(t.id).len() > 1;
        if t.minimized && !tabbed {
            let z = l
                .bar(t.id)
                .map(|b| b.rect)
                .or_else(|| l.zone(Hit::Strip(t.id)));
            let cut = z.map(|z| Cut::solid(Shape::new(z, RADIUS)));
            out.extend(z.map(|z| (t.id, z, rgba(MINIMIZED, 1.0), cut)));
        } else if let Some(b) = l.bar(t.id) {
            let cut = l.shape(t.id).map(Cut::solid);
            out.extend(
                bar::draw(l, t.id, &b, hover)
                    .into_iter()
                    .map(|(r, k)| (t.id, r, k, cut)),
            );
        }
    }
    for t in tiles() {
        let Some(r) = l.rect(t.id).filter(|_| l.visible(t.id) && !t.fullscreen) else {
            continue;
        };
        let (px, alpha) = match focused == Some(t.id) {
            true => (FOCUS_PX, 1.0),
            false => (IDLE_PX, IDLE_ALPHA),
        };
        let k = accent(alpha);
        // A bar is the window's top edge, so the frame takes it in: its top line runs along
        // the bar's top rows, in front of the bar.
        match l.bar(t.id) {
            Some(b) => {
                let r = Rect {
                    y: b.rect.y + px,
                    h: r.y + r.h - b.rect.y - px,
                    ..r
                };
                let cut = Some(Cut::ring(r, RADIUS, px));
                let [top, rest @ ..] = frame(r, px);
                out.insert(front, (t.id, top, k, cut));
                out.extend(rest.map(|f| (t.id, f, k, cut)));
            }
            None => {
                let cut = Some(Cut::ring(r, RADIUS, px));
                out.extend(frame(r, px).map(|f| (t.id, f, k, cut)));
            }
        }
    }
}

/// Every shown bar's window buttons, and whether each is lit: the zone `under` the pointer, or
/// pressed.
pub fn buttons(
    l: &Layout,
    drag: Option<&Drag>,
    under: Option<Hit>,
) -> Vec<(WinId, Button, Rect, bool)> {
    let hover = match drag.map(|d| &d.0) {
        None => under,
        Some(State::Armed { hit, .. }) => Some(*hit),
        Some(_) => None,
    };
    l.tiles
        .iter()
        .filter(|t| l.active(t.id) == Some(t.id) && !l.covered(t) && !t.fullscreen)
        .filter_map(|t| Some((t.id, l.bar(t.id)?)))
        .flat_map(|(id, bar)| {
            bar.buttons
                .into_iter()
                .map(move |(b, r)| (id, b, r, hover == Some(Hit::Button(id, b))))
        })
        .collect()
}

/// The tile layer, top first. `chrome` is `overlay`'s and the buttons' (a group's, or `None`
/// for the drag grid and preview), `windows` the tile layer's windows top first, each tagged
/// with the group it is the active window of. The grid and preview go above every window, a
/// group's chrome right above its window, and chrome whose window isn't drawn (a minimized
/// strip) below them all.
pub fn tile_stack<T>(
    chrome: Vec<(Option<WinId>, T)>,
    windows: Vec<(Option<WinId>, Vec<T>)>,
) -> Vec<T> {
    let (top, mut rest): (Vec<_>, Vec<_>) = chrome.into_iter().partition(|(g, _)| g.is_none());
    let mut out: Vec<T> = top.into_iter().map(|(_, c)| c).collect();
    for (group, own) in windows {
        if group.is_some() {
            let (mine, other) = std::mem::take(&mut rest)
                .into_iter()
                .partition(|(g, _)| *g == group);
            rest = other;
            out.extend(mine.into_iter().map(|(_, c)| c));
        }
        out.extend(own);
    }
    out.extend(rest.into_iter().map(|(_, c)| c));
    out
}

/// A dialog's frame around its rect `r`, as a tile's: bright while it has the keyboard, else dim;
/// full accent at 1 px while it `waiting` for the user (it was refused the keyboard).
pub fn dialog_frame(r: Rect, focused: bool, waiting: bool) -> [(Rect, [f32; 4]); 4] {
    let (px, alpha) = match (focused, waiting) {
        (true, _) => (FOCUS_PX, 1.0),
        (false, true) => (IDLE_PX, 1.0),
        (false, false) => (IDLE_PX, IDLE_ALPHA),
    };
    frame(r, px).map(|f| (f, accent(alpha)))
}

/// `px` wide rects around `r`, just outside it: top, bottom, left, right.
fn frame(r: Rect, px: i32) -> [Rect; 4] {
    let (x, w) = (r.x - px, r.w + 2 * px);
    [
        Rect {
            x,
            y: r.y - px,
            w,
            h: px,
        },
        Rect {
            x,
            y: r.y + r.h,
            w,
            h: px,
        },
        Rect { x, w: px, ..r },
        Rect {
            x: r.x + r.w,
            w: px,
            ..r
        },
    ]
}

/// The grid's strength for a smallest cell side of `cell` logical px.
fn grid_alpha(cell: i32) -> f32 {
    let scale = cell.clamp(0, GRID_FULL_PX) as f32 / GRID_FULL_PX as f32;
    (GRID_ALPHA * scale).max(GRID_MIN_ALPHA)
}

/// The output's interior cell boundaries, 1 px wide, and the preview where the window would go,
/// rounded by its own rect.
fn move_overlay(
    o: &Output,
    target: Target,
    top: i32,
    alpha: f32,
) -> Vec<(Rect, [f32; 4], Option<Cut>)> {
    let a = o.area;
    let grid = accent(grid_alpha(o.cell_w().min(o.cell_h())));
    let cols = (1..COLS).map(|k| Rect {
        x: o.x_edge(k),
        w: 1,
        ..a
    });
    let rows = (1..ROWS).map(|k| Rect {
        y: o.y_edge(k),
        h: 1,
        ..a
    });
    let preview = |r: Rect| (r, accent(alpha), Some(Cut::solid(Shape::new(r, RADIUS))));
    cols.chain(rows)
        .map(|r| (r, grid, None))
        .chain(target.cells().map(|c| preview(o.window_rect(c, top))))
        .collect()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// `overlay` with the pointer at `p` and no window there, untagged.
    pub(in crate::layout) fn overlay_at(
        l: &Layout,
        drag: Option<&Drag>,
        p: (i32, i32),
        now_ms: u64,
        focused: Option<WinId>,
    ) -> Vec<(Rect, [f32; 4])> {
        let out = overlay(l, drag, l.hit(p), now_ms, focused);
        out.into_iter().map(|(_, r, k, _)| (r, k)).collect()
    }

    /// The overlay with no focus and without the dim frames every shown tile has.
    pub(in crate::layout) fn zones(
        l: &Layout,
        drag: Option<&Drag>,
        p: (i32, i32),
        now_ms: u64,
    ) -> Vec<(Rect, [f32; 4])> {
        let mut out = overlay_at(l, drag, p, now_ms, None);
        out.retain(|&(_, k)| k != accent(IDLE_ALPHA));
        out
    }

    pub(in crate::layout) fn c(col: i32, row: i32, cols: i32, rows: i32) -> Cells {
        Cells {
            col,
            row,
            cols,
            rows,
        }
    }

    pub(in crate::layout) fn uw() -> Output {
        Output {
            name: "uw".into(),
            area: Rect {
                x: 0,
                y: 0,
                w: 3440,
                h: 1440,
            },
            full: Rect {
                x: 0,
                y: 0,
                w: 3440,
                h: 1440,
            },
            priority: 0,
        }
    }

    /// The ultrawide with tiles 0, 1, 2… of apps "a0", "a1"…, each remembering its own size.
    pub(in crate::layout) fn with(tiles: &[Cells]) -> Layout {
        let mut l = Layout::default();
        l.set_outputs(vec![uw()]);
        for (i, &cells) in tiles.iter().enumerate() {
            let app = format!("a{i}");
            let r = uw().cells_rect(cells);
            l.memory.insert(
                app.clone(),
                Memory {
                    output: "uw".into(),
                    cells,
                    size: (r.w, r.h),
                    stamp: i as u64,
                },
            );
            l.tiles.push(Tile {
                id: i as WinId,
                app,
                output: "uw".into(),
                cells,
                stamp: i as u64,
                ..Tile::default()
            });
        }
        l.clock = tiles.len() as u64;
        l.next_id = tiles.len() as WinId;
        l
    }

    /// What a release did, as it is logged.
    fn what(done: Option<Done>) -> Option<&'static str> {
        done.map(|d| match d {
            Done::Layout(what, _) => what,
            Done::Click(a, _) => a.log(),
        })
    }

    pub(in crate::layout) fn at(l: &Layout, id: WinId) -> Cells {
        l.tile(id).unwrap().cells
    }

    /// AC11: tiles overlap only as tab groups, stay on the grid and at least the minimum.
    pub(in crate::layout) fn assert_valid(l: &Layout) {
        let (min_c, min_r) = uw().span_for((MIN_W, MIN_H));
        for (i, t) in l.tiles.iter().enumerate().filter(|(_, t)| !t.floating) {
            let k = t.cells;
            if t.hints != Default::default() {
                let (cols, rows) = l.floor(t.id);
                assert!(
                    k.cols >= cols && k.rows >= rows,
                    "{k:?} below its client minimum"
                );
            }
            assert!(k.col >= 0 && k.row >= 0, "{k:?}");
            assert!(k.col + k.cols <= COLS && k.row + k.rows <= ROWS, "{k:?}");
            assert!(k.cols >= min_c && k.rows >= min_r, "{k:?} below minimum");
            for u in l.tiles[i + 1..].iter().filter(|u| !u.floating) {
                let same = u.output == t.output && u.cells == t.cells;
                assert!(
                    same || !k.overlaps(&u.cells),
                    "{k:?} overlaps {:?}",
                    u.cells
                );
            }
        }
    }

    /// Presses at `from`, moves through `path` (point, ms), releases at the last point.
    pub(in crate::layout) fn drag(
        l: &mut Layout,
        from: (i32, i32),
        path: &[((i32, i32), u64)],
    ) -> Option<&'static str> {
        let mut d = Drag::press(l, from).expect("press in a zone");
        for &(p, t) in path {
            d.motion(l, p, t);
            assert_valid(l);
        }
        let &(p, t) = path.last().unwrap_or(&(from, 0));
        let what = what(d.release(l, p, t));
        assert_valid(l);
        what
    }

    fn preview(d: &Drag) -> Option<Cells> {
        match &d.0 {
            State::Move { target, .. } => target.cells(),
            _ => None,
        }
    }

    // Points on the ultrawide. Cells are 143 or 144 x 120 px; a is cols 0..12, b cols 12..24.
    /// Job 028 AC2: a group's chrome draws right above its window, at the window's place in the
    /// space; the drag grid and preview above every window, a minimized strip below them all.
    #[test]
    fn tile_chrome_stacks_with_its_window() {
        let (a, b, m) = (0, 1, 2);
        let chrome = || {
            let chrome = [(None, "preview"), (Some(a), "A's chrome")];
            chrome
                .into_iter()
                .chain([(Some(m), "strip"), (Some(b), "B's chrome")])
        };
        let win = |name, group| (group, vec![name]);
        let stack = |windows| tile_stack(chrome().collect(), windows);
        assert_eq!(
            stack(vec![
                win("dialog", None),
                win("B", Some(b)),
                win("A", Some(a))
            ]),
            [
                "preview",
                "dialog",
                "B's chrome",
                "B",
                "A's chrome",
                "A",
                "strip"
            ]
        );
        assert_eq!(
            stack(vec![
                win("B", Some(b)),
                win("dialog", None),
                win("A", Some(a))
            ]),
            [
                "preview",
                "B's chrome",
                "B",
                "dialog",
                "A's chrome",
                "A",
                "strip"
            ]
        );

        // `overlay` tags the grid and preview `None`, a minimized strip and every bar, light and
        // frame with their own group.
        let mut l = with(&[c(0, 0, 8, 12), c(8, 0, 8, 12), c(16, 0, 8, 12)]);
        l.set_decorated(b, true);
        assert!(l.minimize(m));
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        d.motion(&mut l, (A_STRIP.0 + 40, A_STRIP.1 + 300), 0);
        assert!(matches!(d.0, State::Move { .. }));
        let o = overlay(&l, Some(&d), None, 0, Some(a));
        let preview = (None, accent(PREVIEW_ALPHA));
        assert!(o.iter().any(|&(g, _, k, _)| (g, k) == preview));
        let strip = l
            .bar(m)
            .map_or_else(|| l.zone(Hit::Strip(m)).unwrap(), |b| b.rect);
        let cut = Some(Cut::solid(Shape::new(strip, RADIUS)));
        assert!(o.contains(&(Some(m), strip, rgba(MINIMIZED, 1.0), cut)));
        let tagged = |id| o.iter().filter(|(g, ..)| *g == Some(id)).count();
        assert!(tagged(a) >= 4 && tagged(b) >= 5, "{o:?}");
        let tops = o.iter().position(|(g, ..)| g.is_some()).unwrap();
        assert!(
            o[tops..].iter().all(|(g, ..)| g.is_some()),
            "grid and preview first"
        );
    }

    const A_STRIP: (i32, i32) = (100, 2);
    const B_MIDDLE: (i32, i32) = (2500, 700);
    const B_RIM: (i32, i32) = (1800, 100);

    fn halves() -> Layout {
        with(&[c(0, 0, 12, 12), c(12, 0, 12, 12)])
    }

    #[test]
    fn ac1_hit_zones_are_gap_px() {
        let l = halves();
        // a's window spans x 6..1714, y 8..1436; b's starts at x 1726.
        assert_eq!(l.hit(A_STRIP), Some(Hit::Strip(0)));
        assert_eq!(l.hit((1687, 7)), Some(Hit::Strip(0)));
        assert_eq!(
            l.hit((1713, 7)),
            Some(Hit::Corner(0, Side::Right, Side::Top))
        );
        assert_eq!(l.hit((1720, 700)), Some(Hit::Edge(1, Side::Left)));
        assert_eq!(l.hit((1725, 700)), Some(Hit::Edge(1, Side::Left)));
        assert_eq!(l.hit((1714, 700)), Some(Hit::Edge(0, Side::Right)));
        assert_eq!(l.hit((1719, 700)), Some(Hit::Edge(0, Side::Right)));
        assert_eq!(l.hit((500, 1436)), Some(Hit::Edge(0, Side::Bottom)));
        assert_eq!(l.hit((500, 700)), None);
        assert_eq!(l.hit((6, 8)), None);
        for x in 0..3440 {
            for y in 0..STRIP {
                assert!(!matches!(l.hit((x, y)), Some(Hit::Edge(_, Side::Top))));
            }
        }
        // Below row 0 the top edge exists, and the bottom zone of the tile above wins it.
        let l = with(&[c(0, 0, 12, 6), c(0, 6, 12, 6), c(12, 6, 12, 6)]);
        assert_eq!(l.hit((500, 718)), Some(Hit::Edge(0, Side::Bottom)));
        assert_eq!(l.hit((2500, 718)), Some(Hit::Edge(2, Side::Top)));
        assert_eq!(l.hit((-5, -5)), None);
    }

    /// AC4 (visual pass): 12 px between windows and to the zone's edges, every gap px one zone.
    #[test]
    fn ac4_tiles_are_gap_px_apart_and_every_gap_px_is_a_zone() {
        use crate::wallpaper::usable_area;
        let quarters = [
            c(0, 0, 12, 6),
            c(12, 0, 12, 6),
            c(0, 6, 12, 6),
            c(12, 6, 12, 6),
        ];
        for (w, h) in [(3440, 1440), (3840, 2160), (1125, 703)] {
            let full = Rect { x: 0, y: 0, w, h };
            // A 44 px band and a 52 px dock.
            let zone = Rect {
                y: 44,
                h: h - 96,
                ..full
            };
            let o = Output {
                area: usable_area(full, zone),
                full,
                ..uw()
            };
            for decorated in [false, true] {
                let mut l = Layout {
                    outputs: vec![o.clone()],
                    ..with(&quarters)
                };
                for id in 0..4 {
                    l.set_decorated(id, decorated);
                }
                let [a, b, c, d] = [0, 1, 2, 3].map(|id| l.rect(id).unwrap());
                let top = |id| l.bar(id).map_or(l.rect(id).unwrap().y, |bar| bar.rect.y);
                let name = format!("{w}x{h} decorated {decorated}");
                // Between windows, or a window and the bar of the one below it.
                assert_eq!(b.x - (a.x + a.w), GAP, "{name}");
                assert_eq!(top(2) - (a.y + a.h), GAP, "{name}");
                assert_eq!(top(3) - (b.y + b.h), GAP, "{name}");
                for id in 0..4 {
                    let (cells, r) = l.frame(id).unwrap();
                    let above = match decorated {
                        true => (l.bar(id).unwrap().rect.y - cells.y, r.y - top(id)),
                        false => (r.y - cells.y, 0),
                    };
                    assert_eq!(
                        above,
                        (STRIP, if decorated { super::super::BAR } else { 0 }),
                        "{name}"
                    );
                }
                // To the screen edges and the panels' zones.
                assert_eq!(a.x - zone.x, GAP, "{name}");
                assert_eq!(zone.x + zone.w - (b.x + b.w), GAP, "{name}");
                assert_eq!(top(0) - zone.y, GAP, "{name}");
                assert_eq!(zone.y + zone.h - (d.y + d.h), GAP, "{name}");
                assert_eq!(c.x, a.x, "{name}");
                // Each px between the windows, inside the grid, is exactly one zone.
                let g = o.area;
                let in_window = |p| [a, b, c, d].iter().any(|r| r.contains(p));
                for x in g.x..g.x + g.w {
                    for y in g.y..g.y + g.h {
                        let p = (x, y);
                        match in_window(p) {
                            // Sampled: a hit test per window px is slow.
                            true if (x + y) % 61 == 0 => assert_eq!(l.hit(p), None, "{name} {p:?}"),
                            true => {}
                            // Job 031: but a bar's rounded corners, which are no zone.
                            false if (0..4).any(|id| l.cut_off(id, p)) => {
                                assert_eq!(l.hit(p), None, "{name} {p:?}")
                            }
                            false => assert!(l.hit(p).is_some(), "{name} {p:?}"),
                        }
                    }
                }
                // Edge zones are the 6 px side gaps and the 4 px below.
                let zone_of = |side| l.zone(Hit::Edge(0, side)).unwrap();
                assert_eq!(zone_of(Side::Left).w, GAP / 2, "{name}");
                assert_eq!(zone_of(Side::Right).w, GAP / 2, "{name}");
                assert_eq!(zone_of(Side::Bottom).h, GAP - STRIP, "{name}");
                assert_eq!(l.zone(Hit::Edge(2, Side::Top)).unwrap().h, GAP - STRIP);
            }
        }
    }

    #[test]
    fn ac2_click_is_not_a_drag() {
        let mut l = halves();
        let (tiles, memory) = (l.tiles.clone(), l.memory.clone());
        assert_eq!(drag(&mut l, A_STRIP, &[((107, 2), 10)]), None);
        assert_eq!(l.tiles, tiles);
        assert_eq!(l.memory, memory);
    }

    #[test]
    fn ac3_drop_on_a_tile_swaps() {
        let mut l = halves();
        let sizes = (l.memory["a0"].size, l.memory["a1"].size);
        assert_eq!(
            drag(&mut l, A_STRIP, &[(B_RIM, 0), (B_RIM, 1000)]),
            Some("swapped")
        );
        assert_eq!(at(&l, 0), c(12, 0, 12, 12));
        assert_eq!(at(&l, 1), c(0, 0, 12, 12));
        assert_eq!(l.memory["a0"].cells, c(12, 0, 12, 12));
        assert_eq!(l.memory["a1"].cells, c(0, 0, 12, 12));
        assert_eq!((l.memory["a0"].size, l.memory["a1"].size), sizes);
    }

    #[test]
    fn ac4_holding_over_the_middle_tabs() {
        let mut l = halves();
        assert_eq!(
            drag(&mut l, A_STRIP, &[(B_MIDDLE, 0), (B_MIDDLE, 500)]),
            Some("tabbed")
        );
        assert_eq!(at(&l, 0), at(&l, 1));
        assert_eq!(at(&l, 1), c(12, 0, 12, 12));

        let mut l = halves();
        assert_eq!(
            drag(&mut l, A_STRIP, &[(B_MIDDLE, 0), (B_MIDDLE, 499)]),
            Some("swapped")
        );

        // Leaving the middle (still over b) resets the timer.
        let path = [
            (B_MIDDLE, 0),
            (B_RIM, 300),
            (B_MIDDLE, 350),
            (B_MIDDLE, 750),
        ];
        let mut l = halves();
        assert_eq!(drag(&mut l, A_STRIP, &path), Some("swapped"));
        let mut l = halves();
        assert_eq!(
            drag(&mut l, A_STRIP, &[&path[..3], &[(B_MIDDLE, 850)]].concat()),
            Some("tabbed")
        );
    }

    #[test]
    fn ac5_drop_on_empty_uses_the_remembered_size() {
        let mut l = with(&[c(0, 0, 12, 12)]);
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        d.motion(&mut l, (3000, 700), 0);
        assert_eq!(preview(&d), Some(c(12, 0, 12, 12)));
        let Some(Done::Layout(what, tiles)) = d.release(&mut l, (3000, 700), 10) else {
            panic!("not moved");
        };
        assert_eq!((what, tiles[0].cells), ("moved", c(12, 0, 12, 12)));
        assert_eq!(at(&l, 0), c(12, 0, 12, 12));
        assert_eq!(l.memory["a0"].cells, c(12, 0, 12, 12));

        let mut l = with(&[c(0, 0, 12, 12)]);
        l.memory.get_mut("a0").unwrap().size = (1144, 1440);
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        d.motion(&mut l, (3000, 700), 0);
        assert_eq!(preview(&d), Some(c(16, 0, 8, 12)));
        d.release(&mut l, (3000, 700), 10);
        assert_eq!(at(&l, 0), c(16, 0, 8, 12));
        assert_eq!(l.memory["a0"].size, (1144, 1440));
        assert_valid(&l);
    }

    #[test]
    fn ac6_preview_stays_on_the_grid_and_own_cells_cancel() {
        let mut l = with(&[c(0, 0, 12, 12)]);
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        for p in [
            (3439, 700),
            (3439, 1439),
            (99_999, 99_999),
            (i32::MIN, i32::MAX),
        ] {
            d.motion(&mut l, p, 0);
            let k = preview(&d).unwrap();
            assert!(
                k.col >= 0 && k.col + k.cols <= COLS && k.row + k.rows <= ROWS,
                "{k:?}"
            );
        }
        drop(d);

        // A drop within half a cell of the press cancels, however far the drag went first.
        let tiles = l.tiles.clone();
        let back = (A_STRIP.0 + 71, A_STRIP.1 + 59);
        assert_eq!(drag(&mut l, A_STRIP, &[((3000, 700), 0), (back, 10)]), None);
        assert_eq!(l.tiles, tiles);

        // A laptop right of it, whose only empty column is below its 4-column minimum: no
        // preview there, and the drop cancels.
        let laptop = Output {
            name: "laptop".into(),
            area: Rect {
                x: 3440,
                y: 0,
                w: 1125,
                h: 703,
            },
            full: Rect {
                x: 3440,
                y: 0,
                w: 1125,
                h: 703,
            },
            priority: 1,
        };
        l.set_outputs(vec![uw(), laptop]);
        for (id, cells) in [(1, c(0, 0, 10, 12)), (2, c(12, 0, 12, 12))] {
            let (app, output) = (format!("a{id}"), "laptop".into());
            l.tiles.push(Tile {
                id,
                app,
                output,
                cells,
                ..Tile::default()
            });
        }
        let tiles = l.tiles.clone();
        let gap = (3440 + 5 * 93 + 40, 300);
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        d.motion(&mut l, gap, 0);
        assert_eq!(preview(&d), None);
        assert_eq!(
            zones(&l, Some(&d), gap, 0).len(),
            (COLS - 1 + ROWS - 1) as usize
        );
        assert!(d.release(&mut l, gap, 10).is_none());
        assert_eq!(l.tiles, tiles);
    }

    #[test]
    fn ac7_dragging_a_tab_moves_only_that_tab() {
        // Job 003: the bar is all tabs, and this point is in tile 0's, below the strip.
        let mut l = with(&[c(0, 0, 12, 12), c(0, 0, 12, 12)]);
        let tab0 = (100, STRIP + 4);
        assert_eq!(drag(&mut l, tab0, &[((3000, 700), 0)]), Some("untabbed"));
        assert_eq!(at(&l, 0), c(12, 0, 12, 12));
        assert_eq!(at(&l, 1), c(0, 0, 12, 12));
    }

    /// x of column boundary `k`, y of row boundary `k`, on the ultrawide.
    fn col(k: i32) -> (i32, i32) {
        (uw().x_edge(k), 700)
    }
    fn row(k: i32) -> (i32, i32) {
        (500, uw().y_edge(k))
    }

    #[test]
    fn ac8_resize_moves_a_shared_edge() {
        let mut l = halves();
        let mut d = Drag::press(&l, (1718, 700)).unwrap();
        for (k, a, b) in [
            (16, c(0, 0, 16, 12), c(16, 0, 8, 12)),
            // Each side stops at the 3-column minimum.
            (23, c(0, 0, 21, 12), c(21, 0, 3, 12)),
            (0, c(0, 0, 3, 12), c(3, 0, 21, 12)),
            (12, c(0, 0, 12, 12), c(12, 0, 12, 12)),
        ] {
            d.motion(&mut l, col(k), 0);
            assert_eq!((at(&l, 0), at(&l, 1)), (a, b), "to {k}");
            assert_valid(&l);
        }
        assert!(d.release(&mut l, col(12), 0).is_none());

        assert_eq!(drag(&mut l, (1718, 700), &[(col(16), 0)]), Some("resized"));
        let o = uw();
        for id in [0, 1] {
            let t = l.tile(id).unwrap();
            let m = &l.memory[&t.app];
            assert_eq!(o.span_for(m.size), (t.cells.cols, t.cells.rows));
            assert_eq!(m.cells, t.cells);
        }
    }

    #[test]
    fn ac9_only_touching_tiles_change() {
        let first = [c(0, 0, 12, 6), c(12, 0, 12, 12), c(0, 6, 12, 6)];
        let mut l = with(&first);
        drag(&mut l, (1718, 100), &[(col(16), 0)]);
        assert_eq!(at(&l, 0), c(0, 0, 16, 6));
        assert_eq!(at(&l, 1), c(16, 0, 8, 12));
        assert_eq!(at(&l, 2), c(0, 6, 12, 6));

        // c grows into the empty cells and stops at b.
        drag(&mut l, (1718, 1000), &[(col(20), 0)]);
        assert_eq!(at(&l, 2), c(0, 6, 16, 6));
        assert_eq!(at(&l, 1), c(16, 0, 8, 12));

        let mut l = with(&first);
        drag(&mut l, (1722, 700), &[(col(8), 0)]);
        assert_eq!(at(&l, 0), c(0, 0, 8, 6));
        assert_eq!(at(&l, 2), c(0, 6, 8, 6));
        assert_eq!(at(&l, 1), c(8, 0, 16, 12));
    }

    #[test]
    fn ac10_vertical_edges() {
        let mut l = with(&[c(0, 0, 12, 6), c(0, 6, 12, 6)]);
        let mut d = Drag::press(&l, (500, 718)).unwrap();
        d.motion(&mut l, row(8), 0);
        assert_eq!((at(&l, 0), at(&l, 1)), (c(0, 0, 12, 8), c(0, 8, 12, 4)));
        d.motion(&mut l, row(12), 0);
        assert_eq!((at(&l, 0), at(&l, 1)), (c(0, 0, 12, 10), c(0, 10, 12, 2)));
        assert_valid(&l);
        assert_eq!(what(d.release(&mut l, row(12), 0)), Some("resized"));

        // The top edge of the lower tile is the same boundary.
        let mut l = with(&[c(0, 0, 12, 6), c(0, 6, 12, 6), c(12, 6, 12, 6)]);
        drag(&mut l, (2500, 718), &[(row(2), 0)]);
        assert_eq!(at(&l, 2), c(12, 2, 12, 10));
        assert_eq!(at(&l, 0), c(0, 0, 12, 6));
    }

    #[test]
    fn all_four_corners_resize_both_axes_and_remember_the_size() {
        let original = c(6, 2, 12, 8);
        for (x, y, target, expected) in [
            (Side::Left, Side::Top, (4, 0), c(4, 0, 14, 10)),
            (Side::Right, Side::Top, (20, 0), c(6, 0, 14, 10)),
            (Side::Left, Side::Bottom, (4, 12), c(4, 2, 14, 10)),
            (Side::Right, Side::Bottom, (20, 12), c(6, 2, 14, 10)),
        ] {
            let mut l = with(&[original]);
            let (frame, _) = l.frame(0).unwrap();
            let from = (
                if x == Side::Left {
                    frame.x + 1
                } else {
                    frame.x + frame.w - 2
                },
                if y == Side::Top {
                    frame.y + 1
                } else {
                    frame.y + frame.h - 2
                },
            );
            assert_eq!(l.hit(from), Some(Hit::Corner(0, x, y)));
            let to = (uw().x_edge(target.0), uw().y_edge(target.1));
            assert_eq!(drag(&mut l, from, &[(to, 0)]), Some("resized"));
            assert_eq!(at(&l, 0), expected);
            assert_eq!(l.memory["a0"].cells, expected);
            let rect = uw().cells_rect(expected);
            assert_eq!(l.memory["a0"].size, (rect.w, rect.h));
        }
    }

    #[test]
    fn diagonal_junction_moves_four_tiles_and_reverses_without_drift() {
        let original = [
            c(0, 0, 12, 6),
            c(12, 0, 12, 6),
            c(0, 6, 12, 6),
            c(12, 6, 12, 6),
        ];
        let mut l = with(&original);
        let from = (uw().x_edge(12) - 2, uw().y_edge(6) - 2);
        let mut d = Drag::press(&l, from).unwrap();
        for (col, row) in [(16, 8), (4, 2), (20, 10), (12, 6)] {
            let p = (uw().x_edge(col), uw().y_edge(row));
            d.motion(&mut l, p, 0);
            let expected = [
                c(0, 0, col, row),
                c(col, 0, 24 - col, row),
                c(0, row, col, 12 - row),
                c(col, row, 24 - col, 12 - row),
            ];
            assert_eq!(
                l.tiles.iter().map(|t| t.cells).collect::<Vec<_>>(),
                expected
            );
            assert_valid(&l);
        }
        assert!(d.release(&mut l, from, 0).is_none());
        assert_eq!(
            l.tiles.iter().map(|t| t.cells).collect::<Vec<_>>(),
            original
        );
    }

    #[test]
    fn diagonal_limits_each_axis_and_keeps_tabs_together() {
        let mut l = with(&[
            c(0, 0, 12, 6),
            c(0, 0, 12, 6),
            c(12, 0, 12, 6),
            c(0, 6, 12, 6),
            c(12, 6, 12, 6),
        ]);
        let from = (uw().x_edge(12) - 2, uw().y_edge(6) - 2);
        let mut d = Drag::press(&l, from).unwrap();
        d.motion(&mut l, (i32::MAX, i32::MAX), 0);
        // The minimum is 3 x 2 cells.
        assert_eq!(at(&l, 0), c(0, 0, 21, 10));
        assert_eq!(at(&l, 1), at(&l, 0));
        assert_eq!(at(&l, 4), c(21, 10, 3, 2));
        assert_valid(&l);
        assert!(d.cancel(&mut l));
        assert_eq!(at(&l, 0), c(0, 0, 12, 6));
        assert_eq!(at(&l, 4), c(12, 6, 12, 6));

        // A full-height neighbor shares only the vertical boundary.
        let mut l = with(&[c(0, 0, 12, 6), c(12, 0, 12, 12), c(0, 6, 12, 6)]);
        drag(&mut l, from, &[((uw().x_edge(16), uw().y_edge(8)), 0)]);
        assert_eq!(at(&l, 0), c(0, 0, 16, 8));
        assert_eq!(at(&l, 1), c(16, 0, 8, 12));
        assert_eq!(at(&l, 2), c(0, 8, 16, 4));
    }

    #[test]
    fn corner_targets_stay_in_gaps_and_light_both_edges() {
        let l = with(&[c(6, 2, 12, 8)]);
        let (frame, win) = l.frame(0).unwrap();
        for x in frame.x - 5..frame.x + frame.w + 5 {
            for y in frame.y - 5..frame.y + frame.h + 5 {
                if matches!(l.hit((x, y)), Some(Hit::Corner(..))) {
                    assert!(!win.contains((x, y)));
                }
            }
        }
        let p = (frame.x + frame.w - 2, frame.y + frame.h - 2);
        let lit = zones(&l, None, p, 0);
        assert_eq!(lit.len(), 2);
        for side in [Side::Right, Side::Bottom] {
            assert!(lit.contains(&(l.zone(Hit::Edge(0, side)).unwrap(), accent(1.0))));
        }
        let mut l = l;
        l.set_decorated(0, true);
        let b = l.bar(0).unwrap();
        for (button, rect) in b.buttons {
            assert_eq!(l.hit(rect.center()), Some(Hit::Button(0, button)));
        }
        l.maximize(0);
        assert!(!matches!(l.hit(p), Some(Hit::Corner(..))));
        l.fullscreen(0);
        assert!(l.hit(p).is_none());
    }

    /// Feed real press/motion/release geometry through the same double-click tracker as input.
    fn edge_click(
        l: &mut Layout,
        clicks: &mut EdgeClicks,
        p: (i32, i32),
        now: u64,
    ) -> Option<Done> {
        let d = Drag::press(l, p).unwrap();
        let edge = d.edge_click(l, p);
        assert!(d.release(l, p, now).is_none());
        clicks.release(l, edge, p, now)
    }

    #[test]
    fn double_click_expands_only_the_clicked_side_in_all_directions() {
        for (side, expected) in [
            (Side::Left, c(0, 2, 18, 8)),
            (Side::Right, c(6, 2, 18, 8)),
            (Side::Top, c(6, 0, 12, 10)),
            (Side::Bottom, c(6, 2, 12, 10)),
        ] {
            let mut l = with(&[c(6, 2, 12, 8)]);
            let p = l.zone(Hit::Edge(0, side)).unwrap().center();
            let mut clicks = EdgeClicks::default();
            assert!(edge_click(&mut l, &mut clicks, p, 0).is_none());
            assert_eq!(
                what(edge_click(&mut l, &mut clicks, p, 400)),
                Some("expanded")
            );
            assert_eq!(at(&l, 0), expected, "{side:?}");
            assert_eq!(l.memory["a0"].cells, expected);
            assert_valid(&l);
        }
    }

    const SIDES: [Side; 4] = [Side::Left, Side::Right, Side::Top, Side::Bottom];
    const OPS: [Nudge; 3] = [Nudge::Move, Nudge::Grow, Nudge::Shrink];

    /// A nudge that keeps the layout valid, as it is logged.
    fn nudge(l: &mut Layout, id: WinId, side: Side, op: Nudge) -> Option<&'static str> {
        let what = what(l.keyboard_adjust(id, side, op).ok());
        assert_valid(l);
        what
    }

    /// A nudge that changes nothing: why, with tiles and memory as they were.
    fn unchanged(l: &mut Layout, id: WinId, side: Side, op: Nudge) -> &'static str {
        let (tiles, memory) = (l.tiles.clone(), l.memory.clone());
        let why = l
            .keyboard_adjust(id, side, op)
            .expect_err(&format!("{id} {side:?} {op:?}"));
        assert_eq!(l.tiles, tiles, "{id} {side:?} {op:?}");
        assert_eq!(l.memory, memory, "{id} {side:?} {op:?}");
        why
    }

    /// The ids and cells a nudge reports, sorted by id.
    fn changed(l: &mut Layout, id: WinId, side: Side, op: Nudge) -> Vec<(WinId, Cells)> {
        let Ok(Done::Layout(_, tiles)) = l.keyboard_adjust(id, side, op) else {
            panic!("{id} {side:?} {op:?} changed nothing");
        };
        assert_valid(l);
        let mut out: Vec<_> = tiles.iter().map(|t| (t.id, t.cells)).collect();
        out.sort_unstable_by_key(|&(id, _)| id);
        out
    }

    fn assert_remembers(l: &Layout, app: &str, cells: Cells) {
        let r = l.output("uw").unwrap().cells_rect(cells);
        assert_eq!(l.memory[app].cells, cells);
        assert_eq!(l.memory[app].size, (r.w, r.h));
    }

    #[test]
    fn keyboard_grows_one_cell_per_side_up_to_the_grid_edge() {
        for (side, expected) in [
            (Side::Left, c(5, 2, 13, 8)),
            (Side::Right, c(6, 2, 13, 8)),
            (Side::Top, c(6, 1, 12, 9)),
            (Side::Bottom, c(6, 2, 12, 9)),
        ] {
            let mut l = with(&[c(6, 2, 12, 8)]);
            l.tile_mut(0).unwrap().home = Some(("unplugged".into(), c(0, 0, 12, 12)));
            assert_eq!(nudge(&mut l, 0, side, Nudge::Grow), Some("expanded"));
            assert_eq!(at(&l, 0), expected);
            assert_remembers(&l, "a0", expected);
            assert!(l.tile(0).unwrap().home.is_none());
        }
        let mut l = with(&[c(6, 2, 13, 8)]);
        for right in 20..=24 {
            assert_eq!(
                changed(&mut l, 0, Side::Right, Nudge::Grow),
                [(0, c(6, 2, right - 6, 8))]
            );
        }
        unchanged(&mut l, 0, Side::Right, Nudge::Grow);
    }

    #[test]
    fn keyboard_growth_stops_at_each_usable_monitor_edge() {
        let mut l = with(&[c(0, 0, 24, 12)]);
        for side in SIDES {
            assert_eq!(unchanged(&mut l, 0, side, Nudge::Grow), "monitor edge");
        }
        for (cells, side) in [
            (c(0, 2, 6, 4), Side::Left),
            (c(18, 2, 6, 4), Side::Right),
            (c(6, 0, 6, 4), Side::Top),
            (c(6, 8, 6, 4), Side::Bottom),
        ] {
            let mut l = with(&[cells]);
            unchanged(&mut l, 0, side, Nudge::Grow);
        }
        // Panels shrink the area; grown tiles stay inside it, also off the global origin.
        for side in SIDES {
            let mut l = with(&[c(6, 2, 12, 8)]);
            let mut output = uw();
            output.area.x = -3440;
            output.area.y = -1408;
            output.area.h = 1344;
            l.set_outputs(vec![output.clone()]);
            while nudge(&mut l, 0, side, Nudge::Grow).is_some() {}
            let rect = l.rect(0).unwrap();
            assert!(rect.x >= output.area.x && rect.y >= output.area.y);
            assert!(rect.x + rect.w <= output.area.x + output.area.w);
            assert!(rect.y + rect.h <= output.area.y + output.area.h);
            let k = at(&l, 0);
            let (start, len) = span(k, matches!(side, Side::Left | Side::Right));
            let n = if matches!(side, Side::Left | Side::Right) {
                COLS
            } else {
                ROWS
            };
            match side {
                Side::Left | Side::Top => assert_eq!(start, 0, "{side:?}"),
                Side::Right | Side::Bottom => assert_eq!(start + len, n, "{side:?}"),
            }
        }
    }

    #[test]
    fn keyboard_growth_takes_a_cell_from_the_neighbor_and_leaves_its_memory() {
        for (x, toward_b, toward_a) in [
            (true, Side::Right, Side::Left),
            (false, Side::Bottom, Side::Top),
        ] {
            let h = if x { COLS / 2 } else { ROWS / 2 };
            let (a, b) = (
                with_span(crate::layout::FULL, x, 0, h),
                with_span(crate::layout::FULL, x, h, h),
            );
            let grown = |k: Cells, start: i32, len: i32| with_span(k, x, start, len);
            for (id, side, mine, theirs) in [
                (0, toward_b, grown(a, 0, h + 1), grown(b, h + 1, h - 1)),
                (1, toward_a, grown(b, h - 1, h + 1), grown(a, 0, h - 1)),
            ] {
                let mut l = with(&[a, b]);
                let other = 1 - id;
                let home = Some(("unplugged".to_string(), c(0, 0, 3, 2)));
                l.tile_mut(other).unwrap().home = home.clone();
                let memory = l.memory[&format!("a{other}")].clone();
                let mut expected = [(id, mine), (other, theirs)];
                expected.sort_unstable_by_key(|&(id, _)| id);
                assert_eq!(changed(&mut l, id, side, Nudge::Grow), expected);
                assert_eq!(l.memory[&format!("a{other}")], memory);
                assert_eq!(l.tile(other).unwrap().home, home);
                assert_remembers(&l, &format!("a{id}"), mine);
            }
        }
    }

    #[test]
    fn keyboard_growth_never_shrinks_a_neighbor_below_the_minimum() {
        let mut l = with(&[c(0, 0, 21, 12), c(21, 0, 3, 12)]);
        let why = unchanged(&mut l, 0, Side::Right, Nudge::Grow);
        assert_eq!(why, "neighbour at minimum");
        let mut l = with(&[c(0, 0, 24, 10), c(0, 10, 24, 2)]);
        let why = unchanged(&mut l, 0, Side::Bottom, Nudge::Grow);
        assert_eq!(why, "neighbour at minimum");
    }

    #[test]
    fn keyboard_growth_takes_from_every_touching_tile_or_none() {
        let mut l = with(&[c(0, 0, 12, 12), c(12, 0, 12, 6), c(12, 6, 12, 6)]);
        assert_eq!(
            changed(&mut l, 0, Side::Right, Nudge::Grow),
            [
                (0, c(0, 0, 13, 12)),
                (1, c(13, 0, 11, 6)),
                (2, c(13, 6, 11, 6))
            ]
        );
        let mut l = with(&[c(0, 0, 12, 12), c(12, 0, 12, 6), c(12, 6, 3, 6)]);
        unchanged(&mut l, 0, Side::Right, Nudge::Grow);
    }

    #[test]
    fn keyboard_growth_moves_a_partial_neighbors_whole_side_and_skips_corners() {
        let mut l = with(&[c(8, 4, 8, 4), c(16, 0, 8, 12)]);
        assert_eq!(
            changed(&mut l, 0, Side::Right, Nudge::Grow),
            [(0, c(8, 4, 9, 4)), (1, c(17, 0, 7, 12))]
        );
        let mut l = with(&[c(8, 4, 8, 4), c(16, 8, 8, 4)]);
        assert_eq!(
            changed(&mut l, 0, Side::Right, Nudge::Grow),
            [(0, c(8, 4, 9, 4))]
        );
        assert_eq!(at(&l, 1), c(16, 8, 8, 4));
    }

    #[test]
    fn keyboard_shrinks_one_cell_and_never_touches_neighbors() {
        for (side, expected) in [
            (Side::Left, c(7, 2, 11, 8)),
            (Side::Right, c(6, 2, 11, 8)),
            (Side::Top, c(6, 3, 12, 7)),
            (Side::Bottom, c(6, 2, 12, 7)),
        ] {
            let mut l = with(&[c(6, 2, 12, 8)]);
            l.tile_mut(0).unwrap().home = Some(("unplugged".into(), c(0, 0, 12, 12)));
            assert_eq!(nudge(&mut l, 0, side, Nudge::Shrink), Some("shrunk"));
            assert_eq!(at(&l, 0), expected);
            assert_remembers(&l, "a0", expected);
            assert!(l.tile(0).unwrap().home.is_none());
        }
        let mut l = halves();
        assert_eq!(
            changed(&mut l, 0, Side::Right, Nudge::Shrink),
            [(0, c(0, 0, 11, 12))]
        );
        assert_eq!(at(&l, 1), c(12, 0, 12, 12));
        let mut l = with(&[c(6, 2, 3, 2)]);
        for side in SIDES {
            assert_eq!(unchanged(&mut l, 0, side, Nudge::Shrink), "tile at minimum");
        }
    }

    #[test]
    fn keyboard_move_pushes_the_neighbor_until_its_minimum() {
        let mut l = halves();
        l.tile_mut(0).unwrap().home = Some(("unplugged".into(), c(0, 0, 12, 12)));
        let (size, a1) = (l.memory["a0"].size, l.memory["a1"].clone());
        assert_eq!(nudge(&mut l, 0, Side::Right, Nudge::Move), Some("moved"));
        assert_eq!((at(&l, 0), at(&l, 1)), (c(1, 0, 12, 12), c(13, 0, 11, 12)));
        assert_eq!(l.memory["a0"].cells, c(1, 0, 12, 12));
        assert_eq!(l.memory["a0"].size, size);
        assert_eq!(l.memory["a1"], a1);
        assert!(l.tile(0).unwrap().home.is_none());
        for _ in 0..8 {
            assert_eq!(nudge(&mut l, 0, Side::Right, Nudge::Move), Some("moved"));
        }
        assert_eq!((at(&l, 0), at(&l, 1)), (c(9, 0, 12, 12), c(21, 0, 3, 12)));
        unchanged(&mut l, 0, Side::Right, Nudge::Move);

        let mut l = with(&[c(0, 0, 24, 6), c(0, 6, 24, 6)]);
        for _ in 0..4 {
            assert_eq!(nudge(&mut l, 0, Side::Bottom, Nudge::Move), Some("moved"));
        }
        assert_eq!((at(&l, 0), at(&l, 1)), (c(0, 4, 24, 6), c(0, 10, 24, 2)));
        unchanged(&mut l, 0, Side::Bottom, Nudge::Move);
        // Moving back leaves the pushed tile where it is, at its minimum.
        assert_eq!(
            changed(&mut l, 0, Side::Top, Nudge::Move),
            [(0, c(0, 3, 24, 6))]
        );
    }

    #[test]
    fn keyboard_move_leaves_tiles_behind_and_stops_at_edges_and_minimums() {
        let mut l = with(&[c(0, 0, 6, 12), c(6, 0, 6, 12), c(12, 0, 12, 12)]);
        assert_eq!(
            changed(&mut l, 1, Side::Right, Nudge::Move),
            [(1, c(7, 0, 6, 12)), (2, c(13, 0, 11, 12))]
        );
        assert_eq!(at(&l, 0), c(0, 0, 6, 12));
        let mut l = with(&[c(0, 0, 12, 12), c(12, 0, 12, 6), c(12, 6, 3, 6)]);
        unchanged(&mut l, 0, Side::Right, Nudge::Move);
        let mut l = with(&[c(0, 0, 6, 4), c(18, 8, 6, 4)]);
        unchanged(&mut l, 0, Side::Left, Nudge::Move);
        unchanged(&mut l, 0, Side::Top, Nudge::Move);
        unchanged(&mut l, 1, Side::Right, Nudge::Move);
        unchanged(&mut l, 1, Side::Bottom, Nudge::Move);
    }

    #[test]
    fn keyboard_nudges_move_tab_groups_as_one_and_minimized_neighbors_give_way() {
        // Tile 1 is the active tab of 0 and 1.
        for (op, expected) in [
            (Nudge::Move, c(5, 2, 8, 6)),
            (Nudge::Grow, c(4, 2, 9, 6)),
            (Nudge::Shrink, c(4, 2, 7, 6)),
        ] {
            let mut l = with(&[c(4, 2, 8, 6), c(4, 2, 8, 6), c(16, 0, 8, 12)]);
            unchanged(&mut l, 0, Side::Right, op);
            assert_eq!(
                changed(&mut l, 1, Side::Right, op),
                [(0, expected), (1, expected)]
            );
            assert!(l.visible(1));
            assert_eq!(at(&l, 2), c(16, 0, 8, 12));
        }
        // A neighbor tab group gives up the cell with every tab.
        let mut l = with(&[c(0, 0, 12, 12), c(12, 0, 12, 12), c(12, 0, 12, 12)]);
        let theirs = c(13, 0, 11, 12);
        assert_eq!(
            changed(&mut l, 0, Side::Right, Nudge::Grow),
            [(0, c(0, 0, 13, 12)), (1, theirs), (2, theirs)]
        );
        // A minimized neighbor too, unless it is at its minimum.
        let mut l = halves();
        assert!(l.minimize(1));
        assert_eq!(
            changed(&mut l, 0, Side::Right, Nudge::Grow),
            [(0, c(0, 0, 13, 12)), (1, c(13, 0, 11, 12))]
        );
        let mut l = with(&[c(0, 0, 21, 12), c(21, 0, 3, 12)]);
        assert!(l.minimize(1));
        unchanged(&mut l, 0, Side::Right, Nudge::Grow);
        unchanged(&mut l, 0, Side::Right, Nudge::Move);
    }

    #[test]
    fn keyboard_nudges_keep_other_outputs_and_skip_hidden_windows() {
        let mut l = halves();
        let mut second = uw();
        second.name = "other".into();
        second.area.x = 3440;
        second.full.x = 3440;
        l.set_outputs(vec![uw(), second]);
        l.tiles.push(Tile {
            id: 2,
            app: "a2".into(),
            output: "other".into(),
            cells: c(12, 0, 12, 12),
            ..Tile::default()
        });
        // assert_valid ignores outputs, so check this one by hand.
        assert!(l.keyboard_adjust(0, Side::Right, Nudge::Grow).is_ok());
        assert_eq!((at(&l, 0), at(&l, 1)), (c(0, 0, 13, 12), c(13, 0, 11, 12)));
        assert_eq!(at(&l, 2), c(12, 0, 12, 12));
        assert_eq!(l.tile(2).unwrap().output, "other");

        for mode in 0..8 {
            // Tile 0 is the one nudged; 1 is its tab, 2 an output-mate.
            let mut l = with(&[c(6, 2, 8, 6), c(6, 2, 8, 6), c(16, 2, 6, 6)]);
            let id = match mode {
                0 => {
                    assert!(l.minimize(1));
                    1
                }
                1 => 0, // Hidden behind its active tab 1.
                2 => {
                    assert!(l.maximize(1));
                    1
                }
                3 => {
                    assert!(l.fullscreen(1));
                    1
                }
                4 => {
                    assert!(l.fullscreen(2));
                    1
                }
                5 => {
                    assert!(l.maximize(2));
                    1
                }
                6 => {
                    l.set_outputs(Vec::new());
                    1
                }
                _ => 99,
            };
            for side in SIDES {
                for op in OPS {
                    unchanged(&mut l, id, side, op);
                }
            }
        }
    }

    #[test]
    fn keyboard_nudges_use_the_laptop_minimum() {
        let mut l = with(&[c(0, 0, 16, 12), c(16, 0, 8, 12)]);
        let mut laptop = uw();
        laptop.area = Rect {
            x: 0,
            y: 0,
            w: 1125,
            h: 703,
        };
        laptop.full = laptop.area;
        assert_eq!(laptop.span_for((MIN_W, MIN_H)), (7, 5));
        l.set_outputs(vec![laptop]);
        assert_eq!(
            changed(&mut l, 0, Side::Right, Nudge::Grow),
            [(0, c(0, 0, 17, 12)), (1, c(17, 0, 7, 12))]
        );
        unchanged(&mut l, 0, Side::Right, Nudge::Grow);
        unchanged(&mut l, 0, Side::Right, Nudge::Move);
        unchanged(&mut l, 1, Side::Right, Nudge::Shrink);
        unchanged(&mut l, 1, Side::Left, Nudge::Shrink);
    }

    #[test]
    fn edge_expansion_honors_neighbors_panels_and_output_origin() {
        let mut l = halves();
        let p = (1718, 700);
        let mut clicks = EdgeClicks::default();
        edge_click(&mut l, &mut clicks, p, 0);
        assert_eq!(
            what(edge_click(&mut l, &mut clicks, p, 100)),
            Some("expanded")
        );
        assert_eq!(at(&l, 0), c(0, 0, 21, 12));
        assert_eq!(at(&l, 1), c(21, 0, 3, 12));
        assert_valid(&l);

        let mut l = with(&[c(6, 2, 12, 8)]);
        let mut o = uw();
        o.full.x = -3440;
        o.full.y = -1440;
        o.area = Rect {
            x: -3440,
            y: -1416,
            w: 3440,
            h: 1368,
        };
        l.set_outputs(vec![o.clone()]);
        for side in [Side::Top, Side::Bottom] {
            let p = l.zone(Hit::Edge(0, side)).unwrap().center();
            edge_click(&mut l, &mut clicks, p, 0);
            edge_click(&mut l, &mut clicks, p, 100);
        }
        assert_eq!(at(&l, 0), c(6, 0, 12, 12));
        let rect = l.rect(0).unwrap();
        assert!(rect.y >= o.area.y && rect.y + rect.h <= o.area.y + o.area.h);
    }

    #[test]
    fn double_click_rejects_delays_other_edges_intervening_clicks_and_motion() {
        for scenario in 0..5 {
            let mut l = with(&[c(6, 2, 12, 8)]);
            let mut clicks = EdgeClicks::default();
            let p = l.zone(Hit::Edge(0, Side::Right)).unwrap().center();
            edge_click(&mut l, &mut clicks, p, 0);
            let mut next = p;
            let mut time = 100;
            match scenario {
                0 => time = 401,
                1 => next = l.zone(Hit::Edge(0, Side::Left)).unwrap().center(),
                2 => {
                    clicks.release(&mut l, None, p, 50);
                }
                3 => {
                    clicks.motion((p.0, p.1 + 8));
                    clicks.motion(p);
                }
                _ => next.1 += 8,
            }
            assert!(edge_click(&mut l, &mut clicks, next, time).is_none());
            assert_eq!(at(&l, 0), c(6, 2, 12, 8));
        }
        let mut l = halves();
        let p = (1718, 700);
        let mut d = Drag::press(&l, p).unwrap();
        d.motion(&mut l, col(16), 0);
        d.motion(&mut l, p, 1);
        assert!(
            d.edge_click(&l, p).is_none(),
            "a drag back to its origin is no click"
        );
        assert!(d.release(&mut l, p, 2).is_none());
    }

    #[test]
    fn ac12_a_vanished_tile_cancels() {
        let mut l = halves();
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        d.motion(&mut l, B_RIM, 0);
        l.remove(0);
        d.motion(&mut l, (3000, 700), 10);
        assert!(d.release(&mut l, (3000, 700), 20).is_none());
        assert_eq!(at(&l, 1), c(12, 0, 12, 12));

        // A resize undoes what it did to the others.
        let mut l = halves();
        let mut d = Drag::press(&l, (1718, 700)).unwrap();
        d.motion(&mut l, col(16), 0);
        assert_eq!(at(&l, 1), c(16, 0, 8, 12));
        l.remove(0);
        assert!(d.motion(&mut l, col(18), 10));
        assert!(d.release(&mut l, col(18), 20).is_none());
        assert_eq!(at(&l, 1), c(12, 0, 12, 12));

        // An armed press too.
        let mut l = halves();
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        l.remove(0);
        d.motion(&mut l, A_STRIP, 0);
        assert!(zones(&l, Some(&d), A_STRIP, 0).is_empty());
        assert!(d.release(&mut l, B_RIM, 0).is_none());
    }

    #[test]
    fn cancel_puts_everything_back() {
        // A resize mid-way: both tiles return to where they were.
        let mut l = halves();
        let tiles = l.tiles.clone();
        let mut d = Drag::press(&l, (1718, 700)).unwrap();
        d.motion(&mut l, col(18), 0);
        assert_eq!(at(&l, 1), c(18, 0, 6, 12));
        assert!(d.cancel(&mut l));
        assert_eq!(l.tiles, tiles);

        // A move changes nothing until the drop, so cancelling one changes nothing.
        let memory = l.memory.clone();
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        d.motion(&mut l, B_MIDDLE, 0);
        assert!(!d.cancel(&mut l));
        assert_eq!((l.tiles.clone(), l.memory.clone()), (tiles, memory));
    }

    #[test]
    fn grid_fades_on_small_cells() {
        assert_eq!(grid_alpha(80), GRID_ALPHA);
        assert_eq!(grid_alpha(200), GRID_ALPHA);
        assert!(grid_alpha(79) < GRID_ALPHA);
        assert!(grid_alpha(46) <= 0.12);
        assert_eq!(grid_alpha(1), GRID_MIN_ALPHA);
        assert_eq!(grid_alpha(0), GRID_MIN_ALPHA);
        assert_eq!(grid_alpha(i32::MIN), GRID_MIN_ALPHA);
        let all: Vec<f32> = (0..=100).map(grid_alpha).collect();
        assert!(all.windows(2).all(|w| w[0] <= w[1]));
        assert!(all.iter().all(|a| a.is_finite() && *a >= GRID_MIN_ALPHA));
    }

    #[test]
    fn laptop_grid_draws_fainter() {
        let laptop = Output {
            area: Rect {
                x: 0,
                y: 0,
                w: 1125,
                h: 703,
            },
            ..uw()
        };
        assert_eq!(laptop.cell_w().min(laptop.cell_h()), 46);
        let rects = move_overlay(&laptop, Target::Free(c(0, 0, 6, 6)), 0, PREVIEW_ALPHA);
        let (lines, preview) = rects.split_at(rects.len() - 1);
        assert_eq!(lines.len(), (COLS - 1 + ROWS - 1) as usize);
        assert!(
            lines
                .iter()
                .all(|&(_, k, cut)| k == accent(grid_alpha(46)) && cut.is_none())
        );
        assert!(grid_alpha(46) < GRID_ALPHA);
        assert_eq!(preview[0].1, accent(PREVIEW_ALPHA));
        // Job 031: the preview is rounded by its own rect.
        let cut = Some(Cut::solid(Shape::new(preview[0].0, RADIUS)));
        assert_eq!(preview[0].2, cut);
    }

    #[test]
    fn ac13_overlay() {
        let mut l = halves();
        assert!(zones(&l, None, (500, 700), 0).is_empty());
        let strip = Rect {
            x: 6,
            y: 0,
            w: 1708,
            h: STRIP,
        };
        assert_eq!(zones(&l, None, A_STRIP, 0), vec![(strip, accent(1.0))]);
        assert_eq!(
            accent(1.0),
            [
                0x2B as f32 / 255.0,
                0xA3 as f32 / 255.0,
                0xD8 as f32 / 255.0,
                1.0
            ]
        );

        let mut d = Drag::press(&l, A_STRIP).unwrap();
        assert_eq!(zones(&l, Some(&d), A_STRIP, 0), vec![(strip, accent(1.0))]);
        d.motion(&mut l, B_RIM, 0);
        let rects = zones(&l, Some(&d), B_RIM, 0);
        assert_eq!(rects.len(), (COLS - 1 + ROWS - 1 + 1) as usize);
        let lines = &rects[..rects.len() - 1];
        assert!(lines.iter().all(|(r, _)| r.w == 1 || r.h == 1));
        assert!(lines.iter().all(|&(_, k)| k == accent(GRID_ALPHA)));
        // The preview is where the window itself would go: the tile rect.
        assert_eq!(rects.last().unwrap().0, uw().tile_rect(c(12, 0, 12, 12)));
        assert_eq!(rects.last().unwrap().1, accent(PREVIEW_ALPHA));

        // Swap and tab previews look different: more solid once the hold has elapsed.
        d.motion(&mut l, B_MIDDLE, 1000);
        let preview_at = |t| *zones(&l, Some(&d), B_MIDDLE, t).last().unwrap();
        assert_eq!(preview_at(1499).1, accent(PREVIEW_ALPHA));
        assert_eq!(preview_at(1500).1, accent(TAB_ALPHA));
        // Job 007 AC3: 0.55 keeps the text under a tab preview readable.
        assert_eq!(preview_at(1499).1, rgba(ACCENT, 0.35));
        assert_eq!(preview_at(1500).1, rgba(ACCENT, 0.55));
        // Job 003: a tab group has a bar, so the tab-in preview sits below one.
        assert_eq!(preview_at(1499).0, uw().tile_rect(c(12, 0, 12, 12)));
        assert_eq!(
            preview_at(1500).0,
            uw().window_rect(c(12, 0, 12, 12), STRIP + super::super::BAR)
        );
    }

    /// Job 007 AC2: a bright frame around the focused window and a dim one around the rest, just
    /// outside each window and behind the zones.
    #[test]
    fn ac2_focus_and_idle_frames() {
        let framed = |o: &[(Rect, [f32; 4])], win: Rect, px: i32, alpha: f32| {
            let sides: Vec<Rect> = o
                .iter()
                .filter(|&&(_, k)| k == accent(alpha))
                .map(|&(r, _)| r)
                .filter(|r| r.x <= win.x + win.w && win.x <= r.x + r.w)
                .filter(|r| r.y <= win.y + win.h && win.y <= r.y + r.h)
                .collect();
            assert_eq!(sides.len(), 4, "{sides:?}");
            let inter = |a: Rect, b: Rect| {
                a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
            };
            for r in &sides {
                assert!(!inter(*r, win), "{r:?} overlaps {win:?}");
                assert_eq!(r.w.min(r.h), px, "{r:?}");
            }
            let grown = Rect {
                x: win.x - px,
                y: win.y - px,
                w: win.w + 2 * px,
                h: win.h + 2 * px,
            };
            let area: i32 = sides.iter().map(|r| r.w * r.h).sum();
            assert_eq!(
                area,
                grown.w * grown.h - win.w * win.h,
                "the sides touch the window"
            );
            assert!(sides.iter().all(|&r| inter(r, grown)));
        };
        let mut l = halves();
        let (left, right) = (l.rect(0).unwrap(), l.rect(1).unwrap());
        let o = overlay_at(&l, None, (500, 700), 0, Some(0));
        assert_eq!(o.len(), 8);
        framed(&o, left, FOCUS_PX, 1.0);
        framed(&o, right, IDLE_PX, IDLE_ALPHA);
        assert_eq!(accent(IDLE_ALPHA), rgba(ACCENT, 0.3));

        let o = overlay_at(&l, None, (500, 700), 0, None);
        assert!(o.iter().all(|&(_, k)| k == accent(IDLE_ALPHA)));
        framed(&o, left, IDLE_PX, IDLE_ALPHA);
        framed(&o, right, IDLE_PX, IDLE_ALPHA);

        // A hovered strip is in front of its tile's frame.
        let o = overlay_at(&l, None, A_STRIP, 0, Some(0));
        let strip = o.iter().position(|&(r, _)| r.h == STRIP).unwrap();
        let frame = o
            .iter()
            .position(|&(r, _)| r.w.min(r.h) == FOCUS_PX)
            .unwrap();
        assert!(strip < frame);

        // Minimized, under a maximize and fullscreen: no frame.
        let frames_of = |l: &Layout, id: WinId| {
            let o = overlay_at(l, None, (500, 700), 0, Some(id));
            o.iter().filter(|&&(_, k)| k == accent(1.0)).count()
        };
        assert_eq!(frames_of(&l, 1), 4);
        l.minimize(1);
        assert_eq!(frames_of(&l, 1), 0, "minimized");
        let mut l = halves();
        l.maximize(0);
        assert_eq!(frames_of(&l, 1), 0, "under a maximize");
        assert_eq!(frames_of(&l, 0), 4, "the maximized window itself");
        let mut l = halves();
        l.fullscreen(1);
        assert_eq!(frames_of(&l, 1), 0, "fullscreen");
        assert!(overlay_at(&l, None, (500, 700), 0, Some(1)).is_empty());

        // Round 1: a decorated window's frame takes in its bar; the top line runs along the bar's
        // top rows in front of it, and the sides start at the bar's top.
        let mut l = halves();
        l.set_decorated(0, true);
        let (bar, win) = (l.bar(0).unwrap().rect, l.rect(0).unwrap());
        let o = overlay_at(&l, None, (500, 700), 0, Some(0));
        let top = Rect {
            x: win.x - FOCUS_PX,
            y: bar.y,
            w: win.w + 2 * FOCUS_PX,
            h: FOCUS_PX,
        };
        let pos = |want: (Rect, [f32; 4])| o.iter().position(|&x| x == want).unwrap();
        assert!(pos((top, accent(1.0))) < pos((bar, rgba(crate::theme::BAR_BG, 1.0))));
        let left = Rect {
            x: win.x - FOCUS_PX,
            y: bar.y + FOCUS_PX,
            w: FOCUS_PX,
            h: win.y + win.h - bar.y - FOCUS_PX,
        };
        assert!(o.contains(&(left, accent(1.0))));
        assert_eq!(o.iter().filter(|&&(_, k)| k == accent(1.0)).count(), 4);
    }

    /// The output and target of a move in progress.
    fn moving_to(d: &Drag) -> Option<(&str, Target)> {
        match &d.0 {
            State::Move { output, target, .. } => Some((output, *target)),
            _ => None,
        }
    }

    /// A point on `id`'s strip row, `dx` px from the cells' left edge.
    fn on_strip(l: &Layout, id: WinId, dx: i32) -> (i32, i32) {
        let (c, _) = l.frame(id).unwrap();
        (c.x + dx, c.y + 3)
    }

    /// Job 016 AC4: the preview moves a cell for every cell the pointer does, from the first one,
    /// until the grid's edge; under half a cell it is the tile's own cells and the drop cancels.
    #[test]
    fn ac4_move_follows_the_pointer_cell_by_cell() {
        let start = c(9, 3, 6, 6);
        let (cell_w, cell_h) = (3440.0 / 24.0, 120.0);
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let mut l = with(&[start]);
            let press = on_strip(&l, 0, uw().cells_rect(start).w / 2);
            assert_eq!(l.hit(press), Some(Hit::Strip(0)));
            let near = (press.0 + dx * 70 + dy * 9, press.1 + dy * 59 + dx * 9);
            let mut d = Drag::press(&l, press).unwrap();
            d.motion(&mut l, near, 0);
            assert_eq!(moving_to(&d), Some(("uw", Target::Own(start))));
            assert!(d.release(&mut l, near, 10).is_none());
            assert_eq!(at(&l, 0), start);

            let mut d = Drag::press(&l, press).unwrap();
            let mut p = press;
            for k in 1..=24 {
                let off = |cell: f64, d: i32| (f64::from(k * d) * cell).round() as i32;
                p = (press.0 + off(cell_w, dx), press.1 + off(cell_h, dy));
                d.motion(&mut l, p, 0);
                let want = Cells {
                    col: (start.col + k * dx).clamp(0, COLS - start.cols),
                    row: (start.row + k * dy).clamp(0, ROWS - start.rows),
                    ..start
                };
                assert_eq!(
                    moving_to(&d),
                    Some(("uw", Target::Free(want))),
                    "{dx},{dy} k {k}"
                );
            }
            assert_eq!(what(d.release(&mut l, p, 10)), Some("moved"));
            let end = |n: i32, d: i32| if d > 0 { n } else { 0 };
            let (col, row) = match (dx, dy) {
                (0, _) => (start.col, end(ROWS - start.rows, dy)),
                _ => (end(COLS - start.cols, dx), start.row),
            };
            assert_eq!(at(&l, 0), Cells { col, row, ..start });
        }
    }

    /// Job 016 AC5: unclamped, the preview's origin is within half a cell of `pointer - grab`;
    /// a smaller remembered size keeps the pointer on the preview.
    #[test]
    fn ac5_the_grabbed_point_stays_under_the_pointer() {
        let o = uw();
        let (cols, rows) = (6, 6);
        let size = o.cells_rect(c(0, 0, cols, rows));
        for grab in [(430, 3), (40, 3), (size.w - 40, 3), (300, 34)] {
            for x in (-200..3640).step_by(7) {
                for y in (-100..1540).step_by(13) {
                    let k = candidate(&o, (cols, rows), (x, y), grab);
                    let (wx, wy) = (x - grab.0, y - grab.1);
                    if (o.x_edge(0)..=o.x_edge(COLS - cols)).contains(&wx) {
                        let off = (o.x_edge(k.col) - wx).abs();
                        assert!(off <= o.cell_w() / 2 + 1, "{x},{y} {grab:?}: {off}");
                    }
                    if (o.y_edge(0)..=o.y_edge(ROWS - rows)).contains(&wy) {
                        let off = (o.y_edge(k.row) - wy).abs();
                        assert!(off <= o.cell_h() / 2, "{x},{y} {grab:?}: {off}");
                    }
                }
            }
        }
        // Grabbed 1600 px in, remembering 3 x 2 cells: the pointer stays over the preview.
        let mut l = with(&[c(0, 0, 12, 12)]);
        l.memory.get_mut("a0").unwrap().size = (430, 240);
        let press = on_strip(&l, 0, 1600);
        let mut d = Drag::press(&l, press).unwrap();
        for k in 1..40 {
            let p = (press.0 + 37 * k, press.1 + 29 * k);
            d.motion(&mut l, p, 0);
            let Some((_, Target::Free(cells))) = moving_to(&d) else {
                panic!("no free preview at {p:?}");
            };
            assert_eq!((cells.cols, cells.rows), (3, 2));
            let r = o.cells_rect(cells);
            assert!(r.x <= p.0 && p.0 < r.x + r.w, "{p:?} outside {cells:?}");
        }
    }

    /// Job 016 AC6: over another tile it is still a swap or a tab; a candidate that would overlap a
    /// tile falls back to the nearest free rect, or to none.
    #[test]
    fn ac6_blocked_candidates_fall_back_and_never_overlap() {
        let mut l = with(&[c(0, 0, 6, 12), c(12, 0, 12, 12)]);
        let press = on_strip(&l, 0, 430);
        let mut d = Drag::press(&l, press).unwrap();
        // Four cells right is free; eight would overlap b, with the pointer still left of it.
        d.motion(&mut l, (press.0 + 573, press.1), 0);
        assert_eq!(moving_to(&d), Some(("uw", Target::Free(c(4, 0, 6, 12)))));
        let p = (press.0 + 1147, press.1 + 300);
        assert!(p.0 < uw().x_edge(12));
        d.motion(&mut l, p, 0);
        assert_eq!(moving_to(&d), Some(("uw", Target::Free(c(6, 0, 6, 12)))));
        assert_eq!(what(d.release(&mut l, p, 10)), Some("moved"));
        assert_eq!(at(&l, 0), c(6, 0, 6, 12));
        assert_eq!(at(&l, 1), c(12, 0, 12, 12));

        // Over b it swaps, and a hold tabs.
        let moved = || with(&[c(6, 0, 6, 12), c(12, 0, 12, 12)]);
        let (l, over_b, middle) = (moved(), (2500, 300), (2580, 700));
        let press = on_strip(&l, 0, 430);
        let mut d = Drag::press(&l, press).unwrap();
        let mut l = l;
        d.motion(&mut l, over_b, 0);
        assert_eq!(
            moving_to(&d),
            Some(("uw", Target::Onto(1, c(12, 0, 12, 12))))
        );
        assert_eq!(what(d.release(&mut l, over_b, 10)), Some("swapped"));
        let hold = [(middle, 0), (middle, HOLD_MS)];
        assert_eq!(drag(&mut moved(), press, &hold), Some("tabbed"));

        // A tab pulled out of a full grid, pointer over its own group: nowhere to go.
        let mut l = with(&[c(0, 0, 12, 12), c(0, 0, 12, 12), c(12, 0, 12, 12)]);
        let tiles = l.tiles.clone();
        let from = l.zone(Hit::Tab(1)).unwrap().center();
        let mut d = Drag::press(&l, from).unwrap();
        d.motion(&mut l, (1600, 700), 0);
        assert_eq!(moving_to(&d), Some(("uw", Target::Nowhere)));
        assert!(d.release(&mut l, (1600, 700), 10).is_none());
        assert_eq!(l.tiles, tiles);
    }

    /// Job 016 AC7: on another monitor the candidate is clamped to that monitor's grid.
    #[test]
    fn ac7_a_move_to_another_monitor_clamps_to_its_grid() {
        let mut l = with(&[c(0, 0, 6, 6)]);
        let mut uhd = uw();
        uhd.name = "uhd".into();
        uhd.area = Rect {
            x: 3440,
            y: 0,
            w: 3840,
            h: 2160,
        };
        uhd.full = uhd.area;
        l.set_outputs(vec![uw(), uhd.clone()]);
        let (cols, rows) = uhd.span_for(l.memory["a0"].size);
        assert_eq!((cols, rows), (5, 4));
        let press = on_strip(&l, 0, 430);
        let mut d = Drag::press(&l, press).unwrap();
        for (p, want) in [
            ((3460, 50), c(0, 0, cols, rows)),
            ((3460, 100), c(0, 1, cols, rows)),
            ((3440 + 3839, 2159), c(COLS - cols, ROWS - rows, cols, rows)),
            ((3440 + 2000, 920), c(10, 5, cols, rows)),
        ] {
            d.motion(&mut l, p, 0);
            assert_eq!(moving_to(&d), Some(("uhd", Target::Free(want))), "{p:?}");
        }
        assert_eq!(
            what(d.release(&mut l, (3440 + 2000, 920), 0)),
            Some("moved")
        );
        assert_eq!(l.tile(0).unwrap().output, "uhd");
        assert_eq!(at(&l, 0), c(10, 5, cols, rows));
    }

    /// Strip-row and bar-row hits for one tile, `id`'s group: corners at the strip's ends only.
    fn assert_strip_corners(l: &Layout, id: WinId, name: &str) {
        let (c, r) = l.frame(id).unwrap();
        for x in c.x..c.x + c.w {
            for y in c.y..c.y + STRIP {
                let want = match x {
                    _ if x < c.x + CORNER_PX => Hit::Corner(id, Side::Left, Side::Top),
                    _ if x >= c.x + c.w - CORNER_PX => Hit::Corner(id, Side::Right, Side::Top),
                    _ => Hit::Strip(id),
                };
                assert_eq!(l.hit((x, y)), Some(want), "{name} {x},{y}");
                assert!(!r.contains((x, y)));
            }
        }
        if let Some(bar) = l.bar(id) {
            assert!(bar.rect.y >= c.y + STRIP, "{name}");
            for x in bar.rect.x..bar.rect.x + bar.rect.w {
                for y in bar.rect.y..bar.rect.y + bar.rect.h {
                    // Job 031: the bar's rounded corners are no zone.
                    let want = bar
                        .buttons
                        .iter()
                        .filter(|_| !l.cut_off(id, (x, y)))
                        .find(|(_, b)| b.contains((x, y)))
                        .map(|&(b, _)| Hit::Button(id, b))
                        .or_else(|| match bar.tabs.is_empty() {
                            _ if l.cut_off(id, (x, y)) => None,
                            true => Some(Hit::Strip(id)),
                            false => bar
                                .tabs
                                .iter()
                                .find(|(_, t)| t.contains((x, y)))
                                .map(|&(t, _)| Hit::Tab(t)),
                        });
                    assert_eq!(l.hit((x, y)), want, "{name} bar {x},{y}");
                }
            }
        }
    }

    /// Job 016 AC8: the strip's outer 26 px at each end are top corners, at row 0 and below it,
    /// plain, decorated and tabbed; the bar row keeps its tabs and buttons.
    #[test]
    fn ac8_strip_ends_are_top_corners() {
        for row in [0, 3] {
            let cells = c(6, row, 12, 6);
            let mut plain = with(&[cells]);
            assert_strip_corners(&plain, 0, &format!("plain row {row}"));
            plain.set_decorated(0, true);
            assert_strip_corners(&plain, 0, &format!("decorated row {row}"));
            let mut tabbed = with(&[cells, cells]);
            assert_strip_corners(&tabbed, 1, &format!("tabbed row {row}"));
            tabbed.set_decorated(1, true);
            assert_strip_corners(&tabbed, 1, &format!("tabbed decorated row {row}"));
        }
        // The 320 px minimum tile keeps 256 px of strip that moves.
        let l = with(&[c(6, 3, 3, 2)]);
        let (c, _) = l.frame(0).unwrap();
        let moves = (c.x..c.x + c.w)
            .filter(|&x| l.hit((x, c.y)) == Some(Hit::Strip(0)))
            .count();
        assert!(moves >= 256 - GAP as usize, "{moves}");
    }

    /// Job 016 AC9: a strip-end corner resizes both axes by whole cells, stops at the minimum and
    /// is remembered; on row 0 the top only moves down; a maximized tile has no top corners.
    #[test]
    fn ac9_strip_corners_resize_both_axes() {
        let o = uw();
        let to = |col, row| (o.x_edge(col), o.y_edge(row));
        for (dx, side, step, end) in [
            (20, Side::Left, c(8, 5, 10, 4), c(15, 7, 3, 2)),
            (12 * 143 - 20, Side::Right, c(6, 5, 10, 4), c(6, 7, 3, 2)),
        ] {
            let mut l = with(&[c(6, 3, 12, 6)]);
            let from = on_strip(&l, 0, dx);
            assert_eq!(l.hit(from), Some(Hit::Corner(0, side, Side::Top)));
            let (mid, far) = match side {
                Side::Left => (to(8, 5), to(23, 11)),
                _ => (to(16, 5), to(1, 11)),
            };
            let mut d = Drag::press(&l, from).unwrap();
            d.motion(&mut l, mid, 0);
            assert_eq!(at(&l, 0), step);
            d.motion(&mut l, far, 0);
            assert_eq!(at(&l, 0), end, "stops at the 3 x 2 minimum");
            assert_eq!(what(d.release(&mut l, far, 0)), Some("resized"));
            assert_eq!(l.memory["a0"].cells, end);
            let r = o.cells_rect(end);
            assert_eq!(l.memory["a0"].size, (r.w, r.h));
        }

        let mut l = with(&[c(6, 0, 12, 6)]);
        assert_eq!(l.zone(Hit::Edge(0, Side::Top)), None);
        let from = on_strip(&l, 0, 20);
        assert_eq!(drag(&mut l, from, &[(to(8, 2), 0)]), Some("resized"));
        assert_eq!(at(&l, 0), c(8, 2, 10, 4));
        let from = on_strip(&l, 0, 20);
        assert_eq!(
            drag(&mut l, from, &[((o.x_edge(4), -500), 0)]),
            Some("resized")
        );
        assert_eq!(at(&l, 0), c(4, 0, 14, 6));

        assert!(l.maximize(0));
        let (c, _) = l.frame(0).unwrap();
        for x in c.x..c.x + c.w {
            assert!(!matches!(l.hit((x, c.y + 2)), Some(Hit::Corner(..))), "{x}");
        }
    }

    /// Job 016 AC10: a top corner, hovered or pressed, lights its side zone, its top zone where
    /// there is one, and its strip segment.
    #[test]
    fn ac10_top_corners_light_side_top_and_strip() {
        for row in [0, 3] {
            let mut l = with(&[c(6, row, 12, 6)]);
            let (c, r) = l.frame(0).unwrap();
            for (side, p, segment) in [
                (
                    Side::Left,
                    on_strip(&l, 0, 20),
                    Rect {
                        x: r.x,
                        y: c.y,
                        w: CORNER_PX - GAP / 2,
                        h: STRIP,
                    },
                ),
                (
                    Side::Right,
                    (c.x + c.w - 2, c.y + 20),
                    Rect {
                        x: c.x + c.w - CORNER_PX,
                        y: c.y,
                        w: CORNER_PX - GAP / 2,
                        h: STRIP,
                    },
                ),
            ] {
                assert_eq!(l.hit(p), Some(Hit::Corner(0, side, Side::Top)));
                let mut want: Vec<Rect> = [side, Side::Top]
                    .into_iter()
                    .filter_map(|s| l.zone(Hit::Edge(0, s)))
                    .collect();
                want.push(segment);
                assert_eq!(want.len(), if row == 0 { 2 } else { 3 });
                let lit = |l: &Layout, d: Option<&Drag>| {
                    zones(l, d, p, 0)
                        .into_iter()
                        .map(|(z, k)| {
                            assert_eq!(k, accent(1.0));
                            z
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(lit(&l, None), want, "row {row} {side:?} hovered");
                let d = Drag::press(&l, p).unwrap();
                assert_eq!(lit(&l, Some(&d)), want, "row {row} {side:?} pressed");
                d.cancel(&mut l);
            }
        }
    }

    #[test]
    fn degenerate_output_and_wild_pointers_dont_panic() {
        let mut l = Layout::default();
        l.set_outputs(vec![Output {
            name: "tiny".into(),
            area: Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 5,
            },
            full: Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 5,
            },
            priority: 0,
        }]);
        l.place("a", (0, 0)).unwrap();
        for p in [
            (0, 0),
            (5, 2),
            (9, 4),
            (i32::MIN, i32::MIN),
            (i32::MAX, i32::MAX),
        ] {
            zones(&l, None, p, 0);
            if let Some(mut d) = Drag::press(&l, p) {
                d.motion(&mut l, (i32::MAX, i32::MIN), 0);
                zones(&l, Some(&d), p, 0);
                d.release(&mut l, (i32::MIN, i32::MAX), u64::MAX);
            }
        }
        // Job 016 AC7: grab offsets and pointers at the extremes, on a tiny and a real grid.
        let wild = [0, 1, -1, i32::MIN, i32::MAX];
        for o in [l.outputs[0].clone(), uw()] {
            for &(x, y, gx, gy) in &[(0, 0, 0, 0), (i32::MIN, i32::MAX, i32::MAX, i32::MIN)] {
                for span in [(1, 1), (COLS, ROWS), (3, 2)] {
                    for v in wild {
                        let k = candidate(&o, span, (x ^ v, y), (gx, gy ^ v));
                        assert!(k.col >= 0 && k.col + k.cols <= COLS, "{k:?}");
                        assert!(k.row >= 0 && k.row + k.rows <= ROWS, "{k:?}");
                    }
                }
            }
        }
        let mut l = with(&[c(6, 3, 12, 6)]);
        let mut d = Drag::press(&l, on_strip(&l, 0, 400)).unwrap();
        for p in [
            (i32::MIN, i32::MIN),
            (i32::MAX, i32::MAX),
            (i32::MIN, 0),
            (0, i32::MAX),
        ] {
            d.motion(&mut l, p, 0);
            let k = preview(&d).unwrap();
            assert!(k.col >= 0 && k.col + k.cols <= COLS && k.row + k.rows <= ROWS);
        }
        d.release(&mut l, (i32::MAX, i32::MIN), 0);
        assert_valid(&l);
    }

    // Client minimums (job 017).

    /// Halves where tile 0's client minimum, 995 x 710, needs 8 x 7 cells.
    fn late_halves() -> Layout {
        let mut l = halves();
        assert_eq!(l.set_hints(0, (995, 710), (0, 0)), crate::layout::Fit::Fits);
        assert_eq!(l.floor(0), (8, 7));
        l
    }

    #[test]
    fn ac8_every_resize_path_stops_at_the_client_minimum() {
        let mut l = late_halves();
        assert_eq!(drag(&mut l, (1718, 700), &[(col(2), 0)]), Some("resized"));
        assert_eq!((at(&l, 0), at(&l, 1)), (c(0, 0, 8, 12), c(8, 0, 16, 12)));
        // Growing or moving the neighbour into it does nothing.
        assert_eq!(
            unchanged(&mut l, 1, Side::Left, Nudge::Grow),
            "neighbour at minimum"
        );
        assert_eq!(
            unchanged(&mut l, 1, Side::Left, Nudge::Move),
            "neighbour at minimum"
        );

        let mut l = with(&[c(0, 0, 12, 12)]);
        l.set_hints(0, (995, 710), (0, 0));
        let (frame, _) = l.frame(0).unwrap();
        let from = (frame.x + frame.w - 2, frame.y + frame.h - 2);
        assert_eq!(l.hit(from), Some(Hit::Corner(0, Side::Right, Side::Bottom)));
        assert_eq!(drag(&mut l, from, &[((0, 0), 0)]), Some("resized"));
        assert_eq!(at(&l, 0), c(0, 0, 8, 7));

        let mut l = late_halves();
        let p = l.zone(Hit::Edge(1, Side::Left)).unwrap().center();
        let mut clicks = EdgeClicks::default();
        assert!(edge_click(&mut l, &mut clicks, p, 0).is_none());
        assert_eq!(
            what(edge_click(&mut l, &mut clicks, p, 100)),
            Some("expanded")
        );
        assert_eq!((at(&l, 0), at(&l, 1)), (c(0, 0, 8, 12), c(8, 0, 16, 12)));

        let mut l = late_halves();
        while nudge(&mut l, 0, Side::Right, Nudge::Shrink).is_some() {}
        while nudge(&mut l, 0, Side::Bottom, Nudge::Shrink).is_some() {}
        assert_eq!(at(&l, 0), c(0, 0, 8, 7));
        assert_eq!(
            unchanged(&mut l, 0, Side::Left, Nudge::Shrink),
            "tile at minimum"
        );
    }

    #[test]
    fn ac7_a_relaxed_minimum_resizes_down_to_navis() {
        let navi = uw().span_for((MIN_W, MIN_H));
        let relax = |l: &mut Layout| {
            assert_eq!(l.set_hints(0, (0, 0), (0, 0)), crate::layout::Fit::Fits);
        };
        let mut l = late_halves();
        relax(&mut l);
        assert_eq!(drag(&mut l, (1718, 700), &[(col(0), 0)]), Some("resized"));
        assert_eq!(at(&l, 0).cols, navi.0);

        let mut l = late_halves();
        relax(&mut l);
        let p = l.zone(Hit::Edge(1, Side::Left)).unwrap().center();
        let mut clicks = EdgeClicks::default();
        edge_click(&mut l, &mut clicks, p, 0);
        edge_click(&mut l, &mut clicks, p, 100);
        assert_eq!(at(&l, 0).cols, navi.0);
    }

    #[test]
    fn ac9_tabs_and_swaps_respect_minimums() {
        // 1410 px high fits 12 rows alone, not below a tab bar.
        for who in [0, 1] {
            let mut l = halves();
            assert_eq!(
                l.set_hints(who, (0, 1410), (0, 0)),
                crate::layout::Fit::Fits
            );
            let tiles = l.tiles.clone();
            assert_eq!(
                drag(&mut l, A_STRIP, &[(B_MIDDLE, 0), (B_MIDDLE, 500)]),
                None
            );
            assert_eq!(l.tiles, tiles, "no tab commits");
            assert_eq!(drag(&mut l, A_STRIP, &[(B_RIM, 0)]), Some("swapped"));
        }
        let mut l = with(&[c(0, 0, 16, 12), c(16, 0, 8, 12)]);
        l.set_hints(0, (1500, 0), (0, 0));
        let tiles = l.tiles.clone();
        assert_eq!(
            drag(&mut l, A_STRIP, &[(B_RIM, 0)]),
            None,
            "8 columns can't hold it"
        );
        assert_eq!(l.tiles, tiles);

        // A group's minimum is its largest member's.
        let mut l = with(&[c(0, 0, 12, 12), c(0, 0, 12, 12)]);
        l.set_hints(0, (995, 0), (0, 0));
        assert_eq!(l.set_hints(1, (1500, 0), (0, 0)), crate::layout::Fit::Fits);
        assert_eq!((l.floor(0).0, l.floor(1).0), (11, 11));

        // An untab lands at least at its minimum, though it remembers less.
        l.memory.get_mut("a1").unwrap().size = (860, 1440);
        let tab = l
            .bar(0)
            .unwrap()
            .tabs
            .into_iter()
            .find(|t| t.0 == 1)
            .unwrap()
            .1;
        assert_eq!(
            drag(&mut l, tab.center(), &[((3000, 700), 0)]),
            Some("untabbed")
        );
        assert!(at(&l, 1).cols >= 11, "{:?}", at(&l, 1));

        // Placement tabs only into a tile big enough, and floats when none is.
        let full = [
            c(0, 0, 9, 12),
            c(9, 0, 3, 12),
            c(12, 0, 3, 12),
            c(15, 0, 3, 12),
            c(18, 0, 3, 12),
            c(21, 0, 3, 12),
        ];
        let mut l = with(&full);
        let hints = crate::layout::Hints::new((900, 0), (0, 0));
        let (_, step) = l.place_with("seven", (3400, 10), hints, false).unwrap();
        assert_eq!(step, crate::layout::Step::Tabbed(0));
        let hints = crate::layout::Hints::new((1300, 0), (0, 0));
        let (id, step) = l.place_with("ten", (3400, 10), hints, false).unwrap();
        assert_eq!(step, crate::layout::Step::Floating);
        assert!(l.tile(id).unwrap().floating);
        assert_valid(&l);
    }
}
