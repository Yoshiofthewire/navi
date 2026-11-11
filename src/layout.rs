//! The grid layout from docs/design.md. Pure geometry, no Wayland types.

use std::collections::HashMap;

mod bar;
pub mod dialog;
mod drag;
mod float;
mod memory;
pub use bar::{Button, Decor, decoration};
pub use drag::{
    Action, Done, Drag, EdgeClicks, Hit, Nudge, Side, buttons, dialog_frame, float_overlay, overlay,
};
use memory::MAX_PX;
pub use memory::safe;

pub const COLS: i32 = 24;
pub const ROWS: i32 = 12;
/// The visible space between tiles, and from a tile to the screen edge or a panel, in logical px.
pub const GAP: i32 = 12;
/// The grab strip above each tile, inside the gap; `GAP - STRIP` is left below the tile.
pub const STRIP: i32 = 8;
/// A tab bar's height, between the strip and a tabbed or server-decorated window.
/// 24 px is WCAG 2.5.8's minimum target size.
pub const BAR: i32 = 24;
pub const MIN_W: i32 = 320;
pub const MIN_H: i32 = 240;
/// A float's distance from the panels and screen edges, while it fits.
const FLOAT_MARGIN: i32 = STRIP;

pub type WinId = u64;

/// The grid inside `zone`, inset so an edge tile's window (or bar) is `GAP` from the zone's edge:
/// the tile's own gap supplies the rest.
pub fn grid_in(zone: Rect) -> Rect {
    Rect {
        x: zone.x + GAP / 2,
        y: zone.y + GAP - STRIP,
        w: zone.w - GAP,
        h: zone.h - GAP,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    pub fn contains(&self, (x, y): (i32, i32)) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// How far a client's popup positioner may reach, in logical px: far past any output. With every
/// coordinate within ±this and every size in 1..=this, Smithay's placement sums stay inside i32.
pub const POPUP_LIMIT: i32 = 1 << 20;

/// A client's positioner coordinate, bounded to `POPUP_LIMIT`.
pub fn popup_coord(v: i32) -> i32 {
    v.clamp(-POPUP_LIMIT, POPUP_LIMIT)
}

/// A client's positioner rect, bounded to `POPUP_LIMIT`; sizes stay at least 1.
pub fn popup_rect(r: Rect) -> Rect {
    let size = |v: i32| v.clamp(1, POPUP_LIMIT);
    Rect::new(popup_coord(r.x), popup_coord(r.y), size(r.w), size(r.h))
}

/// How many popups deep a chain may go: it bounds the per-commit walks of the chain, and as navi
/// places each popup within ±4 · `POPUP_LIMIT` of its parent, the chain's position stays exact in
/// i32 (Smithay's `Point` sums saturate, so a longer chain would pin instead of overflow).
pub const POPUP_DEPTH: usize = 16;
const _: () = assert!(POPUP_DEPTH as i64 * 4 * POPUP_LIMIT as i64 <= i32::MAX as i64 / 2);

/// Whether the popup chain from `popup` up through `parent` (the next popup, if any) is too deep.
/// Walks at most `POPUP_DEPTH` steps.
pub fn popup_too_deep<T>(popup: T, parent: impl Fn(&T) -> Option<T>) -> bool {
    let mut at = popup;
    for _ in 0..POPUP_DEPTH {
        match parent(&at) {
            Some(p) => at = p,
            None => return false,
        }
    }
    true
}

/// A rectangle of whole grid cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cells {
    pub col: i32,
    pub row: i32,
    pub cols: i32,
    pub rows: i32,
}

impl Cells {
    fn overlaps(&self, o: &Cells) -> bool {
        self.col < o.col + o.cols
            && o.col < self.col + self.cols
            && self.row < o.row + o.rows
            && o.row < self.row + self.rows
    }
}

#[derive(Clone, Debug)]
pub struct Output {
    pub name: String,
    /// The grid's px in the global coordinate space: what layer surfaces leave of the output.
    pub area: Rect,
    /// The whole output, which a fullscreen window covers.
    pub full: Rect,
    /// Lower is preferred.
    pub priority: u32,
}

impl Output {
    /// The smallest cell; even boundaries make every cell this or 1 px more.
    fn cell_w(&self) -> i32 {
        (self.area.w / COLS).max(1)
    }

    fn cell_h(&self) -> i32 {
        (self.area.h / ROWS).max(1)
    }

    // Even boundaries: cells differ by at most 1 px, and halving the grid keeps every boundary.
    fn x_edge(&self, col: i32) -> i32 {
        self.area.x + (i64::from(col) * i64::from(self.area.w) / i64::from(COLS)) as i32
    }

    fn y_edge(&self, row: i32) -> i32 {
        self.area.y + (i64::from(row) * i64::from(self.area.h) / i64::from(ROWS)) as i32
    }

    /// The px covered by `c`, gap included.
    pub fn cells_rect(&self, c: Cells) -> Rect {
        let (x, y) = (self.x_edge(c.col), self.y_edge(c.row));
        Rect {
            x,
            y,
            w: self.x_edge(c.col + c.cols) - x,
            h: self.y_edge(c.row + c.rows) - y,
        }
    }

    /// Where the window itself goes: `cells_rect` minus the gap.
    pub fn tile_rect(&self, c: Cells) -> Rect {
        self.window_rect(c, STRIP)
    }

    /// `tile_rect` with `top` px above the window instead of `STRIP`.
    fn window_rect(&self, c: Cells, top: i32) -> Rect {
        let r = self.cells_rect(c);
        Rect {
            x: r.x + GAP / 2,
            y: r.y + top,
            w: (r.w - GAP).max(1),
            h: (r.h - top - (GAP - STRIP)).max(1),
        }
    }

    /// The fewest cells whose narrowest run, along x or y, is at least `px`; `None` if the whole
    /// grid isn't. With even boundaries the first `k` cells are a narrowest run of `k`.
    fn fewest(&self, x: bool, px: i32) -> Option<i32> {
        match x {
            true => (1..=COLS).find(|&k| self.x_edge(k) - self.area.x >= px),
            false => (1..=ROWS).find(|&k| self.y_edge(k) - self.area.y >= px),
        }
    }

    /// The smallest span whose window, below a bar if `bar`, is at least `min` at every grid
    /// position, and at least navi's minimum. `None` if even the whole grid is too small.
    fn min_span(&self, min: (i32, i32), bar: bool) -> Option<(i32, i32)> {
        let navi = self.span_for((MIN_W, MIN_H));
        let cols = self.fewest(true, min.0 + GAP)?;
        let rows = self.fewest(false, min.1 + GAP + if bar { BAR } else { 0 })?;
        Some((cols.max(navi.0), rows.max(navi.1)))
    }

    /// The span in cells nearest to a px size, never below the minimum tile size.
    pub fn span_for(&self, (w, h): (i32, i32)) -> (i32, i32) {
        let fit = |px: i32, cell: i32, min: i32, max: i32| {
            let min_cells = ((min + cell - 1) / cell).min(max);
            ((px + cell / 2) / cell).clamp(min_cells, max)
        };
        (
            fit(w, self.cell_w(), MIN_W, COLS),
            fit(h, self.cell_h(), MIN_H, ROWS),
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tile {
    pub id: WinId,
    pub app: String,
    pub output: String,
    pub cells: Cells,
    /// The app asked navi to draw its controls (xdg-decoration server-side).
    pub decorated: bool,
    pub minimized: bool,
    /// Set on every member of a maximized group.
    pub maximized: bool,
    /// Covers its whole output, layer surfaces included; only ever set on a visible tile.
    pub fullscreen: bool,
    /// When it was last placed, moved or activated; a group shows its most recent member.
    pub stamp: u64,
    /// Where it was before its output was unplugged; it goes back when that output returns,
    /// unless the user moved it since.
    pub home: Option<(String, Cells)>,
    pub hints: Hints,
    /// Too big for any tile: centred above the tiles, holding no cells.
    pub floating: bool,
    /// A float's window rect once the user moved or resized it; never remembered.
    pub user_rect: Option<Rect>,
    /// A dialog of its family waits for the user (refused the keyboard): its tab is marked.
    pub attention: bool,
    /// Its family's newest raise, from navi's one stacking order: floats are ordered by it first.
    pub rank: u64,
}

impl Tile {
    /// Itself, or a tile on the same output and cells: tabbed together. Floats never tab.
    fn tabbed(&self, o: &Tile) -> bool {
        self.id == o.id
            || (!self.floating && !o.floating && self.output == o.output && self.cells == o.cells)
    }
}

/// A client's minimum and maximum window size in logical px; 0 on an axis is no limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hints {
    pub min: (i32, i32),
    pub max: (i32, i32),
}

impl Hints {
    /// Untrusted values capped to `0..=MAX_PX`; a max axis below its min axis is dropped.
    pub fn new(min: (i32, i32), max: (i32, i32)) -> Hints {
        let cap = |v: i32| v.clamp(0, MAX_PX);
        let axis = |lo: i32, hi: i32| {
            let (lo, hi) = (cap(lo), cap(hi));
            (lo, if hi < lo { 0 } else { hi })
        };
        let ((min_w, max_w), (min_h, max_h)) = (axis(min.0, max.0), axis(min.1, max.1));
        Hints {
            min: (min_w, min_h),
            max: (max_w, max_h),
        }
    }

    /// `r` no bigger than the max, anchored at its top-left.
    fn clamp(&self, r: Rect) -> Rect {
        let cap = |v: i32, max: i32| if max > 0 { v.min(max) } else { v };
        Rect {
            w: cap(r.w, self.max.0),
            h: cap(r.h, self.max.1),
            ..r
        }
    }
}

/// What a spot must give a window: the px it wants, gap and bar included, and its client
/// minimum.
#[derive(Clone, Copy, Debug)]
struct Need {
    size: (i32, i32),
    min: (i32, i32),
    bar: bool,
}

impl Need {
    /// The span `o` gives it and its minimum span there; `None` if the minimum doesn't fit `o`.
    fn spans(&self, o: &Output) -> Option<((i32, i32), (i32, i32))> {
        let min = o.min_span(self.min, self.bar)?;
        let (c, r) = o.span_for(self.size);
        Some(((c.max(min.0), r.max(min.1)), min))
    }

    /// Its window, at `c` on `o`, is at least its minimum.
    fn holds(&self, o: &Output, c: Cells) -> bool {
        let r = o.window_rect(c, if self.bar { STRIP + BAR } else { STRIP });
        r.w >= self.min.0 && r.h >= self.min.1
    }
}

/// What `set_hints` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fit {
    /// The same hints as before.
    Unchanged,
    /// The tile's cells still hold the window.
    Fits,
    /// The tile moved through the placement order.
    Replaced(String, Cells),
    /// No tile fits: it floats.
    Floated,
    /// A float found a tile.
    Tiled(String, Cells),
}

/// What navi remembers per app ID: where it was, and the size it wants, in px.
#[derive(Clone, Debug, PartialEq)]
struct Memory {
    output: String,
    cells: Cells,
    size: (i32, i32),
    /// The layout clock when it was last placed, moved or resized; the save cap keeps the newest.
    stamp: u64,
}

/// Which step of the placement order found the spot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Remembered,
    Nearest,
    Smaller,
    Shrunk,
    OtherOutput,
    Tabbed(WinId),
    /// No step fits: it floats.
    Floating,
}

#[derive(Default)]
pub struct Layout {
    outputs: Vec<Output>,
    tiles: Vec<Tile>,
    memory: HashMap<String, Memory>,
    next_id: WinId,
    clock: u64,
}

impl Layout {
    /// An output that wasn't there before takes back the tiles whose home it is.
    pub fn set_outputs(&mut self, mut outputs: Vec<Output>) {
        outputs.sort_by_key(|o| o.priority);
        let new: Vec<String> = outputs
            .iter()
            .filter(|o| self.output(&o.name).is_none())
            .map(|o| o.name.clone())
            .collect();
        self.outputs = outputs;
        for name in new {
            self.return_home(&name);
        }
        self.pin_floats();
    }

    /// Tiles whose home is `output` go back to their cells there, by id, where nothing else is.
    /// Either way their home is forgotten.
    fn return_home(&mut self, output: &str) {
        let mut ids: Vec<WinId> = self
            .tiles
            .iter()
            .filter(|t| t.home.as_ref().is_some_and(|h| h.0 == output))
            .map(|t| t.id)
            .collect();
        ids.sort_unstable();
        let mut back: Vec<WinId> = Vec::new();
        for id in ids {
            let Some(t) = self.tile_mut(id) else {
                continue;
            };
            let Some((_, cells)) = t.home.take() else {
                continue;
            };
            // Tabs coming home together share their cells.
            let free = !self.tiles.iter().any(|u| {
                u.output == output
                    && u.cells.overlaps(&cells)
                    && !(back.contains(&u.id) && u.cells == cells)
            });
            if free && let Some(t) = self.tile_mut(id) {
                t.output = output.to_string();
                t.cells = cells;
                t.floating = false;
                t.user_rect = None;
                t.maximized = false;
                t.fullscreen = false;
                back.push(id);
            }
        }
    }

    /// Moves `name`'s tiles, a tab group at a time, to the other outputs with the placement steps
    /// from the main output's centre, floating them on it if nothing fits, and forgets `name`.
    /// Floats go through the same steps. Memory doesn't change.
    pub fn remove_output(&mut self, name: &str) {
        self.outputs.retain(|o| o.name != name);
        let Some(main) = self.outputs.first().cloned() else {
            return;
        };
        let mut ids: Vec<WinId> = self
            .tiles
            .iter()
            .filter(|t| t.output == name)
            .map(|t| t.id)
            .collect();
        ids.sort_unstable();
        while !ids.is_empty() {
            let id = ids.remove(0);
            let group = self.group(id);
            ids.retain(|i| !group.contains(i));
            let half = (main.area.w / 2, main.area.h);
            let Some(need) = self.need(id, &group, self.has_bar(id), half) else {
                continue;
            };
            let spot = self.find_spot(&main, need, main.area.center(), None);
            for t in self.tiles.iter_mut().filter(|t| group.contains(&t.id)) {
                if !t.floating {
                    t.home = t.home.take().or(Some((t.output.clone(), t.cells)));
                }
                (t.output, t.cells, t.floating) = match &spot {
                    Some((output, cells, _)) => (output.clone(), *cells, false),
                    None => (main.name.clone(), Cells::default(), true),
                };
                t.user_rect = None;
                t.maximized = false;
                t.fullscreen = false;
            }
        }
    }

    /// What `app` needs with `hints`: its remembered px size, else `fallback`, lowered to its max
    /// and raised to its min, with the gap and a bar if `bar`.
    fn need_for(&self, app: &str, hints: Hints, bar: bool, fallback: (i32, i32)) -> Need {
        let (w, h) = self.memory.get(app).map_or(fallback, |m| m.size);
        let fit = |v: i32, lo: i32, hi: i32, extra: i32| {
            let v = if hi > 0 { v.min(hi + extra) } else { v };
            v.max(lo + extra)
        };
        let top = GAP + if bar { BAR } else { 0 };
        Need {
            size: (
                fit(w, hints.min.0, hints.max.0, GAP),
                fit(h, hints.min.1, hints.max.1, top),
            ),
            min: hints.min,
            bar,
        }
    }

    /// What moving `ids` needs: `id`'s size, and the largest of their minimums.
    fn need(&self, id: WinId, ids: &[WinId], bar: bool, fallback: (i32, i32)) -> Option<Need> {
        let t = self.tile(id)?;
        let need = self.need_for(&t.app, t.hints, bar, fallback);
        let (w, h) = self.largest_min(ids);
        Some(Need {
            min: (need.min.0.max(w), need.min.1.max(h)),
            ..need
        })
    }

    /// The fewest cells `id`'s group can have: navi's minimum, or a member's client minimum below
    /// the group's bar where that is larger.
    fn floor(&self, id: WinId) -> (i32, i32) {
        let Some((t, o)) = self
            .tile(id)
            .and_then(|t| Some((t, self.output(&t.output)?)))
        else {
            return (COLS, ROWS);
        };
        let min = largest_min(self.tiles.iter().filter(|u| u.tabbed(t)));
        o.min_span(min, self.has_bar(id)).unwrap_or((COLS, ROWS))
    }

    /// The largest client minimum of `ids`, per axis.
    fn largest_min(&self, ids: &[WinId]) -> (i32, i32) {
        largest_min(self.tiles.iter().filter(|t| ids.contains(&t.id)))
    }

    /// Every one of `ids` fits `c` on `o`, below a bar if `bar`, at its minimum span.
    fn fit_in(&self, ids: &[WinId], o: &Output, c: Cells, bar: bool) -> bool {
        span_holds(o, c, self.largest_min(ids), bar)
    }

    /// Stores `id`'s client hints, sanitised. A tile whose cells no longer hold its window, and a
    /// float, go through the placement order again from where they are; memory learns a new
    /// tile's spot and px size.
    pub fn set_hints(&mut self, id: WinId, min: (i32, i32), max: (i32, i32)) -> Fit {
        let hints = Hints::new(min, max);
        let Some(t) = self.tile_mut(id).filter(|t| t.hints != hints) else {
            return Fit::Unchanged;
        };
        t.hints = hints;
        let fit = self.refit(id);
        self.pin_floats();
        fit
    }

    /// Whether the app asked navi to draw its controls. A tile whose window no longer fits below
    /// its new bar is refitted as `set_hints` does; a float always has the bar.
    pub fn set_decorated(&mut self, id: WinId, on: bool) -> Fit {
        let Some(t) = self.tile_mut(id).filter(|t| t.decorated != on) else {
            return Fit::Unchanged;
        };
        t.decorated = on;
        match t.floating {
            true => Fit::Fits,
            false => self.refit(id),
        }
    }

    /// Places `id` again if its cells no longer hold its window, or if it floats.
    fn refit(&mut self, id: WinId) -> Fit {
        let Some(t) = self.tile(id).cloned() else {
            return Fit::Unchanged;
        };
        let Some(o) = self.output(&t.output).cloned() else {
            return Fit::Fits;
        };
        let hints = t.hints;
        let need = Need {
            size: (0, 0),
            min: hints.min,
            bar: self.has_bar(id),
        };
        if !t.floating && need.holds(&o, t.cells) {
            return Fit::Fits;
        }
        let pointer = self.frame(id).map_or(o.area.center(), |(c, _)| c.center());
        let Some((output, cells, step)) = self.settle(id, &o, pointer) else {
            return Fit::Floated;
        };
        if !matches!(step, Step::Tabbed(_)) {
            let tiles = self.tiles_in(&[id]);
            self.learn(&tiles, true);
        }
        match t.floating {
            true => Fit::Tiled(output, cells),
            false => Fit::Replaced(output, cells),
        }
    }

    /// Takes `id` out of its cells and places it again from `pointer` on `here`, alone. It floats
    /// on `here` if nothing fits.
    fn settle(
        &mut self,
        id: WinId,
        here: &Output,
        pointer: (i32, i32),
    ) -> Option<(String, Cells, Step)> {
        let i = self.tiles.iter().position(|t| t.id == id)?;
        let mut t = self.tiles.remove(i);
        let half = (here.area.w / 2, here.area.h);
        let need = self.need_for(&t.app, t.hints, t.decorated, half);
        let mem = self.memory.get(&t.app).cloned();
        let spot = self.find_spot(here, need, pointer, mem.as_ref());
        (t.output, t.cells, t.floating) = match &spot {
            Some((output, cells, _)) => (output.clone(), *cells, false),
            None => (here.name.clone(), Cells::default(), true),
        };
        // A float that stays one keeps the user's rect, clamped to its new hints.
        t.user_rect = t.user_rect.filter(|_| spot.is_none());
        t.maximized = false;
        t.fullscreen = false;
        self.tiles.insert(i, t);
        spot
    }

    /// A float's window, below its bar: the user's rect if it has one (see `placed_float`), else
    /// its wanted size, centred in its bounds. Where that's below the app's minimum it takes what
    /// the minimum needs of the whole output, past the panels (which stay on top), but its bar
    /// never goes above the area, so its buttons stay in reach.
    fn float_rect(&self, t: &Tile, o: &Output) -> Rect {
        let (f, a) = (o.full, float::bounds(o));
        let need = self.need_for(&t.app, t.hints, true, (a.w / 2, a.h));
        let (min_w, min_h) = t.hints.min;
        let bottom = f.y + f.h;
        let w = (need.size.0 - GAP).min(a.w).max(min_w).min(f.w).max(1);
        let h = (need.size.1 - GAP - BAR).min(a.h - BAR).max(min_h);
        let h = h.min(bottom - o.area.y - BAR).max(1);
        let x = match w <= a.w {
            true => a.x + (a.w - w) / 2,
            false => f.x + (f.w - w) / 2,
        };
        let outer = h + BAR;
        let top = match outer <= a.h {
            true => a.y + (a.h - outer) / 2,
            false => a.y.min(bottom - outer).max(o.area.y),
        };
        let centred = Rect {
            x,
            y: top + BAR,
            w,
            h,
        };
        float::placed(t, o, centred)
    }

    /// `id` floats with a minimum bigger than its output can show, so part of it is cut.
    pub fn too_small(&self, id: WinId) -> bool {
        let Some((t, o)) = self
            .tile(id)
            .and_then(|t| Some((t, self.output(&t.output)?)))
        else {
            return false;
        };
        let r = self.float_rect(t, o);
        t.floating && (r.w < t.hints.min.0 || r.h < t.hints.min.1)
    }

    pub fn tiles(&self) -> &[Tile] {
        &self.tiles
    }

    /// Sets `id`'s stacking rank (`Tile::rank`).
    pub fn set_rank(&mut self, id: WinId, rank: u64) {
        if let Some(t) = self.tile_mut(id) {
            t.rank = rank;
        }
    }

    /// Marks exactly the tiles in `ids` as wanting attention. Returns whether any mark changed.
    pub fn set_attention(&mut self, ids: &[WinId]) -> bool {
        let mut changed = false;
        for t in &mut self.tiles {
            let want = ids.contains(&t.id);
            changed |= t.attention != want;
            t.attention = want;
        }
        changed
    }

    pub fn tile(&self, id: WinId) -> Option<&Tile> {
        self.tiles.iter().find(|t| t.id == id)
    }

    /// The window's rect in global logical px: clamped to its max, a float's, or the whole output
    /// while fullscreen.
    pub fn rect(&self, id: WinId) -> Option<Rect> {
        let t = self.tile(id)?;
        match t.fullscreen {
            true => Some(self.output(&t.output)?.full),
            false => Some(self.frame(id)?.1),
        }
    }

    /// The rect `id`'s window is drawn and hit inside. Dialogs, which aren't tiles, have none.
    pub fn clip(&self, id: WinId) -> Option<Rect> {
        self.rect(id)
    }

    /// The px of `id`'s cells (the whole grid while maximized) and of its window, which sits below
    /// a bar if it has one, anchored top-left and clamped to its max. A float's bar and window.
    fn frame(&self, id: WinId) -> Option<(Rect, Rect)> {
        let t = self.tile(id)?;
        let o = self.output(&t.output)?;
        if t.floating {
            let r = self.float_rect(t, o);
            let bar = Rect {
                y: r.y - BAR,
                h: r.h + BAR,
                ..r
            };
            return Some((bar, r));
        }
        let cells = match self.maximized(t) {
            true => FULL,
            false => t.cells,
        };
        let top = if self.has_bar(id) { STRIP + BAR } else { STRIP };
        Some((
            o.cells_rect(cells),
            t.hints.clamp(o.window_rect(cells, top)),
        ))
    }

    /// Closing leaves the cells empty; nothing else moves.
    pub fn remove(&mut self, id: WinId) {
        self.tiles.retain(|t| t.id != id);
    }

    /// `id` and the tiles tabbed with it, by id.
    fn group(&self, id: WinId) -> Vec<WinId> {
        let Some(t) = self.tile(id) else {
            return Vec::new();
        };
        let mut ids: Vec<WinId> = self
            .tiles
            .iter()
            .filter(|u| u.tabbed(t))
            .map(|u| u.id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// The tile `id`'s group shows: its most recently activated member that isn't minimized,
    /// else its most recent one.
    pub fn active(&self, id: WinId) -> Option<WinId> {
        let t = self.tile(id)?;
        self.tiles
            .iter()
            .filter(|u| u.tabbed(t))
            .max_by_key(|u| (!u.minimized, u.stamp))
            .map(|u| u.id)
    }

    fn maximized(&self, t: &Tile) -> bool {
        self.tiles.iter().any(|u| u.maximized && u.tabbed(t))
    }

    /// Another window is fullscreen on its output, or another group is maximized over its tile.
    fn covered(&self, t: &Tile) -> bool {
        self.tiles.iter().any(|u| {
            u.output == t.output
                && ((u.maximized && !t.floating && !u.tabbed(t)) || (u.fullscreen && u.id != t.id))
        })
    }

    pub fn is_fullscreen(&self, id: WinId) -> bool {
        self.tile(id).is_some_and(|t| t.fullscreen)
    }

    /// A window is fullscreen on `output`.
    pub fn fullscreen_on(&self, output: &str) -> bool {
        self.tiles
            .iter()
            .any(|t| t.fullscreen && t.output == output)
    }

    pub fn is_maximized(&self, id: WinId) -> bool {
        self.tile(id).is_some_and(|t| self.maximized(t))
    }

    /// Shown on screen: not minimized, its group's active tile, and not under a maximized group.
    pub fn visible(&self, id: WinId) -> bool {
        self.tile(id)
            .is_some_and(|t| !t.minimized && self.active(id) == Some(id) && !self.covered(t))
    }

    /// Tabbed, floating, or showing a window that wants navi's controls. A float always has
    /// them, so it can be minimized or closed whatever the app draws.
    pub fn has_bar(&self, id: WinId) -> bool {
        let controls = self
            .active(id)
            .and_then(|a| self.tile(a))
            .is_some_and(|a| a.decorated || a.floating);
        controls || self.group(id).len() > 1
    }

    fn tile_mut(&mut self, id: WinId) -> Option<&mut Tile> {
        self.tiles.iter_mut().find(|t| t.id == id)
    }

    /// Makes `id` the most recent member of its group.
    fn touch(&mut self, id: WinId) {
        self.clock += 1;
        let now = self.clock;
        if let Some(t) = self.tile_mut(id) {
            t.stamp = now;
        }
    }

    fn set_maximized(&mut self, id: WinId, on: bool) {
        let group = self.group(id);
        for t in self.tiles.iter_mut().filter(|t| group.contains(&t.id)) {
            t.maximized = on;
        }
    }

    /// Ends every maximize and fullscreen on `output`.
    fn clear_maximized(&mut self, output: &str) {
        for t in self.tiles.iter_mut().filter(|t| t.output == output) {
            t.maximized = false;
            t.fullscreen = false;
        }
    }

    /// Covers `id`'s whole output with its group, uncovering any other. The cells stay as they
    /// are. Returns whether anything changed; a hidden or unknown `id` changes nothing.
    pub fn maximize(&mut self, id: WinId) -> bool {
        let Some(t) = self.tile(id).filter(|_| self.visible(id)) else {
            return false;
        };
        if self.maximized(t) || t.floating {
            return false;
        }
        let output = t.output.clone();
        self.clear_maximized(&output);
        self.set_maximized(id, true);
        true
    }

    pub fn unmaximize(&mut self, id: WinId) -> bool {
        let changed = self.visible(id) && self.is_maximized(id);
        if changed {
            self.set_maximized(id, false);
        }
        changed
    }

    /// Hides `id`, keeping its cells, and ends its group's maximize and its fullscreen.
    pub fn minimize(&mut self, id: WinId) -> bool {
        if !self.visible(id) {
            return false;
        }
        self.set_maximized(id, false);
        if let Some(t) = self.tile_mut(id) {
            t.minimized = true;
            t.fullscreen = false;
        }
        true
    }

    /// Covers `id`'s whole output with it. A hidden or unknown `id` changes nothing.
    pub fn fullscreen(&mut self, id: WinId) -> bool {
        if !self.visible(id) || self.is_fullscreen(id) {
            return false;
        }
        if let Some(t) = self.tile_mut(id) {
            t.fullscreen = true;
        }
        true
    }

    /// Back to its tile, or to its maximized group.
    pub fn unfullscreen(&mut self, id: WinId) -> bool {
        let Some(t) = self.tile_mut(id).filter(|t| t.fullscreen) else {
            return false;
        };
        t.fullscreen = false;
        true
    }

    /// Ends every fullscreen on `output`, as `unfullscreen` does; returns the windows it ended.
    pub fn unfullscreen_output(&mut self, output: &str) -> Vec<WinId> {
        let on = self
            .tiles
            .iter_mut()
            .filter(|t| t.fullscreen && t.output == output);
        on.map(|t| {
            t.fullscreen = false;
            t.id
        })
        .collect()
    }

    /// Focus or restore: shows `id` as its group's active tile, restoring it if minimized, and
    /// ends any other maximize or fullscreen covering it on its output.
    pub fn activate(&mut self, id: WinId) -> bool {
        let Some(t) = self.tile(id) else {
            return false;
        };
        let mut changed = t.minimized || self.active(id) != Some(id);
        if changed {
            self.touch(id);
            if let Some(t) = self.tile_mut(id) {
                t.minimized = false;
            }
        }
        let t = self.tile(id).cloned().unwrap_or_default();
        for u in self.tiles.iter_mut().filter(|u| u.output == t.output) {
            // A float is above every maximize.
            let unmaximize = u.maximized && !t.floating && !u.tabbed(&t);
            if unmaximize || (u.fullscreen && u.id != id) {
                u.maximized &= !unmaximize;
                u.fullscreen = false;
                changed = true;
            }
        }
        changed
    }

    /// The fullscreen windows `activate(id)` ends: every other one on its output.
    pub fn fullscreen_ended_by(&self, id: WinId) -> Vec<WinId> {
        let Some(t) = self.tile(id) else {
            return Vec::new();
        };
        let on = self.tiles.iter().filter(|u| u.output == t.output);
        on.filter(|u| u.fullscreen && u.id != id)
            .map(|u| u.id)
            .collect()
    }

    /// Places a new window of `app` with no client hints and no bar.
    pub fn place(&mut self, app: &str, pointer: (i32, i32)) -> Option<(WinId, Step)> {
        self.place_with(app, pointer, Hints::default(), false)
    }

    /// Places a new window of `app`, following the order in docs/design.md; it floats if no step
    /// fits. `None` only when there are no outputs.
    pub fn place_with(
        &mut self,
        app: &str,
        pointer: (i32, i32),
        hints: Hints,
        decorated: bool,
    ) -> Option<(WinId, Step)> {
        let here = self
            .outputs
            .iter()
            .find(|o| o.area.contains(pointer))
            .or(self.outputs.first())?
            .clone();
        let mem = self.memory.get(app).cloned();
        let need = self.need_for(app, hints, decorated, (here.area.w / 2, here.area.h));
        let (output, cells, step) = self
            .find_spot(&here, need, pointer, mem.as_ref())
            .unwrap_or((here.name.clone(), Cells::default(), Step::Floating));

        let id = self.next_id;
        self.next_id += 1;
        // A new window must be visible (principle 6).
        self.clear_maximized(&output);
        self.tiles.push(Tile {
            id,
            app: app.to_string(),
            output: output.clone(),
            cells,
            decorated,
            hints,
            floating: step == Step::Floating,
            ..Tile::default()
        });
        self.touch(id);
        let fresh = Memory {
            output,
            cells,
            size: need.size,
            stamp: self.clock,
        };
        match step {
            // The cells belong to another tile; only learn the size.
            Step::Tabbed(_) => {
                self.memory.entry(app.to_string()).or_insert(fresh).stamp = self.clock;
            }
            Step::Floating => {}
            _ => {
                self.memory.insert(app.to_string(), fresh);
            }
        }
        Some((id, step))
    }

    /// The placement order. `None` when no step fits, so the window floats.
    fn find_spot(
        &mut self,
        here: &Output,
        need: Need,
        pointer: (i32, i32),
        mem: Option<&Memory>,
    ) -> Option<(String, Cells, Step)> {
        if let Some(m) = mem
            && let Some(o) = self.output(&m.output)
            && self.is_free(&m.output, m.cells)
            && need.holds(o, m.cells)
        {
            return Some((m.output.clone(), m.cells, Step::Remembered));
        }
        if let Some((want, min)) = need.spans(here) {
            if let Some(c) = self.nearest_free(here, want, pointer) {
                return Some((here.name.clone(), c, Step::Nearest));
            }
            if let Some(c) = self.smaller_free(here, want, min, pointer) {
                return Some((here.name.clone(), c, Step::Smaller));
            }
            if let Some(c) = self.shrink_for(here, want, min, pointer) {
                return Some((here.name.clone(), c, Step::Shrunk));
            }
        }
        let others: Vec<Output> = self
            .outputs
            .iter()
            .filter(|o| o.name != here.name)
            .cloned()
            .collect();
        for o in &others {
            if let Some((want, _)) = need.spans(o)
                && let Some(c) = self.nearest_free(o, want, o.area.center())
            {
                return Some((o.name.clone(), c, Step::OtherOutput));
            }
        }
        // A tab, below a bar, in the nearest tile that holds it and every member.
        let mut near: Vec<&Tile> = self
            .tiles
            .iter()
            .filter(|t| t.output == here.name && !t.floating)
            .collect();
        near.sort_by_key(|t| dist2(here.cells_rect(t.cells).center(), pointer));
        near.into_iter()
            .find(|t| {
                span_holds(here, t.cells, need.min, true)
                    && self.fit_in(&self.group(t.id), here, t.cells, true)
            })
            .map(|t| (here.name.clone(), t.cells, Step::Tabbed(t.id)))
    }

    pub fn output(&self, name: &str) -> Option<&Output> {
        self.outputs.iter().find(|o| o.name == name)
    }

    /// No tile is in `c`; floats hold no cells.
    fn is_free(&self, output: &str, c: Cells) -> bool {
        !self
            .tiles
            .iter()
            .any(|t| t.output == output && !t.floating && t.cells.overlaps(&c))
    }

    /// The empty rect of `span` whose centre is closest to `pointer`.
    fn nearest_free(&self, o: &Output, span: (i32, i32), pointer: (i32, i32)) -> Option<Cells> {
        let (cols, rows) = span;
        spots(cols, rows)
            .filter(|&c| self.is_free(&o.name, c))
            .min_by_key(|&c| dist2(o.cells_rect(c).center(), pointer))
    }

    /// The largest empty rect between the `min` and `want` spans; ties go to the one nearest
    /// `pointer`.
    fn smaller_free(
        &self,
        o: &Output,
        (want_c, want_r): (i32, i32),
        (min_c, min_r): (i32, i32),
        pointer: (i32, i32),
    ) -> Option<Cells> {
        (min_r..=want_r)
            .flat_map(|rows| (min_c..=want_c).flat_map(move |cols| spots(cols, rows)))
            .filter(|&c| self.is_free(&o.name, c))
            .min_by_key(|&c| (-c.cols * c.rows, dist2(o.cells_rect(c).center(), pointer)))
    }

    /// Takes columns from the tiles in the new window's rows, never below their own floor, until
    /// it gets an equal share (at least its `min` columns), then slides them apart, in order, to
    /// open one gap near `pointer`. Moves nothing on failure.
    fn shrink_for(
        &mut self,
        o: &Output,
        (want_c, rows): (i32, i32),
        (min_c, _): (i32, i32),
        pointer: (i32, i32),
    ) -> Option<Cells> {
        let band = (0..=ROWS - rows)
            .map(|row| Cells {
                col: 0,
                row,
                cols: COLS,
                rows,
            })
            .min_by_key(|&b| (o.cells_rect(b).center().1 - pointer.1).abs())?;
        let (mut row, others): (Vec<usize>, Vec<usize>) = (0..self.tiles.len())
            .filter(|&i| self.tiles[i].output == o.name && !self.tiles[i].floating)
            .partition(|&i| self.tiles[i].cells.overlaps(&band));
        row.sort_by_key(|&i| self.tiles[i].cells.col);
        let cells: Vec<Cells> = row.iter().map(|&i| self.tiles[i].cells).collect();
        // Stacked tiles aren't a row of side-by-side tiles.
        if cells.windows(2).any(|p| p[0].col + p[0].cols > p[1].col) {
            return None;
        }

        let n = cells.len();
        let target = (COLS / (n as i32 + 1)).min(want_c).max(min_c);
        let wants: Vec<i32> = row
            .iter()
            .map(|&i| {
                let app = &self.tiles[i].app;
                self.memory.get(app).map_or(COLS, |m| o.span_for(m.size).0)
            })
            .collect();
        let floors: Vec<i32> = row
            .iter()
            .map(|&i| self.floor(self.tiles[i].id).0)
            .collect();
        let mut widths: Vec<i32> = cells.iter().map(|c| c.cols).collect();
        while COLS - widths.iter().sum::<i32>() < target {
            let v = (0..n)
                .filter(|&k| widths[k] > floors[k])
                .max_by_key(|&k| (widths[k] > wants[k], widths[k]))?;
            widths[v] -= 1;
        }

        // Tiles left of the gap pack to the left edge, the rest to the right edge.
        let (gap, slot) = (0..=n)
            .flat_map(|k| {
                let left: i32 = widths[..k].iter().sum();
                let right = COLS - widths[k..].iter().sum::<i32>();
                (left..=right - target).map(move |col| {
                    let c = Cells {
                        col,
                        cols: target,
                        ..band
                    };
                    (c, k)
                })
            })
            .min_by_key(|&(c, _)| dist2(o.cells_rect(c).center(), pointer))?;
        let moved: Vec<Cells> = cells
            .iter()
            .enumerate()
            .map(|(k, c)| {
                let col = match k < slot {
                    true => widths[..k].iter().sum(),
                    false => COLS - widths[k..].iter().sum::<i32>(),
                };
                Cells {
                    col,
                    cols: widths[k],
                    ..*c
                }
            })
            .collect();
        if moved
            .iter()
            .any(|m| others.iter().any(|&i| self.tiles[i].cells.overlaps(m)))
        {
            return None;
        }
        for (&i, m) in row.iter().zip(moved) {
            self.tiles[i].cells = m;
        }
        Some(gap)
    }
}

const FULL: Cells = Cells {
    col: 0,
    row: 0,
    cols: COLS,
    rows: ROWS,
};

/// Every rect of `cols` x `rows` cells on the grid.
fn spots(cols: i32, rows: i32) -> impl Iterator<Item = Cells> {
    (0..=ROWS - rows).flat_map(move |row| {
        (0..=COLS - cols).map(move |col| Cells {
            col,
            row,
            cols,
            rows,
        })
    })
}

fn largest_min<'a>(tiles: impl Iterator<Item = &'a Tile>) -> (i32, i32) {
    tiles.fold((0, 0), |(w, h), t| {
        (w.max(t.hints.min.0), h.max(t.hints.min.1))
    })
}

/// `c` on `o` is at least the span a window of client minimum `min` needs, below a bar if `bar`.
fn span_holds(o: &Output, c: Cells, min: (i32, i32), bar: bool) -> bool {
    o.min_span(min, bar)
        .is_some_and(|(cols, rows)| c.cols >= cols && c.rows >= rows)
}

/// Squared distance; saturates instead of overflowing on wild pointer coordinates.
fn dist2((ax, ay): (i32, i32), (bx, by): (i32, i32)) -> u64 {
    let d = |a: i32, b: i32| u64::from(a.abs_diff(b)).pow(2);
    d(ax, bx).saturating_add(d(ay, by))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Action, Button, Done, float_overlay};
    use drag::Hit;

    #[test]
    fn popup_chains_stop_at_the_depth_limit() {
        // A chain of `n` popups: `i` is a popup while `i < n`; its parent is `i + 1`.
        let chain = |n: usize| popup_too_deep(0, move |&i: &usize| (i + 1 < n).then_some(i + 1));
        assert!(!chain(1));
        assert!(!chain(POPUP_DEPTH - 1));
        assert!(!chain(POPUP_DEPTH));
        assert!(chain(POPUP_DEPTH + 1));
        assert!(chain(usize::MAX));
    }

    #[test]
    fn popup_values_are_bounded() {
        const L: i32 = POPUP_LIMIT;
        for (v, want) in [
            (i32::MAX, L),
            (i32::MIN, -L),
            (0, 0),
            (L, L),
            (L - 1, L - 1),
            (L + 1, L),
            (-L, -L),
            (-L + 1, -L + 1),
            (-L - 1, -L),
        ] {
            assert_eq!(popup_coord(v), want, "{v}");
        }
        let r = |x, y, w, h| Rect::new(x, y, w, h);
        assert_eq!(
            popup_rect(r(i32::MAX, i32::MIN, i32::MAX, i32::MAX)),
            r(L, -L, L, L)
        );
        assert_eq!(
            popup_rect(r(i32::MIN, i32::MAX, i32::MIN, 0)),
            r(-L, L, 1, 1)
        );
        for v in [1, L - 1, L] {
            assert_eq!(popup_rect(r(v, -v, v, v)), r(v, -v, v, v));
        }
        assert_eq!(popup_rect(r(L + 1, -L - 1, L + 1, L + 1)), r(L, -L, L, L));
        let menu = r(40, 30, 200, 24);
        assert_eq!(popup_rect(menu), menu);
    }

    fn out(name: &str, x: i32, y: i32, w: i32, h: i32, priority: u32) -> Output {
        Output {
            name: name.into(),
            area: Rect { x, y, w, h },
            full: Rect { x, y, w, h },
            priority,
        }
    }

    fn cells(col: i32, row: i32, cols: i32, rows: i32) -> Cells {
        Cells {
            col,
            row,
            cols,
            rows,
        }
    }

    fn ultrawide() -> Output {
        out("uw", 0, 0, 3440, 1440, 0)
    }

    fn uhd() -> Output {
        out("4k", 0, 0, 3840, 2160, 0)
    }

    fn laptop() -> Output {
        out("laptop", 0, 0, 1125, 703, 0)
    }

    fn layout(outputs: Vec<Output>) -> Layout {
        let mut l = Layout::default();
        l.set_outputs(outputs);
        l
    }

    fn cells_of(l: &Layout, id: WinId) -> Cells {
        l.tile(id).unwrap().cells
    }

    /// The rig's outputs, the ultrawide also with a 48 px taskbar and 32 px band, and an odd one.
    fn sample_outputs() -> [Output; 5] {
        [
            ultrawide(),
            out("uw-bars", 0, 32, 3440, 1360, 0),
            uhd(),
            laptop(),
            out("odd", 7, 11, 1001, 599, 0),
        ]
    }

    #[test]
    fn ac1_grid_is_24_by_12_and_covers_the_area() {
        assert_eq!((COLS, ROWS), (24, 12));
        for o in sample_outputs() {
            assert_eq!(o.cells_rect(FULL), o.area, "{}", o.name);
        }
    }

    #[test]
    fn ac2_cells_are_even_and_tile_the_area() {
        for o in sample_outputs() {
            let (a, name) = (o.area, &o.name);
            let ws: Vec<i32> = (0..COLS)
                .map(|c| o.cells_rect(cells(c, 0, 1, 1)).w)
                .collect();
            let hs: Vec<i32> = (0..ROWS)
                .map(|r| o.cells_rect(cells(0, r, 1, 1)).h)
                .collect();
            // Each cell starts where the previous one ends, and the last ends at the area's edge.
            for c in 0..COLS {
                let x = a.x + ws[..c as usize].iter().sum::<i32>();
                assert_eq!(o.cells_rect(cells(c, 0, 1, 1)).x, x, "{name} col {c}");
            }
            for r in 0..ROWS {
                let y = a.y + hs[..r as usize].iter().sum::<i32>();
                assert_eq!(o.cells_rect(cells(0, r, 1, 1)).y, y, "{name} row {r}");
            }
            assert_eq!(ws.iter().sum::<i32>(), a.w, "{name}");
            assert_eq!(hs.iter().sum::<i32>(), a.h, "{name}");
            for v in [ws, hs] {
                let (lo, hi) = (v.iter().min().unwrap(), v.iter().max().unwrap());
                assert!(hi - lo <= 1, "{name}: {v:?}");
            }
        }

        let uw = ultrawide();
        for col in 0..=COLS - 8 {
            let w = uw.cells_rect(cells(col, 0, 8, ROWS)).w;
            assert!(w == 1146 || w == 1147, "8 cols at {col}: {w}");
        }
        for col in 0..=COLS - 6 {
            assert_eq!(uw.cells_rect(cells(col, 0, 6, ROWS)).w, 860, "at {col}");
        }
        // The laptop's 21 px remainder is spread, not given to the last column.
        let lp = laptop();
        assert_eq!(lp.cells_rect(cells(0, 0, 1, 1)).w, 46);
        assert_eq!(lp.cells_rect(cells(COLS - 1, 0, 1, 1)).w, 47);
    }

    #[test]
    fn ac3_minimum_span_is_at_least_320_by_240_everywhere() {
        let [uw, uw_bars, k, lp, _] = sample_outputs();
        for (o, span) in [(uw, (3, 2)), (uw_bars, (3, 3)), (k, (2, 2)), (lp, (7, 5))] {
            assert_eq!(o.span_for((MIN_W, MIN_H)), span, "{}", o.name);
            for c in spots(span.0, span.1) {
                let r = o.cells_rect(c);
                assert!(r.w >= MIN_W && r.h >= MIN_H, "{} {c:?}: {r:?}", o.name);
            }
        }
    }

    /// The grid table in docs/design.md's Layout model.
    #[test]
    fn design_table_matches() {
        let r = |x, y, w, h| Rect { x, y, w, h };
        for ((w, h), zone, area, span, most) in [
            ((3440, 1440), (0, 1440), (3428, 1428), (3, 3), 8),
            ((3440, 1440), (44, 1440 - 96), (3428, 1332), (3, 3), 8),
            ((3840, 2160), (0, 2160), (3828, 2148), (3, 2), 8),
            ((1125, 703), (0, 703), (1113, 691), (7, 5), 3),
        ] {
            let full = r(0, 0, w, h);
            let a = crate::wallpaper::usable_area(full, r(0, zone.0, w, zone.1));
            let o = out("o", a.x, a.y, a.w, a.h, 0);
            assert_eq!((a.w, a.h), area);
            assert_eq!(o.span_for((MIN_W, MIN_H)), span, "{area:?}");
            assert_eq!(COLS / span.0, most);
        }
    }

    #[test]
    fn common_splits_are_exact() {
        let uw = ultrawide();
        // Halves, thirds and quarters of the ultrawide.
        assert_eq!(uw.cells_rect(cells(12, 0, 12, 12)).w, 1720);
        let thirds: Vec<i32> = (0..3)
            .map(|k| uw.cells_rect(cells(k * 8, 0, 8, 12)).w)
            .collect();
        assert_eq!(thirds, [1146, 1147, 1147]);
        assert_eq!(uw.cells_rect(cells(18, 0, 6, 12)).w, 860);

        // The 4K as 3x3, and a video in its top 2x2.
        let k = uhd();
        assert_eq!(
            k.cells_rect(cells(8, 4, 8, 4)),
            Rect {
                x: 1280,
                y: 720,
                w: 1280,
                h: 720
            }
        );
        assert_eq!(
            k.cells_rect(cells(0, 0, 16, 8)),
            Rect {
                x: 0,
                y: 0,
                w: 2560,
                h: 1440
            }
        );
    }

    #[test]
    fn tile_rect_leaves_gap_and_grab_strip() {
        let k = uhd();
        assert_eq!(
            k.tile_rect(cells(0, 0, 8, 4)),
            Rect {
                x: 6,
                y: 8,
                w: 1268,
                h: 708
            }
        );
    }

    #[test]
    fn span_rounds_to_nearest_cell_and_respects_minimum() {
        assert_eq!(uhd().span_for((900, 700)), (6, 4));
        assert_eq!(ultrawide().span_for((900, 700)), (6, 6));
        // 320x240 needs 7x5 laptop cells (46x58 at least).
        assert_eq!(laptop().span_for((10, 10)), (7, 5));
        assert_eq!(laptop().span_for((99_999, 99_999)), (COLS, ROWS));
        // An output too small for the minimum gets the whole grid, not a panic.
        assert_eq!(
            out("tiny", 0, 0, 200, 100, 0).span_for((10, 10)),
            (COLS, ROWS)
        );
        assert_eq!(out("tinier", 0, 0, 5, 5, 0).cells_rect(FULL).w, 5);
    }

    #[test]
    fn unknown_app_gets_half_width_full_height_near_pointer() {
        let mut l = layout(vec![ultrawide()]);
        let (id, step) = l.place("browser", (3000, 700)).unwrap();
        assert_eq!(step, Step::Nearest);
        assert_eq!(cells_of(&l, id), cells(12, 0, 12, 12));

        let (id2, _) = l.place("term", (3000, 700)).unwrap();
        assert_eq!(cells_of(&l, id2), cells(0, 0, 12, 12));
    }

    #[test]
    fn app_returns_to_its_remembered_spot() {
        let mut l = layout(vec![ultrawide()]);
        let (id, _) = l.place("t3", (10, 10)).unwrap();
        assert_eq!(cells_of(&l, id), cells(0, 0, 12, 12));
        l.remove(id);

        // The pointer is on the far side, but the old spot is free.
        let (id, step) = l.place("t3", (3400, 10)).unwrap();
        assert_eq!(step, Step::Remembered);
        assert_eq!(cells_of(&l, id), cells(0, 0, 12, 12));
    }

    #[test]
    fn taken_spot_falls_back_to_nearest_at_remembered_size() {
        let mut l = layout(vec![ultrawide()]);
        let (a, _) = l.place("term", (10, 10)).unwrap();
        l.memory.get_mut("term").unwrap().size = (860, 720);
        l.remove(a);
        l.place("other", (10, 10)).unwrap(); // takes cols 0..12

        let (id, step) = l.place("term", (10, 10)).unwrap();
        assert_eq!(step, Step::Nearest);
        // 860x720 = 6x6 cells, as close to the pointer as the free space allows.
        assert_eq!(cells_of(&l, id), cells(12, 0, 6, 6));
    }

    #[test]
    fn closing_leaves_cells_empty() {
        let mut l = layout(vec![ultrawide()]);
        let (a, _) = l.place("a", (10, 10)).unwrap();
        let (b, _) = l.place("b", (3400, 10)).unwrap();
        l.remove(a);
        assert_eq!(l.tiles().len(), 1);
        assert_eq!(cells_of(&l, b), cells(12, 0, 12, 12));
    }

    #[test]
    fn shrinks_tiles_wider_than_they_want() {
        let mut l = layout(vec![ultrawide()]);
        let (browser, _) = l.place("browser", (10, 10)).unwrap();
        let (term, _) = l.place("term", (3400, 10)).unwrap();
        // The browser now wants only 6 columns, but still has 12.
        l.memory.get_mut("browser").unwrap().size = (860, 1440);
        l.memory.insert(
            "editor".into(),
            Memory {
                output: "gone".into(),
                cells: cells(0, 0, 6, 12),
                size: (860, 1440),
                stamp: 0,
            },
        );

        let (editor, step) = l.place("editor", (10, 700)).unwrap();
        assert_eq!(step, Step::Shrunk);
        assert_eq!(cells_of(&l, browser), cells(6, 0, 6, 12));
        assert_eq!(cells_of(&l, editor), cells(0, 0, 6, 12));
        assert_eq!(cells_of(&l, term), cells(12, 0, 12, 12));
    }

    /// Places unknown `a` at the left and `b` at the right: two half-width tiles.
    fn two_halves(l: &mut Layout) -> (WinId, WinId) {
        let (a, _) = l.place("a", (10, 10)).unwrap();
        let (b, _) = l.place("b", (3400, 10)).unwrap();
        (a, b)
    }

    /// Makes `app` remember `size` without a usable spot.
    fn remember(l: &mut Layout, app: &str, size: (i32, i32)) {
        let m = Memory {
            output: "gone".into(),
            cells: cells(0, 0, 1, 1),
            size,
            stamp: 0,
        };
        l.memory.insert(app.into(), m);
    }

    /// No overlap, inside the grid, and left-to-right in `order`.
    fn assert_row(l: &Layout, order: &[WinId], min_cols: i32) {
        let cs: Vec<Cells> = order.iter().map(|&id| cells_of(l, id)).collect();
        for (i, c) in cs.iter().enumerate() {
            assert!(c.col >= 0 && c.col + c.cols <= COLS, "{c:?} off the grid");
            assert_eq!((c.row, c.rows), (0, ROWS), "{c:?} not full height");
            assert!(c.cols >= min_cols, "{c:?} below the minimum");
            for d in &cs[i + 1..] {
                assert!(!c.overlaps(d), "{c:?} overlaps {d:?}");
                assert!(c.col < d.col, "{c:?} not left of {d:?}");
            }
        }
    }

    #[test]
    fn third_window_shares_the_row_by_pointer() {
        // AC1-AC3: the gap opens on the pointer's side.
        for (x, order) in [(3400, [0, 1, 2]), (1720, [0, 2, 1]), (10, [2, 0, 1])] {
            let mut l = layout(vec![ultrawide()]);
            let (a, b) = two_halves(&mut l);
            let (c, step) = l.place("c", (x, 10)).unwrap();
            assert_eq!(step, Step::Shrunk);
            let ids = [a, b, c];
            for (slot, &k) in order.iter().enumerate() {
                assert_eq!(
                    cells_of(&l, ids[k]),
                    cells(slot as i32 * 8, 0, 8, ROWS),
                    "x={x}"
                );
            }
        }
    }

    #[test]
    fn fourth_window_makes_quarters() {
        let mut l = layout(vec![ultrawide()]);
        let (a, b) = two_halves(&mut l);
        let (c, _) = l.place("c", (3400, 10)).unwrap();
        let (d, step) = l.place("d", (3400, 10)).unwrap();
        assert_eq!(step, Step::Shrunk);
        for (slot, id) in [a, b, c, d].into_iter().enumerate() {
            assert_eq!(cells_of(&l, id), cells(slot as i32 * 6, 0, 6, ROWS));
        }
    }

    #[test]
    fn ac4_equal_share_then_tabs_only_when_every_tile_is_at_minimum() {
        let mut l = layout(vec![ultrawide()]);
        let mut order = Vec::new();
        // Sorted column widths after each window; 24 isn't divisible by 5 or 7.
        let shares: [&[i32]; 8] = [
            &[12],
            &[12, 12],
            &[8, 8, 8],
            &[6, 6, 6, 6],
            &[4, 5, 5, 5, 5],
            &[4, 4, 4, 4, 4, 4],
            &[3, 3, 3, 3, 4, 4, 4],
            &[3, 3, 3, 3, 3, 3, 3, 3],
        ];
        for (n, share) in (1..=8).zip(shares) {
            let (id, step) = l.place(&format!("app{n}"), (3400, 10)).unwrap();
            if n >= 3 {
                assert_eq!(step, Step::Shrunk, "window {n}");
            }
            // The 1st lands on the right, the 2nd left of it, the rest at the right end.
            match n {
                2 => order.insert(0, id),
                _ => order.push(id),
            }
            assert_row(&l, &order, 3);
            let mut widths: Vec<i32> = order.iter().map(|&id| cells_of(&l, id).cols).collect();
            widths.sort_unstable();
            assert_eq!(widths, share, "window {n}");
        }
        let (_, step) = l.place("app9", (3400, 10)).unwrap();
        assert!(matches!(step, Step::Tabbed(_)));
    }

    #[test]
    fn laptop_minimum_stops_at_thirds_and_failure_moves_nothing() {
        // AC6 and AC11: 7 laptop columns are the minimum.
        let mut l = layout(vec![laptop()]);
        let (a, _) = l.place("a", (10, 10)).unwrap();
        let (b, _) = l.place("b", (1100, 10)).unwrap();
        let (c, step) = l.place("c", (1100, 10)).unwrap();
        assert_eq!(step, Step::Shrunk);
        for (slot, id) in [a, b, c].into_iter().enumerate() {
            assert_eq!(cells_of(&l, id), cells(slot as i32 * 8, 0, 8, ROWS));
        }

        let before: Vec<Cells> = l.tiles().iter().map(|t| t.cells).collect();
        let (_, step) = l.place("d", (1100, 10)).unwrap();
        assert!(matches!(step, Step::Tabbed(_)));
        let after: Vec<Cells> = l.tiles()[..3].iter().map(|t| t.cells).collect();
        assert_eq!(after, before);
    }

    #[test]
    fn shrinks_before_overflowing_to_next_output_by_priority() {
        let mut l = layout(vec![
            out("4k", 0, -2160, 3840, 2160, 1),
            out("uw", 0, 0, 3440, 1440, 0),
        ]);
        two_halves(&mut l);
        let (c, step) = l.place("c", (100, 100)).unwrap();
        assert_eq!(step, Step::Shrunk);
        assert_eq!(l.tile(c).unwrap().output, "uw");

        for n in 4..=8 {
            let (_, step) = l.place(&format!("app{n}"), (100, 100)).unwrap();
            assert_eq!(step, Step::Shrunk);
        }
        let (g, step) = l.place("app9", (100, 100)).unwrap();
        assert_eq!(step, Step::OtherOutput);
        let t = l.tile(g).unwrap();
        assert_eq!(t.output, "4k");
        // Half the ultrawide (1720x1440 px) is 11x8 4K cells, as near the centre as they go.
        assert_eq!(t.cells, cells(6, 2, 11, 8));
    }

    #[test]
    fn opens_smaller_in_a_gap_before_shrinking() {
        let mut l = layout(vec![ultrawide()]);
        let (a, _) = l.place("a", (10, 10)).unwrap();
        remember(&mut l, "b", (1144, 1440));
        let (b, _) = l.place("b", (3400, 10)).unwrap();
        assert_eq!(cells_of(&l, a), cells(0, 0, 12, 12));
        assert_eq!(cells_of(&l, b), cells(16, 0, 8, 12));

        let (c, step) = l.place("c", (3400, 10)).unwrap();
        assert_eq!(step, Step::Smaller);
        assert_eq!(cells_of(&l, c), cells(12, 0, 4, 12));
        assert_eq!(cells_of(&l, a), cells(0, 0, 12, 12));
        assert_eq!(cells_of(&l, b), cells(16, 0, 8, 12));
        // AC10
        assert_eq!(l.memory["a"].size, (1720, 1440));
        assert_eq!(l.memory["b"].size, (1144, 1440));
        assert_eq!(l.memory["c"].size, (1720, 1440));
    }

    #[test]
    fn gap_below_minimum_is_not_used_smaller() {
        let mut l = layout(vec![ultrawide()]);
        let (a, _) = l.place("a", (10, 10)).unwrap();
        remember(&mut l, "b", (1430, 1440));
        let (b, _) = l.place("b", (3400, 10)).unwrap();
        // 2 free columns (286 px) are below the 3-column minimum.
        assert_eq!(cells_of(&l, b), cells(14, 0, 10, 12));

        let (c, step) = l.place("c", (3400, 10)).unwrap();
        assert_eq!(step, Step::Shrunk);
        assert_eq!(cells_of(&l, c).cols, 8);
        assert_row(&l, &[a, b, c], 3);
    }

    #[test]
    fn shrinking_never_changes_memory() {
        let mut l = layout(vec![ultrawide()]);
        two_halves(&mut l);
        l.place("c", (3400, 10)).unwrap();
        for app in ["a", "b", "c"] {
            assert_eq!(l.memory[app].size, (1720, 1440), "{app}");
        }
    }

    #[test]
    fn pointer_picks_the_output() {
        let mut l = layout(vec![
            out("uw", 0, 0, 3440, 1440, 0),
            out("4k", 0, -2160, 3840, 2160, 1),
        ]);
        let (id, _) = l.place("a", (100, -100)).unwrap();
        assert_eq!(l.tile(id).unwrap().output, "4k");
        assert_eq!(
            l.rect(id).unwrap(),
            Rect {
                x: 6,
                y: -2152,
                w: 1908,
                h: 2148
            }
        );
    }

    #[test]
    fn ac8_fullscreen_covers_the_output_and_ends() {
        let mut l = layout(vec![ultrawide()]);
        // The grid is the output minus a 48 px bar; fullscreen ignores that.
        let mut o = ultrawide();
        o.area.h -= 48;
        l.set_outputs(vec![o]);
        let (a, b) = two_halves(&mut l);
        assert!(l.fullscreen(a));
        assert!(!l.fullscreen(a), "already");
        assert_eq!(l.rect(a), Some(ultrawide().area));
        assert!(l.visible(a) && !l.visible(b));
        assert!(l.fullscreen_on("uw"));
        assert!(!l.fullscreen(b), "hidden");
        // navi's zones for it are gone with its strip.
        assert_eq!(l.hit((100, 2)), None);

        assert!(l.unfullscreen(a));
        assert!(!l.unfullscreen(a));
        assert!(!l.is_maximized(a), "back to its tile");
        assert_eq!(
            l.rect(a),
            Some(l.output("uw").unwrap().tile_rect(cells_of(&l, a)))
        );
        assert!(l.visible(b));

        assert!(l.fullscreen(a) && l.minimize(a));
        assert!(!l.is_fullscreen(a) && l.visible(b));
        assert!(l.activate(a));

        assert!(l.fullscreen(a) && l.activate(b));
        assert!(!l.is_fullscreen(a) && l.visible(a) && l.visible(b));

        assert!(l.fullscreen(a));
        let (c, _) = l.place("c", (3400, 10)).unwrap();
        assert!(!l.is_fullscreen(a) && l.visible(a) && l.visible(c));
        assert!(!l.fullscreen_on("uw"));
    }

    #[test]
    fn fullscreen_escape_ends_one_output_only() {
        let (mut l, [a, b, c]) = rig_with_tiles();
        let tile = l.rect(a);
        assert!(l.fullscreen(a) && l.fullscreen(c));
        assert!(!l.visible(b));

        assert_eq!(l.unfullscreen_output("4k"), vec![a]);
        assert!(!l.fullscreen_on("4k") && !l.is_fullscreen(a));
        assert_eq!(l.rect(a), tile, "back to its tile");
        assert!(l.visible(a) && l.visible(b), "hidden windows shown again");
        assert!(
            l.is_fullscreen(c) && l.fullscreen_on("uw"),
            "other output untouched"
        );
        assert_eq!(
            l.unfullscreen_output("4k"),
            Vec::<WinId>::new(),
            "no change"
        );

        // A maximize from before fullscreen survives it.
        assert!(l.maximize(a) && l.fullscreen(a));
        let maximized = l.output("4k").unwrap().window_rect(FULL, STRIP);
        assert_eq!(l.unfullscreen_output("4k"), vec![a]);
        assert!(l.is_maximized(a) && !l.visible(b));
        assert_eq!(l.rect(a), Some(maximized));
        assert!(l.is_fullscreen(c));
    }

    #[test]
    fn ac8_activate_uncovers_and_restores() {
        let mut l = layout(vec![ultrawide()]);
        let (a, b) = two_halves(&mut l);
        assert!(l.maximize(a));
        assert!(!l.visible(b));
        assert!(l.activate(b));
        assert!(!l.is_maximized(a) && l.visible(a) && l.visible(b));
        assert!(!l.activate(b), "already shown");

        // A minimized tab becomes the group's visible active tab.
        let t = l.next_id;
        l.next_id += 1;
        l.tiles.push(Tile {
            id: t,
            app: "t".into(),
            output: "uw".into(),
            cells: cells_of(&l, b),
            ..Tile::default()
        });
        l.touch(t);
        assert!(l.visible(t) && !l.visible(b));
        assert!(l.minimize(t));
        assert!(l.visible(b));
        assert!(l.activate(t));
        assert!(l.visible(t) && !l.tile(t).unwrap().minimized && !l.visible(b));

        // Its own group stays maximized.
        assert!(l.maximize(t));
        assert!(l.activate(b));
        assert!(l.is_maximized(b) && l.visible(b));
    }

    #[test]
    fn activate_reports_the_fullscreen_it_ends() {
        let (mut l, [a, b, c]) = rig_with_tiles();
        // Another window on the fullscreen output ends it, and is reported.
        assert!(l.fullscreen(a));
        assert_eq!(l.fullscreen_ended_by(b), vec![a]);
        assert!(l.activate(b));
        assert!(!l.is_fullscreen(a) && l.visible(b));
        // The fullscreen window itself, or a window on another output: none, still fullscreen.
        assert!(l.fullscreen(a));
        assert_eq!(l.fullscreen_ended_by(a), Vec::<WinId>::new());
        l.activate(a);
        assert_eq!(l.fullscreen_ended_by(c), Vec::<WinId>::new());
        l.activate(c);
        assert!(l.is_fullscreen(a));
        assert_eq!(
            l.fullscreen_ended_by(99),
            Vec::<WinId>::new(),
            "no such tile"
        );
    }

    /// Yoshi's rig: ultrawide (main), 4K above it, laptop left.
    fn rig() -> Vec<Output> {
        vec![
            out("uw", 1800, 2160, 3440, 1440, 0),
            out("4k", 1640, 0, 3840, 2160, 1),
            out("laptop", 0, 2306, 1800, 1125, 2),
        ]
    }

    /// Two tiles on the 4K and one on the ultrawide.
    fn rig_with_tiles() -> (Layout, [WinId; 3]) {
        let mut l = layout(rig());
        let (a, _) = l.place("a", (2000, 100)).unwrap();
        let (b, _) = l.place("b", (5000, 100)).unwrap();
        let (c, _) = l.place("c", (2000, 2200)).unwrap();
        assert_eq!(l.tile(a).unwrap().output, "4k");
        assert_eq!(l.tile(b).unwrap().output, "4k");
        assert_eq!(l.tile(c).unwrap().output, "uw");
        (l, [a, b, c])
    }

    fn spot(l: &Layout, id: WinId) -> (String, Cells) {
        let t = l.tile(id).unwrap();
        (t.output.clone(), t.cells)
    }

    #[test]
    fn ac8_unplug_evacuates_and_replug_returns() {
        let (mut l, [a, b, c]) = rig_with_tiles();
        let (was_a, was_b) = (spot(&l, a), spot(&l, b));
        let memory = l.memory_text();

        l.remove_output("4k");
        for id in [a, b] {
            let (o, _) = spot(&l, id);
            assert!(o == "uw" || o == "laptop", "{o}");
        }
        assert_eq!(l.tile(a).unwrap().home, Some(was_a.clone()));
        assert_eq!(l.tile(b).unwrap().home, Some(was_b.clone()));
        assert_eq!(l.memory_text(), memory);
        assert!(l.tile(c).unwrap().home.is_none());
        for id in [a, b, c] {
            assert!(l.rect(id).is_some(), "{id} shown");
        }
        let after = spot(&l, c);
        assert_eq!(after.0, "uw");

        l.set_outputs(rig());
        assert_eq!((spot(&l, a), spot(&l, b)), (was_a, was_b));
        assert!(l.tiles().iter().all(|t| t.home.is_none()));
        assert_eq!(l.memory_text(), memory);
        // A tile that was never on the 4K doesn't move when it returns.
        assert_eq!(spot(&l, c), after);
    }

    #[test]
    fn ac8_tiles_never_on_the_output_stay_put() {
        let mut l = layout(rig());
        let (c, _) = l.place("c", (2000, 2200)).unwrap();
        let (d, _) = l.place("d", (100, 2400)).unwrap();
        let before = (spot(&l, c), spot(&l, d));
        l.remove_output("4k");
        l.set_outputs(rig());
        assert_eq!((spot(&l, c), spot(&l, d)), before);
    }

    #[test]
    fn ac8_moved_since_stays_where_the_user_put_it() {
        let (mut l, [a, b, _]) = rig_with_tiles();
        let was_b = spot(&l, b);
        l.remove_output("4k");
        // Drag a's strip to the laptop.
        let r = l.frame(a).unwrap().0;
        let from = (r.x + r.w / 2, r.y + 2);
        let mut d = Drag::press(&l, from).expect("a's strip");
        d.motion(&mut l, (200, 2600), 10);
        let done = d.release(&mut l, (300, 2700), 20);
        assert!(matches!(done, Some(Done::Layout("moved", _))));
        let moved = spot(&l, a);
        assert_eq!(moved.0, "laptop");
        assert!(l.tile(a).unwrap().home.is_none());

        l.set_outputs(rig());
        assert_eq!(spot(&l, a), moved);
        assert_eq!(spot(&l, b), was_b);
    }

    #[test]
    fn ac8_taken_home_cells_keep_the_tile_put() {
        let (mut l, [a, b, _]) = rig_with_tiles();
        let was_a = spot(&l, a);
        l.remove_output("4k");
        let evacuated = spot(&l, b);
        // Something took b's cells on the 4K meanwhile.
        let (_, home) = l.tile(b).unwrap().home.clone().unwrap();
        l.tiles.push(Tile {
            id: 99,
            app: "z".into(),
            output: "4k".into(),
            cells: home,
            ..Tile::default()
        });
        l.set_outputs(rig());
        assert_eq!(spot(&l, a), was_a);
        assert_eq!(spot(&l, b), evacuated);
        assert!(l.tile(b).unwrap().home.is_none());
    }

    #[test]
    fn unplug_keeps_tab_groups_together() {
        let (mut l, [a, b, _]) = rig_with_tiles();
        // Tab b onto a.
        let cells = l.tile(a).unwrap().cells;
        l.tile_mut(b).unwrap().cells = cells;
        l.remove_output("4k");
        assert_eq!(spot(&l, a), spot(&l, b));
        l.set_outputs(rig());
        assert_eq!(spot(&l, a), ("4k".into(), cells));
        assert_eq!(spot(&l, b), ("4k".into(), cells));
    }

    #[test]
    fn no_outputs_places_nothing() {
        assert_eq!(Layout::default().place("a", (0, 0)), None);
    }

    // Client sizes (job 017).

    const FIXED: (i32, i32) = (1220, 600);
    const LATE: (i32, i32) = (995, 710);

    fn fixed() -> Hints {
        Hints::new(FIXED, FIXED)
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
    }

    fn inside(a: Rect, b: Rect) -> bool {
        a.x >= b.x && a.y >= b.y && a.x + a.w <= b.x + b.w && a.y + a.h <= b.y + b.h
    }

    /// The ultrawide's grid under its panels, as in the design table.
    fn uw_panels() -> Output {
        out("uwp", 44, 0, 3428, 1332, 0)
    }

    #[test]
    fn ac1_hints_are_sanitised_and_repeats_change_nothing() {
        assert_eq!(Hints::new((0, 0), (0, 0)), Hints::default());
        let h = Hints::new((100_000, 5), (0, 70_000));
        assert_eq!((h.min, h.max), ((MAX_PX, 5), (0, MAX_PX)));
        let h = Hints::new((800, 600), (400, 700));
        assert_eq!(
            (h.min, h.max),
            ((800, 600), (0, 700)),
            "max below min dropped"
        );
        let h = Hints::new((-5, i32::MIN), (i32::MIN, -1));
        assert_eq!(h, Hints::default());

        let mut l = layout(vec![ultrawide()]);
        let (id, _) = l.place("bmm", (10, 10)).unwrap();
        assert_ne!(l.set_hints(id, (70_000, 600), (0, 0)), Fit::Unchanged);
        let before = (l.tiles.clone(), l.memory.clone(), l.clock, l.memory_text());
        for _ in 0..1000 {
            assert_eq!(l.set_hints(id, (70_000, 600), (0, 0)), Fit::Unchanged);
            assert_eq!(l.set_hints(id, (MAX_PX, 600), (-3, 0)), Fit::Unchanged);
        }
        assert_eq!(
            (l.tiles.clone(), l.memory.clone(), l.clock, l.memory_text()),
            before
        );
        assert_eq!(
            l.set_hints(99, FIXED, FIXED),
            Fit::Unchanged,
            "no such tile"
        );
    }

    /// `min_span`'s worst-case rule, checked at every grid position.
    fn assert_boundary(o: &Output, min: (i32, i32), bar: bool, span: (i32, i32)) {
        let top = if bar { STRIP + BAR } else { STRIP };
        for c in spots(span.0, span.1) {
            let r = o.window_rect(c, top);
            assert!(r.w >= min.0 && r.h >= min.1, "{} {c:?}: {r:?}", o.name);
        }
        let short = |cols, rows| {
            spots(cols, rows).any(|c| {
                let r = o.window_rect(c, top);
                r.w < min.0 || r.h < min.1
            })
        };
        assert!(short(span.0 - 1, span.1), "{} one column fewer", o.name);
        assert!(short(span.0, span.1 - 1), "{} one row fewer", o.name);
    }

    #[test]
    fn ac2_effective_minimum_span_is_the_worst_case() {
        let k_panels = out("4kp", 0, 0, 3828, 2148, 0);
        let cases = [
            (uw_panels(), FIXED, true, (9, 6)),
            (uw_panels(), LATE, false, (8, 7)),
            (uhd(), FIXED, true, (8, 4)),
            (uhd(), LATE, false, (7, 5)),
            (k_panels.clone(), FIXED, true, (8, 4)),
            (k_panels, LATE, false, (7, 5)),
        ];
        for (o, min, bar, span) in cases {
            assert_eq!(o.min_span(min, bar), Some(span), "{} {min:?}", o.name);
            assert_boundary(&o, min, bar, span);
        }
        let lp = out("lp", 0, 0, 1113, 691, 0);
        assert_eq!(
            lp.min_span(FIXED, true),
            None,
            "1220 is wider than the grid"
        );
        assert_eq!(
            lp.min_span(LATE, false),
            None,
            "710 is taller than the grid"
        );
        assert_eq!(lp.min_span((1220, 10), false), None);
        assert_eq!(lp.min_span((10, 710), false), None);
        assert_eq!(
            lp.min_span((1101, 679), false),
            Some((COLS, ROWS)),
            "the whole grid"
        );
        for o in sample_outputs() {
            let navi = Some(o.span_for((MIN_W, MIN_H)));
            for (min, bar) in [
                ((0, 0), false),
                ((10, 10), true),
                ((MIN_W - GAP, MIN_H - GAP - BAR), true),
                ((MIN_W - GAP, MIN_H - GAP), false),
            ] {
                assert_eq!(o.min_span(min, bar), navi, "{} {min:?}", o.name);
            }
        }
    }

    #[test]
    fn ac3_fixed_size_gets_its_size_on_an_empty_ultrawide() {
        let mut l = layout(vec![ultrawide()]);
        let (id, step) = l.place_with("upd", (1720, 700), fixed(), true).unwrap();
        assert_eq!(step, Step::Nearest);
        let k = cells_of(&l, id);
        assert_eq!((k.cols, k.rows), (9, 6));
        let o = ultrawide();
        let w = o.window_rect(k, STRIP + BAR);
        let r = l.rect(id).unwrap();
        assert_eq!(
            r,
            Rect {
                w: 1220,
                h: 600,
                ..w
            }
        );
        assert_eq!(l.clip(id), Some(r));
        assert_eq!(l.bar(id).unwrap().rect.w, 1220);
        let focus = crate::theme::rgba(crate::theme::ACCENT, 1.0);
        let frames = overlay(&l, None, (0, 0), 0, Some(id));
        assert!(
            frames
                .iter()
                .any(|&(f, k)| k == focus && f.w == 1220 + 4 && f.h == 2)
        );
        assert!(
            !frames.iter().any(|&(f, _)| f.x >= r.x + r.w + 2),
            "nothing in the leftover"
        );
    }

    #[test]
    fn ac4_neighbours_shrink_to_their_own_minimum_for_a_fixed_window() {
        let mut l = layout(vec![ultrawide()]);
        let (a, b) = two_halves(&mut l);
        let memory = l.memory_text();
        let (id, step) = l.place_with("upd", (3400, 10), fixed(), true).unwrap();
        assert_eq!(step, Step::Shrunk);
        assert!(cells_of(&l, id).cols >= 9);
        assert_no_overlap(&l);
        for n in [a, b] {
            assert!(
                cells_of(&l, n).cols >= l.floor(n).0,
                "{:?}",
                cells_of(&l, n)
            );
        }
        assert!(l.rect(id).unwrap().w == 1220 && l.rect(id).unwrap().h == 600);
        for (line, app) in memory.lines().skip(1).zip(["b", "a"]) {
            assert!(l.memory_text().contains(line), "{app}'s memory changed");
        }

        // Neighbours whose client minimum is 11 columns can't make room: the 4K, else a float,
        // since 1410 px high they can't take a tab's bar either.
        for with_4k in [false, true] {
            let mut outputs = vec![ultrawide()];
            if with_4k {
                outputs.push(out("4k", 0, -2160, 3840, 2160, 1));
            }
            let mut l = layout(outputs);
            let (a, b) = two_halves(&mut l);
            for id in [a, b] {
                assert_eq!(l.set_hints(id, (1500, 1410), (0, 0)), Fit::Fits);
                assert_eq!(l.floor(id), (11, 12));
            }
            let (id, step) = l.place_with("upd", (3400, 10), fixed(), true).unwrap();
            let t = l.tile(id).unwrap();
            match with_4k {
                true => {
                    assert_eq!(step, Step::OtherOutput);
                    assert_eq!(
                        (t.output.as_str(), t.cells.cols, t.cells.rows),
                        ("4k", 8, 4)
                    );
                }
                false => assert!(step == Step::Floating && t.floating),
            }
            assert_eq!(
                (cells_of(&l, a), cells_of(&l, b)),
                (cells(0, 0, 12, 12), cells(12, 0, 12, 12))
            );
        }
    }

    /// The laptop alone with a fixed 1220 x 600 float and a tile beside it.
    fn laptop_float() -> (Layout, WinId, WinId) {
        let mut l = layout(vec![laptop()]);
        let (f, step) = l.place_with("upd", (500, 300), fixed(), true).unwrap();
        assert_eq!(step, Step::Floating);
        assert!(l.is_free("laptop", FULL), "the float holds no cells");
        let (t, step) = l.place("term", (500, 300)).unwrap();
        assert_eq!(step, Step::Nearest);
        (l, f, t)
    }

    #[test]
    fn ac5_too_big_for_any_tile_floats() {
        let (mut l, f, t) = laptop_float();
        let float = l.tile(f).unwrap().clone();
        assert!(float.floating && float.cells == Cells::default());
        assert!(l.is_free("laptop", float.cells));
        let a = laptop().area;
        let (outer, clip) = (l.frame(f).unwrap().0, l.clip(f).unwrap());
        assert!(inside(outer, a) && inside(clip, a));
        assert!(
            (outer.x - a.x - (a.x + a.w - outer.x - outer.w)).abs() <= 1,
            "{outer:?}"
        );
        assert!(
            (outer.y - a.y - (a.y + a.h - outer.y - outer.h)).abs() <= 1,
            "{outer:?}"
        );
        assert_eq!(
            (clip.w, clip.h),
            (a.w, 600),
            "1220 is wider than the laptop"
        );
        assert!(l.too_small(f));
        assert_eq!(outer.h, 600 + BAR);

        // Over the float, only its buttons; with it minimized, the tile's zones show through.
        let mut through = 0;
        for x in (outer.x..outer.x + outer.w).step_by(7) {
            for y in (outer.y..outer.y + outer.h).step_by(5) {
                let hit = l.hit((x, y));
                assert!(
                    hit.is_none_or(|h| matches!(h, Hit::Button(id, _) | Hit::Strip(id) if id == f)),
                    "{hit:?}"
                );
            }
        }
        let buttons = l.bar(f).unwrap().buttons;
        assert_eq!(buttons.len(), 3);
        let close = buttons[2].1;
        assert_eq!(
            l.hit((close.x + 4, close.y + 4)),
            Some(Hit::Button(f, Button::Close))
        );

        assert!(!l.maximize(f), "maximize is refused");
        assert!(l.fullscreen(f));
        assert_eq!(l.rect(f), Some(laptop().full));
        assert!(l.unfullscreen(f));
        assert_eq!(l.clip(f), Some(clip));
        assert!(l.tile(f).unwrap().floating);
        assert!(l.minimize(f) && !l.visible(f));
        for x in (outer.x..outer.x + outer.w).step_by(7) {
            for y in (outer.y..outer.y + outer.h).step_by(5) {
                through += usize::from(l.hit((x, y)).is_some_and(|h| h.id() == t));
            }
        }
        assert!(through > 0, "the tile's zones are under the float");
        assert!(l.activate(f) && l.visible(f));

        // Nothing tabs into it; moves never target it.
        for n in 0..6 {
            let (_, step) = l.place(&format!("app{n}"), (500, 300)).unwrap();
            assert_ne!(step, Step::Tabbed(f));
        }
        let mut l = laptop_float().0;
        let centre = l.clip(f).unwrap().center();
        let strip = l.zone(Hit::Strip(t)).unwrap().center();
        let mut d = Drag::press(&l, strip).expect("the tile's strip, beside the float");
        d.motion(&mut l, centre, 0);
        d.motion(&mut l, centre, 1000);
        let done = d.release(&mut l, centre, 1000);
        assert!(
            !matches!(done, Some(Done::Layout("tabbed" | "swapped", _))),
            "{done:?}"
        );
        assert_eq!(l.tile(f).unwrap(), &float);

        // With the 4K connected it tiles there; unplugging re-runs the fit from the main output.
        let mut l = layout(vec![laptop(), out("4k", 2000, 0, 3840, 2160, 1)]);
        let (id, step) = l.place_with("upd", (500, 300), fixed(), true).unwrap();
        assert_eq!(step, Step::OtherOutput);
        let k = l.tile(id).unwrap();
        assert_eq!(
            (k.output.as_str(), k.cells.cols, k.cells.rows),
            ("4k", 8, 4)
        );
        let (mut l, f, _) = laptop_float();
        l.set_outputs(vec![laptop(), out("uw", 2000, 0, 3440, 1440, 1)]);
        l.remove_output("laptop");
        let k = l.tile(f).unwrap();
        assert!(!k.floating && k.output == "uw" && k.home.is_none(), "{k:?}");
        let (mut l, f, _) = laptop_float();
        l.set_outputs(vec![laptop(), out("lp2", 2000, 0, 1125, 703, 1)]);
        l.remove_output("laptop");
        let k = l.tile(f).unwrap();
        assert!(k.floating && k.output == "lp2", "{k:?}");
        assert!(inside(l.clip(f).unwrap(), l.output("lp2").unwrap().area));
    }

    #[test]
    fn a_float_always_has_navis_buttons_and_minimizing_uncovers_the_tiles() {
        // A client that draws its own title bar (no xdg-decoration) still gets navi's bar.
        let mut l = layout(vec![laptop()]);
        let (t, _) = l.place("term", (500, 300)).unwrap();
        let (f, step) = l
            .place_with("bmm", (500, 300), Hints::new(LATE, (0, 0)), false)
            .unwrap();
        assert_eq!(step, Step::Floating);
        assert!(l.has_bar(f));
        let buttons = l.bar(f).unwrap().buttons;
        let kinds: Vec<Button> = buttons.iter().map(|b| b.0).collect();
        assert_eq!(kinds, [Button::Minimize, Button::Maximize, Button::Close]);
        let minimize = buttons[0].1;
        let at = (minimize.x + 4, minimize.y + 4);
        assert_eq!(l.hit(at), Some(Hit::Button(f, Button::Minimize)));
        assert!(inside(l.frame(f).unwrap().0, laptop().full));
        let d = Drag::press(&l, at).unwrap();
        assert_eq!(
            d.release(&mut l, at, 10),
            Some(Done::Click(Action::Minimize, f))
        );
        assert!(!l.visible(f));
        let strip = l.zone(Hit::Strip(t)).unwrap().center();
        assert_eq!(
            l.hit(strip),
            Some(Hit::Strip(t)),
            "the tile is reachable again"
        );
        assert!(l.activate(f) && l.visible(f), "the dock restores it");
        assert_eq!(
            l.set_decorated(f, true),
            Fit::Fits,
            "a float keeps its place"
        );
    }

    #[test]
    fn a_float_keeps_its_minimum_height_on_the_monitor_and_a_margin_when_it_fits() {
        // A band of 44 px over a 1125 x 703 laptop: 590 px don't fit a tile below a bar.
        let mut o = laptop();
        o.area = Rect {
            x: 0,
            y: 44,
            w: 1125,
            h: 600,
        };
        let mut l = layout(vec![o.clone()]);
        let (f, step) = l
            .place_with("tall", (500, 300), Hints::new((300, 590), (0, 0)), false)
            .unwrap();
        assert_eq!(step, Step::Floating);
        let (outer, clip) = (l.frame(f).unwrap().0, l.clip(f).unwrap());
        assert_eq!(clip.h, 590, "not clamped to the area");
        assert_eq!(outer.y, o.area.y + FLOAT_MARGIN, "its bar below the band");
        assert!(inside(outer, o.full) && !l.too_small(f));

        // Floating only because the neighbours can't give way: it fits, FLOAT_MARGIN inside.
        let (mut l, ids) = quarters();
        for id in [ids[0], ids[1], ids[3]] {
            l.set_hints(id, (840, 0), (0, 0));
        }
        assert_eq!(l.set_hints(ids[2], LATE, (0, 0)), Fit::Floated);
        let a = ultrawide().area;
        let inset = Rect {
            x: a.x + FLOAT_MARGIN,
            y: a.y + FLOAT_MARGIN,
            w: a.w - 2 * FLOAT_MARGIN,
            h: a.h - 2 * FLOAT_MARGIN,
        };
        assert!(inside(l.frame(ids[2]).unwrap().0, inset));

        let mut l = layout(vec![laptop()]);
        let (f, _) = l
            .place_with("huge", (10, 10), Hints::new((2000, 1500), (0, 0)), false)
            .unwrap();
        assert!(l.too_small(f) && inside(l.frame(f).unwrap().0, laptop().full));
        // Too big for the monitor: cut, its bar still below the band.
        let mut l = layout(vec![o.clone()]);
        let huge = Hints::new((2000, 1500), (0, 0));
        let (f, _) = l.place_with("huge", (10, 300), huge, false).unwrap();
        let outer = l.frame(f).unwrap().0;
        assert!(
            l.too_small(f) && outer.y == o.area.y && inside(outer, o.full),
            "{outer:?}"
        );
    }

    #[test]
    fn a_late_bar_refits_and_a_re_place_ends_fullscreen() {
        // 1410 px high holds 12 rows without a bar, not below one.
        let mut l = layout(vec![ultrawide()]);
        let (a, _) = l.place("a", (10, 10)).unwrap();
        assert_eq!(l.set_hints(a, (0, 1410), (0, 0)), Fit::Fits);
        assert_eq!(l.set_decorated(a, false), Fit::Unchanged);
        assert_eq!(l.set_decorated(a, true), Fit::Floated);

        let (mut l, [_, _, late, _]) = quarters();
        assert!(l.fullscreen(late));
        assert!(matches!(l.set_hints(late, LATE, (0, 0)), Fit::Replaced(..)));
        assert!(!l.is_fullscreen(late));
    }

    /// Four quarters on the ultrawide; the third is "late".
    fn quarters() -> (Layout, [WinId; 4]) {
        let mut l = layout(vec![ultrawide()]);
        let (a, b) = two_halves(&mut l);
        let (late, _) = l.place("late", (3400, 10)).unwrap();
        let (d, _) = l.place("d", (3400, 10)).unwrap();
        for id in [a, b, late, d] {
            assert_eq!(cells_of(&l, id).cols, 6);
        }
        (l, [a, b, late, d])
    }

    fn assert_no_overlap(l: &Layout) {
        let tiles: Vec<&Tile> = l.tiles.iter().filter(|t| !t.floating).collect();
        for (i, t) in tiles.iter().enumerate() {
            for u in &tiles[i + 1..] {
                let same = t.output == u.output && t.cells == u.cells;
                assert!(
                    same || t.output != u.output || !t.cells.overlaps(&u.cells),
                    "{t:?} {u:?}"
                );
            }
        }
    }

    #[test]
    fn ac6_a_late_minimum_re_places_or_floats() {
        // Half the ultrawide already holds 995 x 710.
        let mut l = layout(vec![ultrawide()]);
        let (a, _) = two_halves(&mut l);
        let before = l.tiles.clone();
        assert_eq!(l.set_hints(a, LATE, (0, 0)), Fit::Fits);
        assert_eq!(cells_of(&l, a), before[0].cells);

        let (mut l, [_, _, late, _]) = quarters();
        let Fit::Replaced(output, k) = l.set_hints(late, LATE, (0, 0)) else {
            panic!("not re-placed");
        };
        assert_eq!((output.as_str(), cells_of(&l, late)), ("uw", k));
        assert!(k.cols >= 8 && k.rows >= 7, "{k:?}");
        assert_no_overlap(&l);
        let r = ultrawide().cells_rect(k);
        assert_eq!(
            (l.memory["late"].cells, l.memory["late"].size),
            (k, (r.w, r.h))
        );
        assert_eq!(l.set_hints(late, LATE, (0, 0)), Fit::Unchanged);

        // Neighbours at their client minimum, no tile big enough to tab into, no other output.
        let (mut l, ids) = quarters();
        for &id in &ids {
            if id != ids[2] {
                assert_eq!(l.set_hints(id, (840, 0), (0, 0)), Fit::Fits);
            }
        }
        assert_eq!(l.set_hints(ids[2], LATE, (0, 0)), Fit::Floated);
        assert!(l.tile(ids[2]).unwrap().floating);
        let mut l = layout(vec![laptop()]);
        let (id, _) = l.place("late", (10, 10)).unwrap();
        assert_eq!(l.set_hints(id, LATE, (0, 0)), Fit::Floated);
    }

    #[test]
    fn ac7_a_relaxed_minimum_tiles_a_float_and_frees_a_tiles_floor() {
        let (mut l, f, _) = laptop_float();
        let Fit::Tiled(output, k) = l.set_hints(f, (400, 300), (0, 0)) else {
            panic!("not tiled");
        };
        let t = l.tile(f).unwrap();
        assert!(!t.floating && t.output == output && t.cells == k);
        assert_no_overlap(&l);

        let mut l = layout(vec![ultrawide()]);
        let (id, _) = l
            .place_with("late", (10, 10), Hints::new(LATE, (0, 0)), false)
            .unwrap();
        let was = cells_of(&l, id);
        assert_eq!(l.floor(id), (8, 7));
        assert_eq!(l.set_hints(id, (0, 0), (0, 0)), Fit::Fits);
        assert_eq!(cells_of(&l, id), was, "keeps its cells");
        assert_eq!(l.floor(id), ultrawide().span_for((MIN_W, MIN_H)));
        while l.keyboard_adjust(id, Side::Right, Nudge::Shrink).is_ok() {}
        while l.keyboard_adjust(id, Side::Bottom, Nudge::Shrink).is_ok() {}
        let k = cells_of(&l, id);
        assert_eq!((k.cols, k.rows), ultrawide().span_for((MIN_W, MIN_H)));
    }

    #[test]
    fn ac10_a_max_is_wanted_and_clamps_the_rect() {
        let mut l = layout(vec![ultrawide()]);
        let small = Hints::new((0, 0), (800, 600));
        let (id, step) = l.place_with("small", (1720, 700), small, false).unwrap();
        assert_eq!(step, Step::Nearest);
        let k = cells_of(&l, id);
        assert_eq!(
            (k.cols, k.rows),
            ultrawide().span_for((800 + GAP, 600 + GAP))
        );
        assert!(k.cols < 12, "not half the monitor");
        let r = l.rect(id).unwrap();
        assert!(r.w <= 800 && r.h <= 600, "{r:?}");
        assert!(l.maximize(id));
        let full = ultrawide().window_rect(FULL, STRIP);
        assert_eq!(
            l.rect(id),
            Some(Rect {
                w: 800,
                h: 600,
                ..full
            })
        );
        assert!(l.fullscreen(id));
        assert_eq!(l.rect(id), Some(ultrawide().full));
    }

    #[test]
    fn ac11_a_re_placed_size_persists_and_floats_leave_memory_alone() {
        let (mut l, [_, _, late, _]) = quarters();
        let Fit::Replaced(_, k) = l.set_hints(late, LATE, (0, 0)) else {
            panic!("not re-placed");
        };
        let text = l.memory_text();
        assert!(text.starts_with("navi-memory 2\n"));
        let r = ultrawide().cells_rect(k);
        let line = format!(
            "late uw {} {} {} {} {} {}",
            k.col, k.row, k.cols, k.rows, r.w, r.h
        );
        assert!(text.contains(&line), "{text}");

        let mut fresh = layout(vec![ultrawide()]);
        fresh.load_memory(&text).unwrap();
        let hints = Hints::new(LATE, (0, 0));
        let (id, step) = fresh.place_with("late", (10, 10), hints, false).unwrap();
        assert_eq!((step, cells_of(&fresh, id)), (Step::Remembered, k));
        assert_eq!(fresh.set_hints(id, LATE, (0, 0)), Fit::Unchanged);

        let mut l = layout(vec![laptop()]);
        l.place("upd", (10, 10)).unwrap();
        l.remove(0);
        let text = l.memory_text();
        let (f, step) = l.place_with("upd", (10, 10), fixed(), true).unwrap();
        assert_eq!(step, Step::Floating);
        assert_eq!(l.set_hints(f, (1300, 600), (0, 0)), Fit::Floated);
        assert_eq!(l.memory_text(), text);
    }

    /// Every rig layout from these tests, plus client sizes and a float.
    fn sized_layouts() -> Vec<Layout> {
        let mut out = vec![rig_with_tiles().0, quarters().0, laptop_float().0];
        let mut l = layout(vec![ultrawide()]);
        two_halves(&mut l);
        l.place_with("upd", (3400, 10), fixed(), true).unwrap();
        l.place_with("small", (3400, 10), Hints::new((0, 0), (500, 400)), false)
            .unwrap();
        out.push(l);
        let (mut l, [_, _, late, _]) = quarters();
        l.set_hints(late, LATE, (0, 0));
        out.push(l);
        let mut l = layout(rig());
        for n in 0..9 {
            l.place_with(
                &format!("app{n}"),
                (2000, 2200),
                Hints::new((0, 0), (900, 700)),
                n % 2 == 0,
            )
            .unwrap();
        }
        l.place_with("upd", (100, 2400), fixed(), true).unwrap();
        out.push(l);
        out
    }

    #[test]
    fn ac12_clips_never_overlap_other_tiles_or_zones() {
        for l in sized_layouts() {
            let shown: Vec<&Tile> = l.tiles.iter().filter(|t| l.visible(t.id)).collect();
            for t in &shown {
                let clip = l.clip(t.id).unwrap();
                let o = l.output(&t.output).unwrap();
                if t.floating {
                    assert!(inside(clip, o.full), "{t:?}");
                    continue;
                }
                assert!(inside(clip, l.frame(t.id).unwrap().0), "{t:?}");
                for u in shown.iter().filter(|u| u.id != t.id && !u.floating) {
                    assert!(!overlaps(clip, l.clip(u.id).unwrap()), "{t:?} {u:?}");
                    let zones = [
                        Hit::Strip(u.id),
                        Hit::Edge(u.id, Side::Left),
                        Hit::Edge(u.id, Side::Right),
                        Hit::Edge(u.id, Side::Top),
                        Hit::Edge(u.id, Side::Bottom),
                    ];
                    for z in zones.into_iter().filter_map(|h| l.zone(h)) {
                        assert!(!overlaps(clip, z), "{t:?} {u:?} {z:?}");
                    }
                }
            }
        }
    }

    // Floating windows (job 019).

    use crate::layout::EdgeClicks;
    use float::{FLOAT_EDGE, bounds};

    /// Tiles whose client minimum blocks 017's shrink on the ultrawide under its panels: 11
    /// columns each, and too tall for a tab's bar.
    const BLOCK: (i32, i32) = (1500, 1300);

    /// The ultrawide under its panels (plus `more`), two blocking halves, and a float of `hints`
    /// placed from the middle: 017 AC4's fixed window when `hints` is `fixed()`.
    fn blocked(more: Vec<Output>, hints: Hints) -> (Layout, [WinId; 2], WinId) {
        let mut outputs = vec![uw_panels()];
        outputs.extend(more);
        let mut l = layout(outputs);
        let (a, _) = l.place("a", (100, 100)).unwrap();
        let (b, _) = l.place("b", (3400, 100)).unwrap();
        for id in [a, b] {
            assert_eq!(l.set_hints(id, BLOCK, (0, 0)), Fit::Fits);
        }
        let (f, step) = l.place_with("upd", (1720, 600), hints, true).unwrap();
        assert_eq!(step, Step::Floating);
        (l, [a, b], f)
    }

    /// A float that can grow and shrink: at least 1220 x 600, no max.
    fn sizable() -> Hints {
        Hints::new(FIXED, (0, 0))
    }

    /// `blocked` with a `sizable` float shrunk to its minimum by the keyboard, then moved
    /// three cells down: room on every side.
    fn blocked_small() -> (Layout, [WinId; 2], WinId) {
        let (mut l, tiles, f) = blocked(vec![], sizable());
        for side in [Side::Right, Side::Bottom] {
            while l.keyboard_adjust(f, side, Nudge::Shrink).is_ok() {}
        }
        for _ in 0..3 {
            l.keyboard_adjust(f, Side::Bottom, Nudge::Move).unwrap();
        }
        assert_eq!(l.clip(f).map(|r| (r.w, r.h)), Some(FIXED));
        (l, tiles, f)
    }

    fn outer_of(l: &Layout, id: WinId) -> Rect {
        l.frame(id).unwrap().0
    }

    fn bounds_of(l: &Layout, id: WinId) -> Rect {
        bounds(l.output(&l.tile(id).unwrap().output).unwrap())
    }

    fn tiles_cells(l: &Layout) -> Vec<(WinId, String, Cells, bool)> {
        let t = l.tiles.iter().filter(|t| !t.floating);
        t.map(|t| (t.id, t.output.clone(), t.cells, t.floating))
            .collect()
    }

    fn shift(r: Rect, dx: i32, dy: i32) -> Rect {
        Rect {
            x: r.x + dx,
            y: r.y + dy,
            ..r
        }
    }

    /// Presses at `from`, moves through `path`, releases at its last point.
    fn float_drag(l: &mut Layout, from: (i32, i32), path: &[(i32, i32)]) -> Option<Done> {
        let mut d = Drag::press(l, from).expect("a float zone");
        for &p in path {
            d.motion(l, p, 0);
            assert_floats_contained(l);
        }
        let last = *path.last().unwrap_or(&from);
        let done = d.release(l, last, 0);
        assert_floats_contained(l);
        done
    }

    /// AC8: every float that fits has its outer rect inside its bounds and its clip inside the
    /// area; no tile's clip or zones overlap another tile's.
    fn assert_floats_contained(l: &Layout) {
        for t in l.tiles.iter().filter(|t| t.floating && !t.fullscreen) {
            let o = l.output(&t.output).unwrap();
            let b = bounds(o);
            let (outer, clip) = (outer_of(l, t.id), l.clip(t.id).unwrap());
            let min = (t.hints.min.0.max(MIN_W), t.hints.min.1.max(MIN_H));
            if min.0 <= b.w && min.1 + BAR <= b.h {
                assert!(inside(outer, b), "{t:?}: {outer:?} outside {b:?}");
                assert!(inside(clip, o.area), "{t:?}");
                assert!(clip.w >= min.0 && clip.h >= min.1, "{t:?}: {clip:?}");
                for (v, max) in [(clip.w, t.hints.max.0), (clip.h, t.hints.max.1)] {
                    assert!(max == 0 || v <= max, "{t:?}: {clip:?}");
                }
            }
            assert!(outer.y >= o.area.y, "{t:?}: its bar above the area");
        }
        let shown: Vec<&Tile> = l
            .tiles
            .iter()
            .filter(|t| l.visible(t.id) && !t.floating)
            .collect();
        for t in &shown {
            let clip = l.clip(t.id).unwrap();
            for u in shown.iter().filter(|u| u.id != t.id) {
                assert!(!overlaps(clip, l.clip(u.id).unwrap()), "{t:?} {u:?}");
                let zones = [Side::Left, Side::Right, Side::Top, Side::Bottom]
                    .map(|s| Hit::Edge(u.id, s))
                    .into_iter()
                    .chain([Hit::Strip(u.id)]);
                for z in zones.filter_map(|h| l.zone(h)) {
                    assert!(!overlaps(clip, z), "{t:?} {u:?} {z:?}");
                }
            }
        }
    }

    /// Round 1 (security probe): one float order. A rear float's bar and buttons under the
    /// front window are drawn below it and take no clicks; focusing the rear float swaps both.
    #[test]
    fn floats_draw_hit_and_raise_in_one_order() {
        let (mut l, [a, _], f) = blocked(vec![], fixed());
        let (g, _) = l.place_with("upd2", (1720, 600), fixed(), true).unwrap();
        for _ in 0..3 {
            l.keyboard_adjust(f, Side::Left, Nudge::Move).unwrap();
        }
        for _ in 0..2 {
            l.keyboard_adjust(f, Side::Bottom, Nudge::Move).unwrap();
        }
        assert_eq!(l.floats(), [g, f], "front first");
        let front = l.clip(g).unwrap();
        let rear_bar = l.bar(f).unwrap();
        let close = rear_bar.buttons[2].1;
        let x = (close.x + close.w / 2, close.y + close.h / 2);
        assert!(front.contains(x), "the rear ✕ lies under the front window");
        assert_eq!(l.hit(x), None, "the front window's client, not the rear ✕");
        // Render draws each float's decorations right above its window, front float first.
        let drawn: Vec<WinId> = float_overlay(&l, None, (0, 0), None)
            .into_iter()
            .map(|(id, ..)| id)
            .collect();
        let last_g = drawn.iter().rposition(|&id| id == g).unwrap();
        assert!(drawn[..=last_g].iter().all(|&id| id == g), "{drawn:?}");
        assert!(drawn[last_g + 1..].iter().all(|&id| id == f), "{drawn:?}");

        // Focusing the rear float brings it in front for drawing and hits alike.
        assert!(l.raise(f));
        assert!(!l.raise(f), "already in front");
        assert!(!l.raise(a), "tiles don't take part");
        assert_eq!(l.floats(), [f, g]);
        let strip = (
            rear_bar.rect.x + rear_bar.rect.w - 300,
            rear_bar.rect.y + 12,
        );
        assert!(front.contains(strip));
        assert_eq!(l.hit(strip), Some(Hit::Strip(f)));
        assert_eq!(l.hit(x), Some(Hit::Button(f, Button::Close)));
        let g_bar = l.bar(g).unwrap().rect;
        let under = (g_bar.x + 10, g_bar.y + 12);
        if l.clip(f).unwrap().contains(under) {
            assert_eq!(l.hit(under), None);
        }
        let drawn: Vec<WinId> = float_overlay(&l, None, (0, 0), None)
            .into_iter()
            .map(|(id, ..)| id)
            .collect();
        assert_eq!(drawn.first(), Some(&f));
    }

    /// Round 3 (PR audit): a front float's dialog, larger than its window, drawn over the rear
    /// float's ✕ and band, covers them: no navi zone there, so the pointer goes to the dialog
    /// (`Navi::zone_at` passes the float of the window under the pointer as `over`).
    #[test]
    fn floats_dialog_in_front_covers_the_zones_behind_it() {
        let (mut l, [a, _], f) = blocked(vec![], fixed());
        let (g, _) = l.place_with("upd2", (1720, 600), fixed(), true).unwrap();
        for _ in 0..3 {
            l.keyboard_adjust(g, Side::Left, Nudge::Move).unwrap();
        }
        assert_eq!(l.floats(), [g, f]);
        let (front, rear) = (l.clip(g).unwrap(), outer_of(&l, f));
        let close = l.bar(f).unwrap().buttons[2].1;
        let x = (close.x + close.w / 2, close.y + close.h / 2);
        let band = (rear.x + rear.w + 3, rear.y + rear.h / 2);
        // A dialog of g centred on it, wider than it, reaching over f's ✕ and right band.
        let dialog = Rect::new(
            front.x - 300,
            front.y - 40,
            front.w + 600 + 2 * 300,
            front.h,
        );
        for p in [x, band] {
            assert!(!front.contains(p) && dialog.contains(p), "{p:?}");
            assert!(
                l.hit(p).is_some_and(|h| h.id() == f),
                "f's zone without the dialog"
            );
            assert_eq!(
                l.hit_under(p, Some(g)),
                None,
                "the dialog is in front: {p:?}"
            );
            assert_eq!(l.hit_under(p, None), l.hit(p), "no window there: f's zone");
            assert_eq!(
                l.hit_under(p, Some(f)),
                l.hit(p),
                "f's own window doesn't cover it"
            );
        }
        // A float's window or dialog also covers every tile zone.
        let a_right = l.zone(Hit::Edge(a, Side::Right)).unwrap();
        let p = (a_right.x + 1, front.y + front.h + 40);
        assert_eq!(l.hit(p), Some(Hit::Edge(a, Side::Right)));
        assert_eq!(l.hit_under(p, Some(f)), None);
        // A rear float's dialog never covers the front float's zones.
        let g_bar = (front.x + 40, front.y - 12);
        assert_eq!(l.hit(g_bar), Some(Hit::Strip(g)));
        assert_eq!(l.hit_under(g_bar, Some(f)), Some(Hit::Strip(g)));
        // Focus brings f in front; then its zones take the point back from g's dialog.
        assert!(l.raise(f));
        assert_eq!(l.hit_under(x, Some(g)), Some(Hit::Button(f, Button::Close)));
    }

    /// PR #39 audit P2: floats follow the stacking ranks first. Restoring or activating the rear
    /// float without a raise (navi's `Raise::Not`: a token that isn't the user's) touches its
    /// stamp but leaves the drawn and hit order alone; a raise of its family puts it in front.
    #[test]
    fn floats_follow_the_raise_ranks() {
        let (mut l, _, f) = blocked(vec![], fixed());
        let (g, _) = l.place_with("upd2", (1720, 600), fixed(), true).unwrap();
        // g half a window to the right of f, over f's right band.
        l.keyboard_adjust(g, Side::Right, Nudge::Move).unwrap();
        l.set_rank(f, 2);
        l.set_rank(g, 4);
        assert_eq!(l.floats(), [g, f]);
        // They overlap: a point in both windows belongs to the front one.
        let (front, rear) = (l.clip(g).unwrap(), l.clip(f).unwrap());
        let p = (front.x + front.w / 2, front.y + front.h / 2);
        assert!(rear.contains(p) && front.contains(p));
        // f's right band runs under g's window: g, in front, takes the point.
        let band = l.float_zone(f, Side::Right).unwrap();
        let q = (band.x + 1, band.y + band.h / 2);
        assert!(front.contains(q));
        assert_eq!(l.hit(q), None, "inside g's window");
        // A minimize and an activation without a raise: the stamp moves, the order doesn't.
        l.minimize(f);
        assert!(l.activate(f));
        assert_eq!(l.floats(), [g, f], "no raise, no move");
        assert_eq!(l.hit(q), None, "g still covers f's band");
        // A raise of f's family (its rank passes g's) puts it in front.
        l.set_rank(f, 6);
        assert_eq!(l.floats(), [f, g]);
        assert_eq!(l.hit(q).map(|h| h.id()), Some(f), "f's band is f's again");
    }

    /// Job 020 AC5: a front float's dialog of a dialog has the float as its root, so it covers
    /// the rear float's zones as the float's own dialog does.
    #[test]
    fn floats_dialog_of_dialog_covers_the_zones_behind_it() {
        use dialog::{Root, place, root};
        let (mut l, _, f) = blocked(vec![], fixed());
        let (g, _) = l.place_with("upd2", (1720, 600), fixed(), true).unwrap();
        for _ in 0..3 {
            l.keyboard_adjust(g, Side::Left, Nudge::Move).unwrap();
        }
        assert_eq!(l.floats(), [g, f]);
        // Windows 0: float g, 1: its dialog, 2: that dialog's dialog.
        let parents = [None, Some(0), Some(1)];
        let owner = root(2, |i| parents[i], |i| (i == 0).then_some(g));
        assert_eq!(owner, Root::Tiled(g));
        let Root::Tiled(owner) = owner else {
            unreachable!()
        };
        let (front, rear) = (l.clip(g).unwrap(), outer_of(&l, f));
        let b = bounds_of(&l, g);
        let d1 = place(front, (200, 200), b);
        let d2 = place(d1, (b.w, front.h), b);
        let band = (rear.x + rear.w + 3, rear.y + rear.h / 2);
        assert!(!front.contains(band) && d2.contains(band), "{d2:?}");
        assert!(l.hit(band).is_some_and(|h| h.id() == f));
        assert_eq!(
            l.hit_under(band, Some(owner)),
            None,
            "the chain is in front"
        );
        assert_eq!(l.hit_under(band, Some(f)), l.hit(band));
    }

    /// Round 1 (security probe): unplugging the monitor under a float drag leaves the float on a
    /// live monitor, whether the drag is cancelled or released.
    #[test]
    fn unplug_mid_drag_keeps_the_float_on_a_live_monitor() {
        for cancel in [true, false] {
            // The laptop is left, too narrow for it: it stays a float there.
            let lp = out("laptop", -1125, 600, 1125, 703, 1);
            let (mut l, _, f) = blocked(vec![lp], fixed());
            let start = outer_of(&l, f);
            let from = (start.x + 300, start.y + 12);
            let mut d = Drag::press(&l, from).unwrap();
            d.motion(&mut l, (from.0 + 40, from.1 + 40), 0);
            l.remove_output("uwp");
            assert!(l.tile(f).unwrap().floating);
            if cancel {
                d.cancel(&mut l);
            } else {
                // Over the laptop, where 1220 px doesn't fit.
                d.motion(&mut l, (-600, 900), 0);
                d.release(&mut l, (-600, 900), 0);
            }
            let t = l.tile(f).unwrap();
            assert!(l.output(&t.output).is_some(), "cancel={cancel}: {t:?}");
            assert!(l.rect(f).is_some(), "cancel={cancel}");
            assert_floats_contained(&l);
        }
    }

    #[test]
    fn ac1_float_zones() {
        let (mut l, [a, _], f) = blocked(vec![], fixed());
        let o = outer_of(&l, f);
        let r = l.clip(f).unwrap();
        let e = FLOAT_EDGE;
        assert_eq!(e, 6);
        assert_eq!(l.hit((o.x + 40, o.y + 10)), Some(Hit::Strip(f)), "the bar");
        for (b, br) in l.bar(f).unwrap().buttons {
            assert_eq!(l.hit((br.x + 12, br.y + 12)), Some(Hit::Button(f, b)));
        }
        let (mx, my) = (o.x + o.w / 2, o.y + o.h / 2);
        for (p, side) in [
            ((o.x - 1, my), Side::Left),
            ((o.x - e, my), Side::Left),
            ((o.x + o.w, my), Side::Right),
            ((o.x + o.w + e - 1, my), Side::Right),
            ((mx, o.y - 1), Side::Top),
            ((mx, o.y - e), Side::Top),
            ((mx, o.y + o.h), Side::Bottom),
            ((mx, o.y + o.h + e - 1), Side::Bottom),
        ] {
            assert_eq!(l.hit(p), Some(Hit::Edge(f, side)), "{p:?}");
        }
        for p in [(o.x - e - 1, my), (mx, o.y - e - 1), (mx, o.y + o.h + e)] {
            assert!(
                l.hit(p).is_none_or(|h| h.id() != f),
                "{p:?} outside the band"
            );
        }
        // The 32 px of each band nearest a corner.
        let (left, right, top, bottom) = (o.x - e, o.x + o.w + e - 1, o.y - e, o.y + o.h + e - 1);
        for (x, sx) in [(left, Side::Left), (right, Side::Right)] {
            for (y, sy) in [(top, Side::Top), (bottom, Side::Bottom)] {
                assert_eq!(l.hit((x, y)), Some(Hit::Corner(f, sx, sy)));
            }
        }
        let c = |p| l.hit(p);
        assert_eq!(
            c((left, top + 31)),
            Some(Hit::Corner(f, Side::Left, Side::Top))
        );
        assert_eq!(c((left, top + 32)), Some(Hit::Edge(f, Side::Left)));
        assert_eq!(
            c((left, bottom - 31)),
            Some(Hit::Corner(f, Side::Left, Side::Bottom))
        );
        assert_eq!(c((left, bottom - 32)), Some(Hit::Edge(f, Side::Left)));
        assert_eq!(
            c((left + 31, top)),
            Some(Hit::Corner(f, Side::Left, Side::Top))
        );
        assert_eq!(c((left + 32, top)), Some(Hit::Edge(f, Side::Top)));
        assert_eq!(
            c((right - 31, bottom)),
            Some(Hit::Corner(f, Side::Right, Side::Bottom))
        );
        assert_eq!(c((right - 32, bottom)), Some(Hit::Edge(f, Side::Bottom)));
        for x in (r.x..r.x + r.w).step_by(7) {
            for y in (r.y..r.y + r.h).step_by(5) {
                assert_eq!(l.hit((x, y)), None, "({x}, {y}) is the client's");
            }
        }
        // A band px over a tile's client, and over a tile's edge zone.
        let client = (o.x - 3, my);
        assert!(l.clip(a).unwrap().contains(client));
        assert_eq!(l.hit(client), Some(Hit::Edge(f, Side::Left)));
        let a_right = l.zone(Hit::Edge(a, Side::Right)).unwrap();
        let p = (a_right.x + 1, o.y - 3);
        assert!(a_right.contains(p));
        assert_eq!(l.hit(p), Some(Hit::Edge(f, Side::Top)));

        // A hovered band lights in the float layer, above the tiles, and not below them.
        let lit = crate::theme::rgba(crate::theme::ACCENT, 1.0);
        let zone = l.zone(Hit::Edge(f, Side::Top)).unwrap();
        assert!(float_overlay(&l, None, p, None).contains(&(f, zone, lit)));
        assert!(
            !overlay(&l, None, p, 0, None)
                .iter()
                .any(|&(r, k)| k == lit && overlaps(r, zone))
        );
        let corner = (left, top);
        let glow = float_overlay(&l, None, corner, None);
        for s in [Side::Left, Side::Top] {
            assert!(
                glow.contains(&(f, l.zone(Hit::Edge(f, s)).unwrap(), lit)),
                "{s:?}"
            );
        }

        // Two floats: the front one's window hides the rear one's zones under it.
        let (g, step) = l.place_with("upd2", (1720, 600), fixed(), true).unwrap();
        assert_eq!(step, Step::Floating);
        for _ in 0..2 {
            l.keyboard_adjust(f, Side::Right, Nudge::Move).unwrap();
        }
        let (rear, front) = (outer_of(&l, f), l.clip(g).unwrap());
        let under = (rear.x - 3, rear.y + rear.h / 2);
        assert!(front.contains(under));
        assert_eq!(l.hit(under), None, "the front float's client");
        let beyond = (rear.x + rear.w + 3, rear.y + rear.h / 2);
        assert!(!outer_of(&l, g).contains(beyond));
        assert_eq!(l.hit(beyond), Some(Hit::Edge(f, Side::Right)));
        let bar = (rear.x + 100, rear.y + 5);
        assert!(front.contains(bar) || outer_of(&l, g).contains(bar));
        assert_ne!(l.hit(bar), Some(Hit::Strip(f)));

        // Minimized or fullscreen: no zones, and the tile's show through again.
        assert!(l.minimize(g) && l.minimize(f));
        assert_eq!(l.hit(p), Some(Hit::Edge(a, Side::Right)));
        assert!(l.activate(f));
        assert_eq!(
            l.hit((rear.x - 1, rear.y + 100)),
            Some(Hit::Edge(f, Side::Left))
        );
        assert!(l.fullscreen(f));
        for q in [(rear.x - 1, rear.y + 100), (rear.x + 40, rear.y + 10)] {
            assert!(l.hit(q).is_none_or(|h| h.id() != f), "{q:?}");
        }
    }

    #[test]
    fn ac2_a_float_follows_its_bar() {
        let (mut l, _, f) = blocked(vec![], fixed());
        let (tiles, memory) = (tiles_cells(&l), l.memory_text());
        let start = outer_of(&l, f);
        let from = (start.x + 300, start.y + 12);
        let grab = (from.0 - start.x, from.1 - start.y);
        let mut d = Drag::press(&l, from).unwrap();
        d.motion(&mut l, (from.0 + 5, from.1 + 5), 0);
        assert_eq!(outer_of(&l, f), start, "under ARM_PX");
        for (dx, dy) in [(120, 60), (121, 60), (-300, -200), (40, 300), (0, 1)] {
            let p = (from.0 + dx, from.1 + dy);
            d.motion(&mut l, p, 0);
            let now = outer_of(&l, f);
            assert_eq!(now, shift(start, dx, dy), "({dx}, {dy})");
            assert_eq!((p.0 - now.x, p.1 - now.y), grab);
        }
        let b = bounds_of(&l, f);
        let wild = [i32::MIN, -100_000, 0, 100_000, i32::MAX];
        for x in wild {
            for y in wild {
                d.motion(&mut l, (x, y), 0);
                let now = outer_of(&l, f);
                assert!(inside(now, b), "({x}, {y}): {now:?}");
                assert!(now.y >= l.output("uwp").unwrap().area.y + FLOAT_MARGIN);
            }
        }
        let to = (from.0 + 120, from.1 + 60);
        let Some(Done::Layout("float moved", moved)) = d.release(&mut l, to, 0) else {
            panic!("not moved");
        };
        assert_eq!(moved.len(), 1);
        assert_eq!(outer_of(&l, f), shift(start, 120, 60));
        assert_eq!((tiles_cells(&l), l.memory_text()), (tiles, memory));
        assert!(l.tile(f).unwrap().floating);

        let was = outer_of(&l, f);
        let from = (was.x + 300, was.y + 12);
        let mut d = Drag::press(&l, from).unwrap();
        d.motion(&mut l, (from.0 - 400, from.1 + 90), 0);
        assert_ne!(outer_of(&l, f), was);
        assert!(d.cancel(&mut l));
        assert_eq!(outer_of(&l, f), was, "cancel puts it back");
        // A click on the bar moves nothing.
        assert_eq!(float_drag(&mut l, from, &[(from.0 + 3, from.1)]), None);
        assert_eq!(outer_of(&l, f), was);
    }

    /// The rig at logical size: the laptop is 1125 x 703, narrower than a 1220 px float.
    fn logical_rig() -> Vec<Output> {
        vec![
            out("4k", 0, -2160, 3840, 2160, 1),
            out("laptop", -1125, 600, 1125, 703, 2),
        ]
    }

    /// `blocked` with the 4K filled, so the float stays on the ultrawide.
    fn blocked_rig() -> (Layout, [WinId; 2], WinId, WinId) {
        let mut l = layout(vec![uw_panels()]);
        l.set_outputs([vec![uw_panels()], logical_rig()].concat());
        let k = Tile {
            id: 50,
            app: "full".into(),
            output: "4k".into(),
            cells: FULL,
            ..Tile::default()
        };
        l.tiles.push(k);
        l.next_id = 51;
        let (a, _) = l.place("a", (100, 100)).unwrap();
        let (b, _) = l.place("b", (3400, 100)).unwrap();
        for id in [a, b] {
            assert_eq!(l.set_hints(id, BLOCK, (0, 0)), Fit::Fits);
        }
        let (f, step) = l.place_with("upd", (1720, 600), fixed(), true).unwrap();
        assert_eq!(step, Step::Floating);
        assert_eq!(l.tile(f).unwrap().output, "uwp");
        (l, [a, b], f, 50)
    }

    #[test]
    fn ac3_a_float_moves_between_monitors_its_minimum_fits() {
        let (mut l, _, f, full) = blocked_rig();
        let start = outer_of(&l, f);
        let from = (start.x + 300, start.y + 12);
        // Over the laptop, narrower than 1220: it stops at the ultrawide's left bound.
        let done = float_drag(&mut l, from, &[(-500, 900)]);
        assert!(matches!(done, Some(Done::Layout("float moved", _))));
        let (t, now) = (l.tile(f).unwrap(), outer_of(&l, f));
        assert_eq!(t.output, "uwp");
        assert_eq!(now.x, bounds_of(&l, f).x);

        // Free cells on the 4K only: it stays floating.
        l.remove(full);
        assert!(l.tile_floats().is_empty());
        assert!(l.tile(f).unwrap().floating);

        let from = (now.x + 300, now.y + 12);
        let done = float_drag(&mut l, from, &[(2000, -1000)]);
        assert!(matches!(done, Some(Done::Layout("float moved", _))));
        let t = l.tile(f).unwrap();
        assert_eq!(t.output, "4k");
        assert!(inside(outer_of(&l, f), bounds_of(&l, f)));
        let (outer, grabbed) = (outer_of(&l, f), (2000 - 300, -1000 - 12));
        assert_eq!(
            (outer.x, outer.y),
            grabbed,
            "the grabbed point under the pointer"
        );

        let tiled = l.tile_floats();
        assert_eq!(tiled.len(), 1);
        let (id, output, k) = tiled[0].clone();
        assert_eq!((id, output.as_str()), (f, "4k"));
        assert!(k.cols >= 8 && k.rows >= 4, "{k:?}");
        let t = l.tile(f).unwrap();
        assert!(!t.floating && t.cells == k && t.user_rect.is_none());
        assert_no_overlap(&l);
    }

    /// The `side` band's middle, or a corner's tip.
    fn band(l: &Layout, f: WinId, hit: Hit) -> (i32, i32) {
        let (o, e) = (outer_of(l, f), FLOAT_EDGE / 2);
        let (mx, my) = (o.x + o.w / 2, o.y + o.h / 2);
        let x = |s| {
            if s == Side::Left {
                o.x - e
            } else {
                o.x + o.w + e
            }
        };
        let y = |s| {
            if s == Side::Top {
                o.y - e
            } else {
                o.y + o.h + e
            }
        };
        let p = match hit {
            Hit::Edge(_, s @ (Side::Left | Side::Right)) => (x(s), my),
            Hit::Edge(_, s) => (mx, y(s)),
            Hit::Corner(_, sx, sy) => (x(sx), y(sy)),
            _ => unreachable!(),
        };
        assert_eq!(l.hit(p), Some(hit));
        p
    }

    /// Left, top, right and bottom of a rect.
    fn sides(r: Rect) -> [i32; 4] {
        [r.x, r.y, r.x + r.w, r.y + r.h]
    }

    #[test]
    fn ac4_a_float_resizes_by_its_band() {
        let (mut l, _, f) = blocked_small();
        let memory = l.memory_text();
        let d = 37;
        for (side, k, sign) in [
            (Side::Left, 0, -1),
            (Side::Top, 1, -1),
            (Side::Right, 2, 1),
            (Side::Bottom, 3, 1),
        ] {
            let start = l.clip(f).unwrap();
            let x = matches!(side, Side::Left | Side::Right);
            let p = band(&l, f, Hit::Edge(f, side));
            let (dx, dy) = if x { (sign * d, 9) } else { (9, sign * d) };
            let done = float_drag(&mut l, p, &[(p.0 + dx, p.1 + dy)]);
            assert!(matches!(done, Some(Done::Layout("float resized", _))));
            let (was, now) = (sides(start), sides(l.clip(f).unwrap()));
            for i in 0..4 {
                let want = if i == k { was[i] + sign * d } else { was[i] };
                assert_eq!(now[i], want, "{side:?} side {i}");
            }
        }
        for (sx, sy) in [
            (Side::Left, Side::Top),
            (Side::Right, Side::Top),
            (Side::Left, Side::Bottom),
            (Side::Right, Side::Bottom),
        ] {
            let was = sides(l.clip(f).unwrap());
            let p = band(&l, f, Hit::Corner(f, sx, sy));
            let (dx, dy) = (
                if sx == Side::Left { -20 } else { 20 },
                if sy == Side::Top { -15 } else { 15 },
            );
            float_drag(&mut l, p, &[(p.0 + dx, p.1 + dy)]);
            let now = sides(l.clip(f).unwrap());
            let (kx, ky) = (
                if sx == Side::Left { 0 } else { 2 },
                if sy == Side::Top { 1 } else { 3 },
            );
            for i in 0..4 {
                let want = match i {
                    _ if i == kx => was[i] + dx,
                    _ if i == ky => was[i] + dy,
                    _ => was[i],
                };
                assert_eq!(now[i], want, "{sx:?}{sy:?} side {i}");
            }
        }
        assert_eq!(l.memory_text(), memory);

        // Every path stops at the floor, the bounds and the max.
        let b = bounds_of(&l, f);
        for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
            let mut ends = [i32::MIN / 2, i32::MAX / 2];
            if matches!(side, Side::Right | Side::Bottom) {
                ends.reverse();
            }
            // Out to the bounds, then in to the floor.
            for far in ends {
                let p = band(&l, f, Hit::Edge(f, side));
                let x = matches!(side, Side::Left | Side::Right);
                let to = if x { (far, p.1) } else { (p.0, far) };
                float_drag(&mut l, p, &[to]);
                let r = l.clip(f).unwrap();
                assert!(r.w >= FIXED.0 && r.h >= FIXED.1, "{side:?}: {r:?}");
                assert!(inside(outer_of(&l, f), b));
            }
        }
        let r = l.clip(f).unwrap();
        assert_eq!((r.w, r.h), FIXED, "shrunk to the client minimum");

        // No client minimum: navi's.
        let mut l = layout(vec![uw_panels()]);
        l.tiles.push(Tile {
            id: 7,
            app: "free".into(),
            output: "uwp".into(),
            floating: true,
            ..Tile::default()
        });
        for side in [Side::Right, Side::Bottom] {
            let p = band(&l, 7, Hit::Edge(7, side));
            float_drag(&mut l, p, &[(p.0 - 5000, p.1 - 5000)]);
        }
        let r = l.clip(7).unwrap();
        assert_eq!((r.w, r.h), (MIN_W, MIN_H));

        // A max stops it; a fixed float doesn't change.
        let max = Hints::new(FIXED, (1300, 650));
        let (mut l, _, f) = blocked(vec![], max);
        let p = band(&l, f, Hit::Corner(f, Side::Right, Side::Bottom));
        float_drag(&mut l, p, &[(p.0 + 500, p.1 + 500)]);
        let r = l.clip(f).unwrap();
        assert_eq!((r.w, r.h), (1300, 650));
        let (mut l, _, f) = blocked(vec![], fixed());
        let was = l.clip(f).unwrap();
        for hit in [
            Hit::Edge(f, Side::Left),
            Hit::Corner(f, Side::Right, Side::Bottom),
        ] {
            let p = band(&l, f, hit);
            assert_eq!(float_drag(&mut l, p, &[(p.0 + 80, p.1 + 80)]), None);
            assert_eq!(l.clip(f), Some(was));
        }

        // Double-clicking an edge fills that side to the bounds, or to the max.
        for (hints, side) in [
            (sizable(), Side::Right),
            (max, Side::Left),
            (sizable(), Side::Top),
        ] {
            let (mut l, _, f) = blocked(vec![], hints);
            if hints == max {
                l.keyboard_adjust(f, side, Nudge::Shrink).unwrap();
            }
            let (b, was) = (bounds_of(&l, f), outer_of(&l, f));
            let mut clicks = EdgeClicks::default();
            let p = band(&l, f, Hit::Edge(f, side));
            let mut done = None;
            for t in [0, 100] {
                let d = Drag::press(&l, p).unwrap();
                let edge = d.edge_click(&l, p);
                assert_eq!(d.release(&mut l, p, t), None);
                done = clicks.release(&mut l, edge, p, t);
            }
            assert!(
                matches!(done, Some(Done::Layout("float resized", _))),
                "{done:?}"
            );
            let now = outer_of(&l, f);
            match (side, hints == max) {
                (Side::Right, _) => assert_eq!((now.x, now.x + now.w), (was.x, b.x + b.w)),
                (Side::Left, true) => {
                    assert_eq!((now.x + now.w, now.w), (was.x + was.w, 1300));
                }
                _ => assert_eq!((now.y, now.y + now.h), (b.y, was.y + was.h)),
            }
        }

        // Cancel restores.
        let (mut l, _, f) = blocked(vec![], sizable());
        let was = outer_of(&l, f);
        let p = band(&l, f, Hit::Edge(f, Side::Right));
        let mut d = Drag::press(&l, p).unwrap();
        d.motion(&mut l, (p.0 + 100, p.1), 0);
        assert_eq!(outer_of(&l, f).w, was.w + 100);
        assert!(d.cancel(&mut l));
        assert_eq!(outer_of(&l, f), was);
    }

    #[test]
    fn ac5_keyboard_moves_and_resizes_a_float_by_cells() {
        let (mut l, _, f) = blocked_small();
        let (tiles, memory) = (tiles_cells(&l), l.memory_text());
        let o = uw_panels();
        let (cw, ch) = (o.cell_w(), o.cell_h());
        let b = bounds_of(&l, f);
        let mut steps = Vec::new();
        loop {
            let was = outer_of(&l, f);
            match l.keyboard_adjust(f, Side::Right, Nudge::Move) {
                Ok(Done::Layout("float moved", _)) => steps.push(outer_of(&l, f).x - was.x),
                Ok(other) => panic!("{other:?}"),
                Err(why) => {
                    assert_eq!(why, "monitor edge");
                    break;
                }
            }
            assert_eq!(outer_of(&l, f).w, was.w);
        }
        let (last, full) = steps.split_last().unwrap();
        assert!(
            full.iter().all(|&s| s == cw) && *last <= cw && *last > 0,
            "{steps:?}"
        );
        assert_eq!(outer_of(&l, f).x + outer_of(&l, f).w, b.x + b.w);
        // Moving up.
        let was = outer_of(&l, f);
        l.keyboard_adjust(f, Side::Top, Nudge::Move).unwrap();
        assert_eq!(outer_of(&l, f), shift(was, 0, -ch));

        // Grow left to the bounds, shrink it back to the floor.
        loop {
            let was = outer_of(&l, f);
            match l.keyboard_adjust(f, Side::Left, Nudge::Grow) {
                Ok(_) => {
                    let now = outer_of(&l, f);
                    assert_eq!(now.x + now.w, was.x + was.w, "the right side stays");
                    assert!(now.w - was.w == cw || now.x == b.x);
                }
                Err(why) => {
                    assert_eq!(why, "monitor edge");
                    break;
                }
            }
        }
        assert_eq!(outer_of(&l, f).x, b.x);
        loop {
            let was = outer_of(&l, f);
            match l.keyboard_adjust(f, Side::Left, Nudge::Shrink) {
                Ok(Done::Layout("float resized", _)) => {
                    let now = outer_of(&l, f);
                    assert!(now.w == was.w - cw || now.w == FIXED.0, "{now:?}");
                }
                Ok(other) => panic!("{other:?}"),
                Err(why) => {
                    assert_eq!(why, "float at minimum");
                    break;
                }
            }
        }
        assert_eq!(l.clip(f).unwrap().w, FIXED.0);
        assert_eq!((tiles_cells(&l), l.memory_text()), (tiles, memory));

        // A max stops growth there.
        let (mut l, _, f) = blocked(vec![], Hints::new(FIXED, (1300, 650)));
        assert_eq!(l.clip(f).unwrap().h, 650, "its wanted size is its max");
        assert!(l.keyboard_adjust(f, Side::Bottom, Nudge::Shrink).is_ok());
        assert_eq!(
            l.clip(f).unwrap().h,
            FIXED.1,
            "less than a cell to the floor"
        );
        assert!(l.keyboard_adjust(f, Side::Bottom, Nudge::Grow).is_ok());
        assert_eq!(l.clip(f).unwrap().h, 650);
        assert_eq!(
            l.keyboard_adjust(f, Side::Bottom, Nudge::Grow),
            Err("float at maximum")
        );
        let (mut l, _, f) = blocked(vec![], fixed());
        assert_eq!(
            l.keyboard_adjust(f, Side::Right, Nudge::Grow),
            Err("float at maximum")
        );
        assert_eq!(
            l.keyboard_adjust(f, Side::Right, Nudge::Shrink),
            Err("float at minimum")
        );
        assert!(l.minimize(f));
        for op in [Nudge::Move, Nudge::Grow, Nudge::Shrink] {
            assert_eq!(
                l.keyboard_adjust(f, Side::Left, op),
                Err("not a shown tile")
            );
        }
        assert!(l.activate(f) && l.fullscreen(f));
        assert_eq!(
            l.keyboard_adjust(f, Side::Left, Nudge::Move),
            Err("not a shown tile")
        );
    }

    #[test]
    fn ac6_the_user_rect_survives_until_the_float_tiles_or_its_output_goes() {
        let (mut l, [a, b], f) = blocked_small();
        let p = (outer_of(&l, f).x + 300, outer_of(&l, f).y + 12);
        float_drag(&mut l, p, &[(p.0 - 200, p.1 + 100)]);
        let e = band(&l, f, Hit::Edge(f, Side::Right));
        float_drag(&mut l, e, &[(e.0 + 60, e.1)]);
        let kept = l.clip(f).unwrap();
        assert!(l.tile(f).unwrap().user_rect.is_some());

        // Tiles shrink and grow, a window opens and closes: none of it tiles the float.
        l.keyboard_adjust(a, Side::Right, Nudge::Shrink).unwrap();
        assert!(l.tile_floats().is_empty());
        let (c, _) = l.place("c", (100, 100)).unwrap();
        assert!(l.tile_floats().is_empty());
        l.remove(c);
        l.keyboard_adjust(a, Side::Right, Nudge::Grow).unwrap();
        assert!(l.tile_floats().is_empty());
        assert_eq!(l.clip(f), Some(kept));

        // New hints: up to the min, down to the max, the top-left kept.
        assert_eq!(l.set_hints(f, (1400, 700), (0, 0)), Fit::Floated);
        let r = l.clip(f).unwrap();
        assert_eq!(
            r,
            Rect {
                w: 1400,
                h: 700,
                ..kept
            }
        );
        assert_eq!(l.set_hints(f, FIXED, (1250, 620)), Fit::Floated);
        let r = l.clip(f).unwrap();
        assert_eq!(
            r,
            Rect {
                w: 1250,
                h: 620,
                ..kept
            }
        );
        // Near the bounds' right, a bigger min keeps it inside.
        let p = (outer_of(&l, f).x + 300, outer_of(&l, f).y + 12);
        float_drag(&mut l, p, &[(i32::MAX, p.1)]);
        assert_eq!(l.set_hints(f, (1600, 700), (0, 0)), Fit::Floated);
        let bd = bounds_of(&l, f);
        let o = outer_of(&l, f);
        assert_eq!((o.x + o.w, o.w), (bd.x + bd.w, 1600));

        // A taller band re-clamps it.
        let mut short = uw_panels();
        short.area = Rect {
            y: 400,
            h: 932,
            ..short.area
        };
        float_drag(&mut l, (o.x + 300, o.y + 12), &[(o.x + 300, i32::MIN)]);
        l.set_outputs(vec![short.clone()]);
        assert!(inside(outer_of(&l, f), bounds(&short)));
        assert_eq!(outer_of(&l, f).y, short.area.y + FLOAT_MARGIN);

        // Tiled, then floating again: centred.
        l.set_outputs(vec![uw_panels()]);
        l.remove(b);
        assert_eq!(l.tile_floats().len(), 1);
        assert!(l.tile(f).unwrap().user_rect.is_none());
        assert_eq!(l.set_hints(f, BLOCK, (0, 0)), Fit::Floated);
        let (o, bd) = (outer_of(&l, f), bounds_of(&l, f));
        assert!((o.x - bd.x - (bd.x + bd.w - o.x - o.w)).abs() <= 1, "{o:?}");
        assert!(l.tile(f).unwrap().user_rect.is_none());

        // Unplugged: 017's centring on the remaining output, here too narrow for it.
        let lp = out("laptop", -1125, 600, 1125, 703, 1);
        let (mut l, _, f) = blocked(vec![lp.clone()], fixed());
        let p = (outer_of(&l, f).x + 300, outer_of(&l, f).y + 12);
        float_drag(&mut l, p, &[(p.0 + 500, p.1 + 200)]);
        l.remove_output("uwp");
        let t = l.tile(f).unwrap().clone();
        assert!(
            t.floating && t.output == "laptop" && t.user_rect.is_none(),
            "{t:?}"
        );
        let mut fresh = layout(vec![lp.clone()]);
        let (g, _) = fresh.place_with("upd", (-500, 900), fixed(), true).unwrap();
        assert_eq!(l.clip(f), fresh.clip(g));
        let o = outer_of(&l, f);
        assert_eq!(
            (o.x, o.w),
            (lp.full.x, lp.full.w),
            "017's cut: the monitor's width"
        );
    }

    #[test]
    fn ac7_floats_tile_when_room_frees() {
        let (mut l, [a, b], f) = blocked(vec![], fixed());
        let before = (l.tiles.clone(), l.memory.clone(), l.clock, l.memory_text());
        for _ in 0..1000 {
            assert!(l.tile_floats().is_empty());
        }
        assert_eq!(
            (l.tiles.clone(), l.memory.clone(), l.clock, l.memory_text()),
            before
        );

        let centre = outer_of(&l, f).center();
        let a_was = cells_of(&l, a);
        l.remove(b);
        let tiled = l.tile_floats();
        assert_eq!(tiled.len(), 1);
        let (id, output, k) = tiled[0].clone();
        assert_eq!((id, output.as_str()), (f, "uwp"));
        assert!(k.cols >= 9 && k.rows >= 6, "{k:?}");
        let o = uw_panels();
        let d = dist2(o.cells_rect(k).center(), centre);
        let mut rest = l.tiles.clone();
        rest.retain(|t| t.id != f);
        let others = Layout {
            tiles: rest,
            ..Layout::default()
        };
        for c in spots(k.cols, k.rows).filter(|&c| others.is_free("uwp", c)) {
            assert!(
                dist2(o.cells_rect(c).center(), centre) >= d,
                "{c:?} is nearer"
            );
        }
        assert!(!k.overlaps(&cells_of(&l, a)));
        assert_eq!(cells_of(&l, a), a_was);
        let t = l.tile(f).unwrap();
        assert!(!t.floating && t.cells == k);
        assert_eq!(l.memory["upd"].cells, k);
        assert_eq!(l.rect(f).map(|r| (r.w, r.h)), Some(FIXED));
        assert_no_overlap(&l);
        assert!(l.tile_floats().is_empty(), "tiled once");

        // Minimized or fullscreen floats wait; restoring is a layout event.
        let (mut l, [_, b], f) = blocked(vec![], fixed());
        assert!(l.minimize(f));
        l.remove(b);
        assert!(l.tile_floats().is_empty());
        assert!(l.activate(f));
        assert_eq!(l.tile_floats().len(), 1);
        let (mut l, [_, b], f) = blocked(vec![], fixed());
        assert!(l.fullscreen(f));
        l.remove(b);
        assert!(l.tile_floats().is_empty());
        assert!(l.unfullscreen(f));
        assert_eq!(l.tile_floats().len(), 1);

        // A maximized group would cover it: it waits for the maximize to end.
        let (mut l, [a, b], _) = blocked(vec![], fixed());
        l.remove(b);
        assert!(l.maximize(a));
        assert!(l.tile_floats().is_empty());
        assert!(l.unmaximize(a));
        assert_eq!(l.tile_floats().len(), 1);

        // Two floats: the most recent first, never overlapping.
        let (mut l, [a, b], f) = blocked(vec![], fixed());
        let (g, step) = l.place_with("upd2", (1720, 600), fixed(), true).unwrap();
        assert_eq!(step, Step::Floating);
        // Apart, so the first one's cells don't split the room the second needs.
        while l.keyboard_adjust(f, Side::Left, Nudge::Move).is_ok() {}
        while l.keyboard_adjust(g, Side::Right, Nudge::Move).is_ok() {}
        l.remove(a);
        l.remove(b);
        let tiled = l.tile_floats();
        let ids: Vec<WinId> = tiled.iter().map(|t| t.0).collect();
        assert_eq!(ids, [g, f]);
        assert!(!cells_of(&l, f).overlaps(&cells_of(&l, g)));
        assert_no_overlap(&l);
    }

    #[test]
    fn ac8_floats_stay_in_bounds_whatever_the_user_does() {
        let mut layouts = sized_layouts();
        layouts.push(blocked(vec![], fixed()).0);
        layouts.push(blocked(vec![], sizable()).0);
        layouts.push(blocked(vec![], Hints::new(FIXED, (1300, 650))).0);
        layouts.push(blocked_rig().0);
        let wild = [i32::MIN, -5000, 0, 5000, i32::MAX];
        let mut checked = 0;
        for mut l in layouts {
            assert_floats_contained(&l);
            let floats: Vec<WinId> = l
                .tiles
                .iter()
                .filter(|t| t.floating && l.visible(t.id))
                .map(|t| t.id)
                .collect();
            for f in floats {
                for &x in &wild {
                    for &y in &wild {
                        let o = outer_of(&l, f);
                        let hits = [
                            (o.x + 30, o.y + 3),
                            (o.x - 2, o.y + o.h / 2),
                            (o.x + o.w + 2, o.y + o.h + 2),
                            (o.x + o.w / 2, o.y - 2),
                        ];
                        for p in hits {
                            if l.hit(p).is_some_and(|h| h.id() == f) {
                                float_drag(&mut l, p, &[(x / 2, y / 2), (x, y)]);
                                checked += 1;
                            }
                        }
                    }
                }
                for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
                    for op in [Nudge::Move, Nudge::Grow, Nudge::Shrink] {
                        for _ in 0..30 {
                            let _ = l.keyboard_adjust(f, side, op);
                            assert_floats_contained(&l);
                        }
                    }
                }
            }
        }
        assert!(checked > 100, "{checked}");
    }
}
