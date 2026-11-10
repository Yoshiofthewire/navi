//! The grid layout from docs/design.md. Pure geometry, no Wayland types.

use std::collections::HashMap;

mod bar;
mod drag;
mod memory;
pub use bar::{Decor, decoration};
pub use drag::{Action, Done, Drag, EdgeClicks, Side, overlay};
pub use memory::safe;

pub const COLS: i32 = 24;
pub const ROWS: i32 = 12;
/// Space between tiles in logical px. Tiles are inset by half of it, and by all of it at the top,
/// where the grab strip will go.
pub const GAP: i32 = 8;
/// A tab bar's height, which replaces the strip's `GAP` above tabbed and server-decorated tiles.
/// 24 px is WCAG 2.5.8's minimum target size.
pub const BAR: i32 = 24;
pub const MIN_W: i32 = 320;
pub const MIN_H: i32 = 240;

pub type WinId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    fn contains(&self, (x, y): (i32, i32)) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
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
        self.window_rect(c, GAP)
    }

    /// `tile_rect` with `top` px above the window instead of `GAP`.
    fn window_rect(&self, c: Cells, top: i32) -> Rect {
        let r = self.cells_rect(c);
        Rect {
            x: r.x + GAP / 2,
            y: r.y + top,
            w: (r.w - GAP).max(1),
            h: (r.h - top - GAP / 2).max(1),
        }
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
}

