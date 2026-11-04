//! Moving and resizing tiles with the mouse, as docs/design.md "Dragging" says.

use super::{
    COLS, Cells, GAP, Layout, MIN_H, MIN_W, Memory, Output, ROWS, Rect, Tile, WinId,
    bar::{self, ACCENT, Button, DIM},
    dist2,
};

/// A press on a strip becomes a move once the pointer travels this far, in px.
const ARM_PX: u64 = 8;
/// Holding a move over a tile's middle this long, in ms, tabs instead of swapping.
const HOLD_MS: u64 = 500;
const GRID_ALPHA: f32 = 0.2;
const PREVIEW_ALPHA: f32 = 0.35;
/// Once a hold over a tile's middle will tab, the preview turns this solid.
const TAB_ALPHA: f32 = 0.7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

/// A navi zone in the gaps: a tile's grab strip (or the free part of its bar), one of its edges,
/// a tab, or a window button of the group's active tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Strip(WinId),
    Edge(WinId, Side),
    Tab(WinId),
    Button(WinId, Button),
}

impl Hit {
    pub fn id(self) -> WinId {
        let (Hit::Strip(id) | Hit::Edge(id, _) | Hit::Tab(id) | Hit::Button(id, _)) = self;
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
        // One set of zones per group, from its active tile, and none under a maximized group.
        let ids = || {
            self.tiles
                .iter()
                .filter(|t| self.active(t.id) == Some(t.id) && !self.covered(t))
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
            return Some(hit);
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

    /// The px of a zone, all inside the gaps around the tile's window.
    pub(super) fn zone(&self, hit: Hit) -> Option<Rect> {
        let t = self.tile(hit.id())?;
        let (c, r) = self.frame(t.id)?;
        Some(match hit {
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
            let fresh = Memory {
                output: t.output.clone(),
                cells: t.cells,
                size: (r.w, r.h),
            };
            let m = self.memory.entry(t.app.clone()).or_insert(fresh.clone());
            m.output = fresh.output;
            m.cells = fresh.cells;
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
    fn resize_to(&mut self, r: &Resize, p: (i32, i32)) -> bool {
        let Some(o) = self.output(&r.output) else {
            return false;
        };
        let x = matches!(r.side, Side::Left | Side::Right);
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
        let to = (0..=n).min_by_key(|&k| off(k)).unwrap_or(r.from);
        let is_changed = |id: WinId| r.changed.iter().any(|&(c, _)| c == id);
        let cells_at = |at: i32| -> Option<Vec<(WinId, Cells)>> {
            r.changed
                .iter()
                .map(|&(id, far)| {
                    let b = r.before.iter().find(|t| t.id == id)?.cells;
                    let (start, len) = span(b, x);
                    let (start, len) = match far {
                        true => (start, at - start),
                        false => (at, start + len - at),
                    };
                    let c = with_span(b, x, start, len);
                    let clear = !self
                        .tiles
                        .iter()
                        .any(|t| t.output == r.output && !is_changed(t.id) && t.cells.overlaps(&c));
                    (len >= min && start >= 0 && start + len <= n && clear).then_some((id, c))
                })
                .collect()
        };
        let step = (to - r.from).signum();
        let (mut at, mut cells) = (r.from, cells_at(r.from).unwrap_or_default());
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
    bar::rgba(ACCENT, alpha)
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
    id: WinId,
    side: Side,
    output: String,
    /// The edge's cell boundary at the press.
    from: i32,
    before: Vec<Tile>,
    /// The tiles that move with the edge, and whether it is their far side (right or bottom).
    changed: Vec<(WinId, bool)>,
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
    /// edge, `None` elsewhere.
    pub fn press(l: &Layout, p: (i32, i32)) -> Option<Drag> {
        let state = match l.hit(p)? {
            hit @ (Hit::Strip(_) | Hit::Tab(_) | Hit::Button(..)) => State::Armed { hit, from: p },
            Hit::Edge(id, side) => {
                let t = l.tile(id)?;
                let x = matches!(side, Side::Left | Side::Right);
                let far = matches!(side, Side::Right | Side::Bottom);
                let (start, len) = span(t.cells, x);
                let from = if far { start + len } else { start };
                let (a, alen) = span(t.cells, !x);
                // The group, plus tiles whose near side lies on the edge and overlaps its span.
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
                        let near = if far { s } else { s + n };
                        (near == from && a < b + blen && b < a + alen).then_some((u.id, !far))
                    })
                    .collect();
                State::Resize(Resize {
                    id,
                    side,
                    output: t.output.clone(),
                    from,
                    before: l.tiles.clone(),
                    changed,
                })
            }
        };
        Some(Drag(state))
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
            State::Resize(r) if l.tile(r.id).is_none() => {
                // Undo, so a window closing mid-drag leaves the rest as they were.
                let changed = l.restore(&r.before);
                self.0 = State::Cancelled;
                return changed;
            }
            State::Resize(r) => return l.resize_to(r, p),
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
                let tiles = l.tiles_in(&ids);
                l.learn(&tiles, false);
                Some(Done::Layout(what, tiles))
            }
            State::Resize(r) => {
                let moved = |t: &&Tile| r.before.iter().any(|b| b.id == t.id && b.cells != t.cells);
                let tiles: Vec<Tile> = l.tiles.iter().filter(moved).cloned().collect();
                l.learn(&tiles, true);
                (!tiles.is_empty()).then_some(Done::Layout("resized", tiles))
            }
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
            Hit::Edge(..) => None,
        }
    }
}

/// Solid rects to draw over the windows: global logical px, premultiplied RGBA, front to back.
/// During a move, the grid and a preview of the landing tile, more solid once the drop would tab;
/// otherwise the hovered or pressed zone. Then every bar, and the dimmed strip of each minimized
/// untabbed tile.
pub fn overlay(
    l: &Layout,
    drag: Option<&Drag>,
    p: (i32, i32),
    now_ms: u64,
) -> Vec<(Rect, [f32; 4])> {
    let mut out = Vec::new();
    let hover = match drag.map(|d| &d.0) {
        None => l.hit(p),
        Some(State::Armed { hit, .. }) => Some(*hit),
        Some(State::Resize(r)) => Some(Hit::Edge(r.id, r.side)),
        Some(State::Move {
            tab,
            output,
            target,
            middle,
            ..
        }) => {
            let held = held(*middle, now_ms);
            let alpha = if held { TAB_ALPHA } else { PREVIEW_ALPHA };
            // A tab can't swap, so over a tile only a hold that will tab shows where it lands.
            let target = match target {
                Target::Onto(..) if *tab && !held => Target::Nowhere,
                t => *t,
            };
            if let Some(o) = l.output(output) {
                out.extend(move_overlay(o, target, alpha));
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
    for t in &l.tiles {
        if l.active(t.id) != Some(t.id) || l.covered(t) {
            continue;
        }
        let tabbed = l.group(t.id).len() > 1;
        if t.minimized && !tabbed {
            out.extend(l.zone(Hit::Strip(t.id)).map(|z| (z, accent(DIM))));
        } else if let Some(b) = l.bar(t.id) {
            out.extend(bar::draw(l, t.id, &b, hover));
        }
    }
    out
}

/// The output's interior cell boundaries, 1 px wide, and the preview where the window would go.
fn move_overlay(o: &Output, target: Target, alpha: f32) -> Vec<(Rect, [f32; 4])> {
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
        .chain(target.cells().map(|c| (o.tile_rect(c), accent(alpha))))
        .collect()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

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

    // Points on the ultrawide. Cells are 286 x 240 px; a is cols 0..6, b cols 6..12.
    const A_STRIP: (i32, i32) = (100, 2);
    const B_MIDDLE: (i32, i32) = (2500, 700);
    const B_RIM: (i32, i32) = (1800, 100);

    fn halves() -> Layout {
        with(&[c(0, 0, 6, 6), c(6, 0, 6, 6)])
    }

    #[test]
    fn ac1_hit_zones_are_gap_px() {
        let l = halves();
        // a's window spans x 4..1712, y 8..1436; b's starts at x 1720.
        assert_eq!(l.hit(A_STRIP), Some(Hit::Strip(0)));
        assert_eq!(l.hit((1711, 7)), Some(Hit::Strip(0)));
        assert_eq!(l.hit((1716, 700)), Some(Hit::Edge(1, Side::Left)));
        assert_eq!(l.hit((1719, 700)), Some(Hit::Edge(1, Side::Left)));
        assert_eq!(l.hit((1712, 700)), Some(Hit::Edge(0, Side::Right)));
        assert_eq!(l.hit((1715, 700)), Some(Hit::Edge(0, Side::Right)));
        assert_eq!(l.hit((500, 1437)), Some(Hit::Edge(0, Side::Bottom)));
        assert_eq!(l.hit((500, 700)), None);
        assert_eq!(l.hit((4, 8)), None);
        for x in 0..3440 {
            for y in 0..GAP {
                assert!(!matches!(l.hit((x, y)), Some(Hit::Edge(_, Side::Top))));
            }
        }
        // Below row 0 the top edge exists, and the bottom zone of the tile above wins it.
        let l = with(&[c(0, 0, 6, 3), c(0, 3, 6, 3), c(6, 3, 6, 3)]);
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
        assert_eq!(at(&l, 0), c(6, 0, 6, 6));
        assert_eq!(at(&l, 1), c(0, 0, 6, 6));
        assert_eq!(l.memory["a0"].cells, c(6, 0, 6, 6));
        assert_eq!(l.memory["a1"].cells, c(0, 0, 6, 6));
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
        assert_eq!(at(&l, 1), c(6, 0, 6, 6));

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
        let mut l = with(&[c(0, 0, 6, 6)]);
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        d.motion(&mut l, (3000, 700), 0);
        assert_eq!(preview(&d), Some(c(6, 0, 6, 6)));
        let Some(Done::Layout(what, tiles)) = d.release(&mut l, (3000, 700), 10) else {
            panic!("not moved");
        };
        assert_eq!((what, tiles[0].cells), ("moved", c(6, 0, 6, 6)));
        assert_eq!(at(&l, 0), c(6, 0, 6, 6));
        assert_eq!(l.memory["a0"].cells, c(6, 0, 6, 6));

        let mut l = with(&[c(0, 0, 6, 6)]);
        l.memory.get_mut("a0").unwrap().size = (1144, 1440);
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        d.motion(&mut l, (3000, 700), 0);
        assert_eq!(preview(&d), Some(c(8, 0, 4, 6)));
        d.release(&mut l, (3000, 700), 10);
        assert_eq!(at(&l, 0), c(8, 0, 4, 6));
        assert_eq!(l.memory["a0"].size, (1144, 1440));
        assert_valid(&l);
    }

    #[test]
    fn ac6_preview_stays_on_the_grid_and_own_cells_cancel() {
        let mut l = with(&[c(0, 0, 6, 6)]);
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
            priority: 1,
        };
        l.set_outputs(vec![uw(), laptop]);
        for (id, cells) in [(1, c(0, 0, 5, 6)), (2, c(6, 0, 6, 6))] {
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
            overlay(&l, Some(&d), gap, 0).len(),
            (COLS - 1 + ROWS - 1) as usize
        );
        assert!(d.release(&mut l, gap, 10).is_none());
        assert_eq!(l.tiles, tiles);
    }

    #[test]
    fn ac7_dragging_a_tab_moves_only_that_tab() {
        // Job 003: the bar is all tabs, and A_STRIP is in tile 0's.
        let mut l = with(&[c(0, 0, 6, 6), c(0, 0, 6, 6)]);
        assert_eq!(drag(&mut l, A_STRIP, &[((3000, 700), 0)]), Some("untabbed"));
        assert_eq!(at(&l, 0), c(6, 0, 6, 6));
        assert_eq!(at(&l, 1), c(0, 0, 6, 6));
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
        let mut d = Drag::press(&l, (1714, 700)).unwrap();
        for (k, a, b) in [
            (8, c(0, 0, 8, 6), c(8, 0, 4, 6)),
            (11, c(0, 0, 10, 6), c(10, 0, 2, 6)),
            (0, c(0, 0, 2, 6), c(2, 0, 10, 6)),
            (6, c(0, 0, 6, 6), c(6, 0, 6, 6)),
        ] {
            d.motion(&mut l, col(k), 0);
            assert_eq!((at(&l, 0), at(&l, 1)), (a, b), "to {k}");
            assert_valid(&l);
        }
        assert!(d.release(&mut l, col(6), 0).is_none());

        assert_eq!(drag(&mut l, (1714, 700), &[(col(8), 0)]), Some("resized"));
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
        let first = [c(0, 0, 6, 3), c(6, 0, 6, 6), c(0, 3, 6, 3)];
        let mut l = with(&first);
        drag(&mut l, (1714, 100), &[(col(8), 0)]);
        assert_eq!(at(&l, 0), c(0, 0, 8, 3));
        assert_eq!(at(&l, 1), c(8, 0, 4, 6));
        assert_eq!(at(&l, 2), c(0, 3, 6, 3));

        // c grows into the empty cells and stops at b.
        drag(&mut l, (1714, 1000), &[(col(10), 0)]);
        assert_eq!(at(&l, 2), c(0, 3, 8, 3));
        assert_eq!(at(&l, 1), c(8, 0, 4, 6));

        let mut l = with(&first);
        drag(&mut l, (1718, 700), &[(col(4), 0)]);
        assert_eq!(at(&l, 0), c(0, 0, 4, 3));
        assert_eq!(at(&l, 2), c(0, 3, 4, 3));
        assert_eq!(at(&l, 1), c(4, 0, 8, 6));
    }

    #[test]
    fn ac10_vertical_edges() {
        let mut l = with(&[c(0, 0, 6, 3), c(0, 3, 6, 3)]);
        let mut d = Drag::press(&l, (500, 718)).unwrap();
        d.motion(&mut l, row(4), 0);
        assert_eq!((at(&l, 0), at(&l, 1)), (c(0, 0, 6, 4), c(0, 4, 6, 2)));
        d.motion(&mut l, row(6), 0);
        assert_eq!((at(&l, 0), at(&l, 1)), (c(0, 0, 6, 5), c(0, 5, 6, 1)));
        assert_valid(&l);
        assert_eq!(what(d.release(&mut l, row(6), 0)), Some("resized"));

        // The top edge of the lower tile is the same boundary.
        let mut l = with(&[c(0, 0, 6, 3), c(0, 3, 6, 3), c(6, 3, 6, 3)]);
        drag(&mut l, (2500, 718), &[(row(1), 0)]);
        assert_eq!(at(&l, 2), c(6, 1, 6, 5));
        assert_eq!(at(&l, 0), c(0, 0, 6, 3));
    }

    #[test]
    fn ac12_a_vanished_tile_cancels() {
        let mut l = halves();
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        d.motion(&mut l, B_RIM, 0);
        l.remove(0);
        d.motion(&mut l, (3000, 700), 10);
        assert!(d.release(&mut l, (3000, 700), 20).is_none());
        assert_eq!(at(&l, 1), c(6, 0, 6, 6));

        // A resize undoes what it did to the others.
        let mut l = halves();
        let mut d = Drag::press(&l, (1714, 700)).unwrap();
        d.motion(&mut l, col(8), 0);
        assert_eq!(at(&l, 1), c(8, 0, 4, 6));
        l.remove(0);
        assert!(d.motion(&mut l, col(9), 10));
        assert!(d.release(&mut l, col(9), 20).is_none());
        assert_eq!(at(&l, 1), c(6, 0, 6, 6));

        // An armed press too.
        let mut l = halves();
        let mut d = Drag::press(&l, A_STRIP).unwrap();
        l.remove(0);
        d.motion(&mut l, A_STRIP, 0);
        assert!(overlay(&l, Some(&d), A_STRIP, 0).is_empty());
        assert!(d.release(&mut l, B_RIM, 0).is_none());
    }

    #[test]
    fn cancel_puts_everything_back() {
        // A resize mid-way: both tiles return to where they were.
        let mut l = halves();
        let tiles = l.tiles.clone();
        let mut d = Drag::press(&l, (1714, 700)).unwrap();
        d.motion(&mut l, col(9), 0);
        assert_eq!(at(&l, 1), c(9, 0, 3, 6));
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
        assert!(overlay(&l, None, (500, 700), 0).is_empty());
        let strip = Rect {
            x: 4,
            y: 0,
            w: 1708,
            h: GAP,
        };
        assert_eq!(overlay(&l, None, A_STRIP, 0), vec![(strip, accent(1.0))]);
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
        assert_eq!(
            overlay(&l, Some(&d), A_STRIP, 0),
            vec![(strip, accent(1.0))]
        );
        d.motion(&mut l, B_RIM, 0);
        let rects = overlay(&l, Some(&d), B_RIM, 0);
        assert_eq!(rects.len(), (COLS - 1 + ROWS - 1 + 1) as usize);
        let lines = &rects[..rects.len() - 1];
        assert!(lines.iter().all(|(r, _)| r.w == 1 || r.h == 1));
        assert!(lines.iter().all(|&(_, k)| k == accent(GRID_ALPHA)));
        // The preview is where the window itself would go: the tile rect.
        assert_eq!(rects.last().unwrap().0, uw().tile_rect(c(6, 0, 6, 6)));
        assert_eq!(rects.last().unwrap().1, accent(PREVIEW_ALPHA));

        // Swap and tab previews look different: more solid once the hold has elapsed.
        d.motion(&mut l, B_MIDDLE, 1000);
        let preview_at = |t| *overlay(&l, Some(&d), B_MIDDLE, t).last().unwrap();
        assert_eq!(preview_at(1499).1, accent(PREVIEW_ALPHA));
        assert_eq!(preview_at(1500).1, accent(TAB_ALPHA));
        assert_eq!(preview_at(1500).0, preview_at(1499).0);
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
            overlay(&l, None, p, 0);
            if let Some(mut d) = Drag::press(&l, p) {
                d.motion(&mut l, (i32::MAX, i32::MIN), 0);
                overlay(&l, Some(&d), p, 0);
                d.release(&mut l, (i32::MIN, i32::MAX), u64::MAX);
            }
        }
    }
}
