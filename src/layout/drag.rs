//! Moving and resizing tiles with the mouse and keyboard, as docs/design.md describes.

use super::{
    COLS, Cells, GAP, Layout, MIN_H, MIN_W, Memory, Output, ROWS, Rect, Tile, WinId,
    bar::{self, Button},
    dist2,
};
use crate::theme::{ACCENT, MINIMIZED, rgba};

/// A press on a strip becomes a move once the pointer travels this far, in px.
const ARM_PX: u64 = 8;
const CORNER_PX: i32 = 32;
const DOUBLE_CLICK_MS: u64 = 400;
/// Holding a move over a tile's middle this long, in ms, tabs instead of swapping.
const HOLD_MS: u64 = 500;
const GRID_ALPHA: f32 = 0.2;
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
        // One set of zones per group, from its active tile; none for a fullscreen tile or one it covers.
        let ids = || {
            self.tiles
                .iter()
                .filter(|t| self.active(t.id) == Some(t.id) && !self.covered(t) && !t.fullscreen)
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
            .find(|&h| self.zone(h).is_some_and(|z| z.contains(p)))?;
        let Hit::Strip(id) = hit else {
            return Some(self.corner_hit(hit, p));
        };
        let Some(bar) = self.bar(id) else {
            return Some(hit);
        };
        if let Some(&(b, _)) = bar.buttons.iter().find(|(_, r)| r.contains(p)) {
            return Some(Hit::Button(id, b));
        }
        match bar.tabs.is_empty() {
            true => Some(hit),
            // The 2 px between tabs are no zone.
            false => bar
                .tabs
                .iter()
                .find(|(_, r)| r.contains(p))
                .map(|&(t, _)| Hit::Tab(t)),
        }
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
                y: c.y - GAP / 2,
                w: r.w,
                h: GAP / 2,
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

    fn tiles_in(&self, ids: &[WinId]) -> Vec<Tile> {
        let has = |t: &&Tile| ids.contains(&t.id);
        self.tiles.iter().filter(has).cloned().collect()
    }

    /// Remembers where `tiles` are now, and their size too when `resized`.
    fn learn(&mut self, tiles: &[Tile], resized: bool) {
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

    /// Where a move of `moving` (`id`'s group, or `id` alone out of it) lands with the pointer at
    /// `p`, on which output.
    fn move_target(&self, id: WinId, moving: &[WinId], p: (i32, i32)) -> Option<(String, Target)> {
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
            .find(|u| u.output == o.name && o.cells_rect(u.cells).contains(p));
        let target = match under {
            Some(u) if group.contains(&u.id) => Target::Own(u.cells),
            Some(u) => Target::Onto(u.id, u.cells),
            None => {
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
                let size = self.memory.get(&t.app).map_or((MIN_W, MIN_H), |m| m.size);
                rest.nearest_free(o, size, p)
                    .or_else(|| rest.smaller_free(o, size, p))
                    .map_or(Target::Nowhere, Target::Free)
            }
        };
        Some((o.name.clone(), target))
    }

    /// Moves the dragged edge to the cell boundary nearest `p`, from the `before` snapshot, as far
    /// as every changed tile stays at least the minimum, on the grid, and clear of other tiles.
    /// Returns whether any cells changed.
    fn resize_to(&mut self, r: &Resize, edge: &ResizeEdge, p: (i32, i32)) -> bool {
        let Some(o) = self.output(&r.output) else {
            return false;
        };
        let x = matches!(edge.side, Side::Left | Side::Right);
        let (min_c, min_r) = o.span_for((MIN_W, MIN_H));
        let (n, min) = if x { (COLS, min_c) } else { (ROWS, min_r) };
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
                    (len >= min && start >= 0 && start + len <= n && clear).then_some((id, c))
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
        let Hit::Edge(_, side) = hit else { return None };
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
    pub fn keyboard_adjust(&mut self, id: WinId, side: Side, op: Nudge) -> Option<Done> {
        let t = self.tile(id)?;
        if !self.visible(id) || self.maximized(t) || t.fullscreen {
            return None;
        }
        let o = self.output(&t.output)?;
        let x = matches!(side, Side::Left | Side::Right);
        let far = matches!(side, Side::Right | Side::Bottom);
        let (min_c, min_r) = o.span_for((MIN_W, MIN_H));
        let (n, min) = if x { (COLS, min_c) } else { (ROWS, min_r) };
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
        if (op == Nudge::Shrink && len < min) || start < 0 || start + len > n {
            return None;
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
                if l - 1 < min {
                    return None;
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
        Some(Done::Layout(what, self.tiles_in(&ids)))
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
        output: String,
        target: Target,
        /// The tile whose middle the pointer is in, and since when.
        middle: Option<(WinId, u64)>,
    },
    Resize(Resize),
    Cancelled,
}

/// A left-button drag that started in a navi zone. Lives from press to release.
pub struct Drag(State);

impl Drag {
    /// A left press at `p`: arms a click or move on a strip, tab or button, starts a resize on an
    /// edge or corner, `None` elsewhere.
    pub fn press(l: &Layout, p: (i32, i32)) -> Option<Drag> {
        let state = match l.hit(p)? {
            hit @ (Hit::Strip(_) | Hit::Tab(_) | Hit::Button(..)) => State::Armed { hit, from: p },
            hit @ (Hit::Edge(..) | Hit::Corner(..)) => State::Resize(Resize::new(l, hit, p)?),
        };
        Some(Drag(state))
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
            self.0 = match hit {
                // A maximized group doesn't move; a button press stays a click.
                Hit::Strip(_) | Hit::Tab(_) if l.is_maximized(id) => State::Cancelled,
                Hit::Strip(_) | Hit::Tab(_) => State::Move {
                    id,
                    tab,
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
                output,
                target,
                middle,
            } => match l.move_target(*id, &l.moving(*id, *tab), p) {
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
            _ => {}
        }
        false
    }

    /// Ends the drag without a drop: a resize puts every tile back. Returns whether cells changed.
    pub fn cancel(self, l: &mut Layout) -> bool {
        match self.0 {
            State::Resize(r) => l.restore(&r.before),
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
            } => {
                let moving = l.moving(id, tab);
                let held = held(middle, now_ms);
                let (what, ids) = match target {
                    Target::Own(_) | Target::Nowhere => return None,
                    Target::Free(c) => {
                        l.put(&moving, &output, c);
                        (if tab { "untabbed" } else { "moved" }, moving)
                    }
                    Target::Onto(_, c) if held => {
                        l.put(&moving, &output, c);
                        ("tabbed", moving)
                    }
                    // The tabs left behind would overlap the swapped tile.
                    Target::Onto(..) if tab => return None,
                    Target::Onto(u, c) => {
                        let theirs = l.group(u);
                        let (own_output, own) = l.tile(id).map(|t| (t.output.clone(), t.cells))?;
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

/// Solid rects to draw over the windows: global logical px, premultiplied RGBA, front to back.
/// During a move, the grid and a preview of the landing tile, more solid once the drop would tab;
/// otherwise the hovered or pressed zone. Then every bar, the dimmed strip of each minimized
/// untabbed tile, and last a frame around each shown window (and its bar): bright around
/// `focused`, dim around the rest.
pub fn overlay(
    l: &Layout,
    drag: Option<&Drag>,
    p: (i32, i32),
    now_ms: u64,
    focused: Option<WinId>,
) -> Vec<(Rect, [f32; 4])> {
    let mut out = Vec::new();
    let hover = match drag.map(|d| &d.0) {
        None => l.hit(p),
        Some(State::Armed { hit, .. }) => Some(*hit),
        Some(State::Resize(r)) => Some(r.hit),
        Some(State::Move {
            id,
            tab,
            output,
            target,
            middle,
        }) => {
            let held = held(*middle, now_ms);
            // The preview is where the window lands: below a bar if it will have one.
            let bar = match target {
                Target::Onto(..) if held => true,
                _ if *tab => l.tile(*id).is_some_and(|t| t.decorated),
                _ => l.has_bar(*id),
            };
            let top = if bar { super::BAR } else { GAP };
            let alpha = if held { TAB_ALPHA } else { PREVIEW_ALPHA };
            // A tab can't swap, so over a tile only a hold that will tab shows where it lands.
            let target = match target {
                Target::Onto(..) if *tab && !held => Target::Nowhere,
                t => *t,
            };
            if let Some(o) = l.output(output) {
                out.extend(move_overlay(o, target, top, alpha));
            }
            None
        }
        Some(State::Cancelled) => None,
    };
    // A lit strip stops at the buttons.
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
    out.extend(lit.map(|z| (z, accent(1.0))));
    if let Some(Hit::Corner(id, x, y)) = hover {
        for side in [x, y] {
            out.extend(l.zone(Hit::Edge(id, side)).map(|z| (z, accent(1.0))));
        }
    }
    // Where a frame's top goes when it runs along the top of a bar, in front of it.
    let front = out.len();
    for t in &l.tiles {
        if l.active(t.id) != Some(t.id) || l.covered(t) || t.fullscreen {
            continue;
        }
        let tabbed = l.group(t.id).len() > 1;
        if t.minimized && !tabbed {
            out.extend(l.zone(Hit::Strip(t.id)).map(|z| (z, rgba(MINIMIZED, 1.0))));
        } else if let Some(b) = l.bar(t.id) {
            out.extend(bar::draw(l, t.id, &b, hover));
        }
    }
    for t in &l.tiles {
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
                let [top, rest @ ..] = frame(r, px);
                out.insert(front, (top, k));
                out.extend(rest.map(|f| (f, k)));
            }
            None => out.extend(frame(r, px).map(|f| (f, k))),
        }
    }
    out
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

/// The output's interior cell boundaries, 1 px wide, and the preview where the window would go.
fn move_overlay(o: &Output, target: Target, top: i32, alpha: f32) -> Vec<(Rect, [f32; 4])> {
    let a = o.area;
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
    cols.chain(rows)
        .map(|r| (r, accent(GRID_ALPHA)))
        .chain(
            target
                .cells()
                .map(|c| (o.window_rect(c, top), accent(alpha))),
        )
        .collect()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// The overlay with no focus and without the dim frames every shown tile has.
    pub(in crate::layout) fn zones(
        l: &Layout,
        drag: Option<&Drag>,
        p: (i32, i32),
        now_ms: u64,
    ) -> Vec<(Rect, [f32; 4])> {
        let mut out = overlay(l, drag, p, now_ms, None);
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
        for (i, t) in l.tiles.iter().enumerate() {
            let k = t.cells;
            assert!(k.col >= 0 && k.row >= 0, "{k:?}");
            assert!(k.col + k.cols <= COLS && k.row + k.rows <= ROWS, "{k:?}");
            assert!(k.cols >= min_c && k.rows >= min_r, "{k:?} below minimum");
            for u in &l.tiles[i + 1..] {
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
    const A_STRIP: (i32, i32) = (100, 2);
    const B_MIDDLE: (i32, i32) = (2500, 700);
    const B_RIM: (i32, i32) = (1800, 100);

    fn halves() -> Layout {
        with(&[c(0, 0, 12, 12), c(12, 0, 12, 12)])
    }

    #[test]
    fn ac1_hit_zones_are_gap_px() {
        let l = halves();
        // a's window spans x 4..1716, y 8..1436; b's starts at x 1724.
        assert_eq!(l.hit(A_STRIP), Some(Hit::Strip(0)));
        assert_eq!(l.hit((1715, 7)), Some(Hit::Strip(0)));
        assert_eq!(l.hit((1720, 700)), Some(Hit::Edge(1, Side::Left)));
        assert_eq!(l.hit((1723, 700)), Some(Hit::Edge(1, Side::Left)));
        assert_eq!(l.hit((1716, 700)), Some(Hit::Edge(0, Side::Right)));
        assert_eq!(l.hit((1719, 700)), Some(Hit::Edge(0, Side::Right)));
        assert_eq!(l.hit((500, 1437)), Some(Hit::Edge(0, Side::Bottom)));
        assert_eq!(l.hit((500, 700)), None);
        assert_eq!(l.hit((4, 8)), None);
        for x in 0..3440 {
            for y in 0..GAP {
                assert!(!matches!(l.hit((x, y)), Some(Hit::Edge(_, Side::Top))));
            }
        }
        // Below row 0 the top edge exists, and the bottom zone of the tile above wins it.
        let l = with(&[c(0, 0, 12, 6), c(0, 6, 12, 6), c(12, 6, 12, 6)]);
        assert_eq!(l.hit((500, 718)), Some(Hit::Edge(0, Side::Bottom)));
        assert_eq!(l.hit((2500, 718)), Some(Hit::Edge(2, Side::Top)));
        assert_eq!(l.hit((-5, -5)), None);
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

        let tiles = l.tiles.clone();
        assert_eq!(
            drag(&mut l, A_STRIP, &[((3000, 700), 0), ((800, 700), 10)]),
            None
        );
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
        // Job 003: the bar is all tabs, and A_STRIP is in tile 0's.
        let mut l = with(&[c(0, 0, 12, 12), c(0, 0, 12, 12)]);
        assert_eq!(drag(&mut l, A_STRIP, &[((3000, 700), 0)]), Some("untabbed"));
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
        let what = what(l.keyboard_adjust(id, side, op));
        assert_valid(l);
        what
    }

    /// A nudge that changes nothing: no result, and tiles and memory as they were.
    fn unchanged(l: &mut Layout, id: WinId, side: Side, op: Nudge) {
        let (tiles, memory) = (l.tiles.clone(), l.memory.clone());
        assert!(
            l.keyboard_adjust(id, side, op).is_none(),
            "{id} {side:?} {op:?}"
        );
        assert_eq!(l.tiles, tiles, "{id} {side:?} {op:?}");
        assert_eq!(l.memory, memory, "{id} {side:?} {op:?}");
    }

    /// The ids and cells a nudge reports, sorted by id.
    fn changed(l: &mut Layout, id: WinId, side: Side, op: Nudge) -> Vec<(WinId, Cells)> {
        let Some(Done::Layout(_, tiles)) = l.keyboard_adjust(id, side, op) else {
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
            unchanged(&mut l, 0, side, Nudge::Grow);
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
        unchanged(&mut l, 0, Side::Right, Nudge::Grow);
        let mut l = with(&[c(0, 0, 24, 10), c(0, 10, 24, 2)]);
        unchanged(&mut l, 0, Side::Bottom, Nudge::Grow);
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
            unchanged(&mut l, 0, side, Nudge::Shrink);
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
        assert!(l.keyboard_adjust(0, Side::Right, Nudge::Grow).is_some());
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
    fn ac13_overlay() {
        let mut l = halves();
        assert!(zones(&l, None, (500, 700), 0).is_empty());
        let strip = Rect {
            x: 4,
            y: 0,
            w: 1712,
            h: GAP,
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
            uw().window_rect(c(12, 0, 12, 12), super::super::BAR)
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
        let o = overlay(&l, None, (500, 700), 0, Some(0));
        assert_eq!(o.len(), 8);
        framed(&o, left, FOCUS_PX, 1.0);
        framed(&o, right, IDLE_PX, IDLE_ALPHA);
        assert_eq!(accent(IDLE_ALPHA), rgba(ACCENT, 0.3));

        let o = overlay(&l, None, (500, 700), 0, None);
        assert!(o.iter().all(|&(_, k)| k == accent(IDLE_ALPHA)));
        framed(&o, left, IDLE_PX, IDLE_ALPHA);
        framed(&o, right, IDLE_PX, IDLE_ALPHA);

        // A hovered strip is in front of its tile's frame.
        let o = overlay(&l, None, A_STRIP, 0, Some(0));
        let strip = o.iter().position(|&(r, _)| r.h == GAP).unwrap();
        let frame = o
            .iter()
            .position(|&(r, _)| r.w.min(r.h) == FOCUS_PX)
            .unwrap();
        assert!(strip < frame);

        // Minimized, under a maximize and fullscreen: no frame.
        let frames_of = |l: &Layout, id: WinId| {
            let o = overlay(l, None, (500, 700), 0, Some(id));
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
        assert!(overlay(&l, None, (500, 700), 0, Some(1)).is_empty());

        // Round 1: a decorated window's frame takes in its bar; the top line runs along the bar's
        // top rows in front of it, and the sides start at the bar's top.
        let mut l = halves();
        l.set_decorated(0, true);
        let (bar, win) = (l.bar(0).unwrap().rect, l.rect(0).unwrap());
        let o = overlay(&l, None, (500, 700), 0, Some(0));
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
    }
}