impl Tile {
    /// Same output and cells: tabbed together.
    fn tabbed(&self, o: &Tile) -> bool {
        self.output == o.output && self.cells == o.cells
    }
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
                t.maximized = false;
                t.fullscreen = false;
                back.push(id);
            }
        }
    }

    /// Moves `name`'s tiles, a tab group at a time, to the other outputs with the placement steps
    /// from the main output's centre, and forgets `name`. Memory doesn't change.
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
            let Some(t) = self.tile(id).cloned() else {
                continue;
            };
            let size = self.output_size(&t);
            let (output, cells, _) = self.find_spot(&main, size, main.area.center(), None);
            for t in self.tiles.iter_mut().filter(|t| group.contains(&t.id)) {
                t.home = t.home.take().or(Some((t.output.clone(), t.cells)));
                t.output = output.clone();
                t.cells = cells;
                t.maximized = false;
                t.fullscreen = false;
            }
        }
    }

    /// The px size `t` wants: its memory, else half an output's width.
    fn output_size(&self, t: &Tile) -> (i32, i32) {
        let half = self
            .outputs
            .first()
            .map_or((MIN_W, MIN_H), |o| (o.area.w / 2, o.area.h));
        self.memory.get(&t.app).map_or(half, |m| m.size)
    }

    pub fn tiles(&self) -> &[Tile] {
        &self.tiles
    }

    pub fn tile(&self, id: WinId) -> Option<&Tile> {
        self.tiles.iter().find(|t| t.id == id)
    }

    /// The window's rect in global logical px.
    pub fn rect(&self, id: WinId) -> Option<Rect> {
        let t = self.tile(id)?;
        match t.fullscreen {
            true => Some(self.output(&t.output)?.full),
            false => Some(self.frame(id)?.1),
        }
    }

    /// The px of `id`'s cells (the whole grid while maximized) and of its window, which sits below
    /// a bar if it has one.
    fn frame(&self, id: WinId) -> Option<(Rect, Rect)> {
        let t = self.tile(id)?;
        let o = self.output(&t.output)?;
        let cells = match self.maximized(t) {
            true => FULL,
            false => t.cells,
        };
        let top = if self.has_bar(id) { BAR } else { GAP };
        Some((o.cells_rect(cells), o.window_rect(cells, top)))
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

    /// Another group on its output is maximized over it, or another window is fullscreen there.
    fn covered(&self, t: &Tile) -> bool {
        self.tiles.iter().any(|u| {
            u.output == t.output
                && ((u.maximized && u.cells != t.cells) || (u.fullscreen && u.id != t.id))
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

    /// Tabbed, or showing a window that wants navi's controls.
    pub fn has_bar(&self, id: WinId) -> bool {
        let decorated = self
            .active(id)
            .and_then(|a| self.tile(a))
            .is_some_and(|a| a.decorated);
        decorated || self.group(id).len() > 1
    }

    pub fn set_decorated(&mut self, id: WinId, on: bool) {
        if let Some(t) = self.tile_mut(id) {
            t.decorated = on;
        }
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
        if self.maximized(t) {
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
            let other_group = u.cells != t.cells;
            if (u.maximized && other_group) || (u.fullscreen && u.id != id) {
                u.maximized = u.maximized && !other_group;
                u.fullscreen = false;
                changed = true;
            }
        }
        changed
    }

    /// Places a new window of `app`, following the order in docs/design.md.
    /// `None` only when there are no outputs.
    pub fn place(&mut self, app: &str, pointer: (i32, i32)) -> Option<(WinId, Step)> {
        let here = self
            .outputs
            .iter()
            .find(|o| o.area.contains(pointer))
            .or(self.outputs.first())?
            .clone();
        let mem = self.memory.get(app).cloned();
        let size = mem
            .as_ref()
            .map_or((here.area.w / 2, here.area.h), |m| m.size);

        let (output, cells, step) = self.find_spot(&here, size, pointer, mem.as_ref());

        let id = self.next_id;
        self.next_id += 1;
        // A new window must be visible (principle 6).
        self.clear_maximized(&output);
        self.tiles.push(Tile {
            id,
            app: app.to_string(),
            output: output.clone(),
            cells,
            ..Tile::default()
        });
        self.touch(id);
        let fresh = Memory {
            output,
            cells,
            size,
            stamp: self.clock,
        };
        match step {
            // The cells belong to another tile; only learn the size.
            Step::Tabbed(_) => {
                self.memory.entry(app.to_string()).or_insert(fresh).stamp = self.clock;
            }
            _ => {
                self.memory.insert(app.to_string(), fresh);
            }
        }
        Some((id, step))
    }

    fn find_spot(
        &mut self,
        here: &Output,
        size: (i32, i32),
        pointer: (i32, i32),
        mem: Option<&Memory>,
    ) -> (String, Cells, Step) {
        if let Some(m) = mem
            && self.output(&m.output).is_some()
            && self.is_free(&m.output, m.cells)
        {
            return (m.output.clone(), m.cells, Step::Remembered);
        }
        if let Some(c) = self.nearest_free(here, size, pointer) {
            return (here.name.clone(), c, Step::Nearest);
        }
        if let Some(c) = self.smaller_free(here, size, pointer) {
            return (here.name.clone(), c, Step::Smaller);
        }
        if let Some(c) = self.shrink_for(here, size, pointer) {
            return (here.name.clone(), c, Step::Shrunk);
        }
        let others: Vec<Output> = self
            .outputs
            .iter()
            .filter(|o| o.name != here.name)
            .cloned()
            .collect();
        for o in &others {
            if let Some(c) = self.nearest_free(o, size, o.area.center()) {
                return (o.name.clone(), c, Step::OtherOutput);
            }
        }
        let dist = |c: Cells| dist2(here.cells_rect(c).center(), pointer);
        match self
            .tiles
            .iter()
            .filter(|t| t.output == here.name)
            .min_by_key(|t| dist(t.cells))
        {
            Some(t) => (here.name.clone(), t.cells, Step::Tabbed(t.id)),
            // Unreachable: an empty output always has room.
            None => {
                let (cols, rows) = here.span_for(size);
                let c = Cells {
                    col: 0,
                    row: 0,
                    cols,
                    rows,
                };
                (here.name.clone(), c, Step::Nearest)
            }
        }
    }

    fn output(&self, name: &str) -> Option<&Output> {
        self.outputs.iter().find(|o| o.name == name)
    }

    fn is_free(&self, output: &str, c: Cells) -> bool {
        !self
            .tiles
            .iter()
            .any(|t| t.output == output && t.cells.overlaps(&c))
    }

    /// The empty rect of `size` whose centre is closest to `pointer`.
    fn nearest_free(&self, o: &Output, size: (i32, i32), pointer: (i32, i32)) -> Option<Cells> {
        let (cols, rows) = o.span_for(size);
        spots(cols, rows)
            .filter(|&c| self.is_free(&o.name, c))
            .min_by_key(|&c| dist2(o.cells_rect(c).center(), pointer))
    }

    /// The largest empty rect between the minimum and `size`; ties go to the one nearest
    /// `pointer`.
    fn smaller_free(&self, o: &Output, size: (i32, i32), pointer: (i32, i32)) -> Option<Cells> {
        let (want_c, want_r) = o.span_for(size);
        let (min_c, min_r) = o.span_for((MIN_W, MIN_H));
        (min_r..=want_r)
            .flat_map(|rows| (min_c..=want_c).flat_map(move |cols| spots(cols, rows)))
            .filter(|&c| self.is_free(&o.name, c))
            .min_by_key(|&c| (-c.cols * c.rows, dist2(o.cells_rect(c).center(), pointer)))
    }

    /// Takes columns from the tiles in the new window's rows until it gets an equal share, then
    /// slides them apart, in order, to open one gap near `pointer`. Moves nothing on failure.
    fn shrink_for(&mut self, o: &Output, size: (i32, i32), pointer: (i32, i32)) -> Option<Cells> {
        let (want_c, rows) = o.span_for(size);
        let (min_c, _) = o.span_for((MIN_W, MIN_H));
        let band = (0..=ROWS - rows)
            .map(|row| Cells {
                col: 0,
                row,
                cols: COLS,
                rows,
            })
            .min_by_key(|&b| (o.cells_rect(b).center().1 - pointer.1).abs())?;
        let (mut row, others): (Vec<usize>, Vec<usize>) = (0..self.tiles.len())
            .filter(|&i| self.tiles[i].output == o.name)
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
        let mut widths: Vec<i32> = cells.iter().map(|c| c.cols).collect();
        while COLS - widths.iter().sum::<i32>() < target {
            let v = (0..n)
                .filter(|&k| widths[k] > min_c)
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

/// Squared distance; saturates instead of overflowing on wild pointer coordinates.
fn dist2((ax, ay): (i32, i32), (bx, by): (i32, i32)) -> u64 {
    let d = |a: i32, b: i32| u64::from(a.abs_diff(b)).pow(2);
    d(ax, bx).saturating_add(d(ay, by))
}

#[cfg(test)]
mod tests {
    use super::*;

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
                x: 4,
                y: 8,
                w: 1272,
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
                x: 4,
                y: -2152,
                w: 1912,
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
        let maximized = l.output("4k").unwrap().window_rect(FULL, GAP);
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
}
