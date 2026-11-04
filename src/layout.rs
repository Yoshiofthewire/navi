//! The grid layout from docs/design.md. Pure geometry, no Wayland types.

use std::collections::HashMap;

mod bar;
mod drag;
pub use bar::{Decor, decoration};
pub use drag::{Action, Done, Drag, overlay};

pub const COLS: i32 = 12;
pub const ROWS: i32 = 6;
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
    /// Logical px in the global coordinate space.
    pub area: Rect,
    /// Lower is preferred.
    pub priority: u32,
}

impl Output {
    fn cell_w(&self) -> i32 {
        (self.area.w / COLS).max(1)
    }

    fn cell_h(&self) -> i32 {
        (self.area.h / ROWS).max(1)
    }

    // The last column and row absorb the rounding remainder.
    fn x_edge(&self, col: i32) -> i32 {
        match col {
            COLS => self.area.x + self.area.w,
            _ => self.area.x + col * self.cell_w(),
        }
    }

    fn y_edge(&self, row: i32) -> i32 {
        match row {
            ROWS => self.area.y + self.area.h,
            _ => self.area.y + row * self.cell_h(),
        }
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
    /// When it was last placed, moved or activated; a group shows its most recent member.
    pub stamp: u64,
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
    pub fn set_outputs(&mut self, mut outputs: Vec<Output>) {
        outputs.sort_by_key(|o| o.priority);
        self.outputs = outputs;
    }

    pub fn tiles(&self) -> &[Tile] {
        &self.tiles
    }

    pub fn tile(&self, id: WinId) -> Option<&Tile> {
        self.tiles.iter().find(|t| t.id == id)
    }

    /// The window's rect in global logical px.
    pub fn rect(&self, id: WinId) -> Option<Rect> {
        Some(self.frame(id)?.1)
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

    /// Another group on its output is maximized over it.
    fn covered(&self, t: &Tile) -> bool {
        self.tiles
            .iter()
            .any(|u| u.maximized && u.output == t.output && u.cells != t.cells)
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

    fn clear_maximized(&mut self, output: &str) {
        for t in self.tiles.iter_mut().filter(|t| t.output == output) {
            t.maximized = false;
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

    /// Hides `id`, keeping its cells, and ends its group's maximize.
    pub fn minimize(&mut self, id: WinId) -> bool {
        if !self.visible(id) {
            return false;
        }
        self.set_maximized(id, false);
        if let Some(t) = self.tile_mut(id) {
            t.minimized = true;
        }
        true
    }

    /// Shows `id` in its group, restoring it if minimized.
    pub fn activate(&mut self, id: WinId) -> bool {
        let Some(t) = self.tile(id) else {
            return false;
        };
        let changed = t.minimized || self.active(id) != Some(id);
        if changed {
            self.touch(id);
            if let Some(t) = self.tile_mut(id) {
                t.minimized = false;
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
        };
        match step {
            // The cells belong to another tile; only learn the size.
            Step::Tabbed(_) => {
                self.memory.entry(app.to_string()).or_insert(fresh);
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

    #[test]
    fn cell_sizes_on_example_monitors() {
        let uw = ultrawide();
        assert_eq!(
            uw.cells_rect(cells(0, 0, 1, 1)),
            Rect {
                x: 0,
                y: 0,
                w: 286,
                h: 240
            }
        );
        // The remainder (3440 - 12 * 286 = 8 px) goes to the last column.
        assert_eq!(
            uw.cells_rect(cells(11, 5, 1, 1)),
            Rect {
                x: 3146,
                y: 1200,
                w: 294,
                h: 240
            }
        );

        let k = uhd();
        assert_eq!(
            k.cells_rect(cells(0, 0, 1, 1)),
            Rect {
                x: 0,
                y: 0,
                w: 320,
                h: 360
            }
        );
        assert_eq!(
            k.cells_rect(cells(11, 5, 1, 1)),
            Rect {
                x: 3520,
                y: 1800,
                w: 320,
                h: 360
            }
        );

        let lp = laptop();
        assert_eq!(
            lp.cells_rect(cells(0, 0, 1, 1)),
            Rect {
                x: 0,
                y: 0,
                w: 93,
                h: 117
            }
        );
        assert_eq!(
            lp.cells_rect(cells(11, 5, 1, 1)),
            Rect {
                x: 1023,
                y: 585,
                w: 102,
                h: 118
            }
        );
    }

    #[test]
    fn whole_grid_covers_output_exactly() {
        for o in [
            ultrawide(),
            uhd(),
            laptop(),
            out("odd", 7, 11, 1001, 599, 0),
        ] {
            assert_eq!(o.cells_rect(cells(0, 0, COLS, ROWS)), o.area);
        }
    }

    #[test]
    fn common_splits_are_exact() {
        let uw = ultrawide();
        // T3 at 8 columns, browser at 4.
        assert_eq!(uw.cells_rect(cells(0, 0, 8, 6)).w, 2288);
        assert_eq!(uw.cells_rect(cells(8, 0, 4, 6)).w, 1152);
        // Three and four wide.
        assert_eq!(uw.cells_rect(cells(4, 0, 4, 6)).w, 1144);
        assert_eq!(uw.cells_rect(cells(3, 0, 3, 6)).w, 858);

        // The 4K as 3x3, and a video in its top 2x2.
        let k = uhd();
        assert_eq!(
            k.cells_rect(cells(4, 2, 4, 2)),
            Rect {
                x: 1280,
                y: 720,
                w: 1280,
                h: 720
            }
        );
        assert_eq!(
            k.cells_rect(cells(0, 0, 8, 4)),
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
            k.tile_rect(cells(0, 0, 4, 2)),
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
        assert_eq!(uhd().span_for((900, 700)), (3, 2));
        assert_eq!(ultrawide().span_for((900, 700)), (3, 3));
        // 320x240 needs 4x3 laptop cells (93x117 each).
        assert_eq!(laptop().span_for((10, 10)), (4, 3));
        assert_eq!(laptop().span_for((99_999, 99_999)), (12, 6));
        // An output too small for the minimum gets the whole grid, not a panic.
        assert_eq!(out("tiny", 0, 0, 200, 100, 0).span_for((10, 10)), (12, 6));
    }

    #[test]
    fn unknown_app_gets_half_width_full_height_near_pointer() {
        let mut l = layout(vec![ultrawide()]);
        let (id, step) = l.place("browser", (3000, 700)).unwrap();
        assert_eq!(step, Step::Nearest);
        assert_eq!(cells_of(&l, id), cells(6, 0, 6, 6));

        let (id2, _) = l.place("term", (3000, 700)).unwrap();
        assert_eq!(cells_of(&l, id2), cells(0, 0, 6, 6));
    }

    #[test]
    fn app_returns_to_its_remembered_spot() {
        let mut l = layout(vec![ultrawide()]);
        let (id, _) = l.place("t3", (10, 10)).unwrap();
        assert_eq!(cells_of(&l, id), cells(0, 0, 6, 6));
        l.remove(id);

        // The pointer is on the far side, but the old spot is free.
        let (id, step) = l.place("t3", (3400, 10)).unwrap();
        assert_eq!(step, Step::Remembered);
        assert_eq!(cells_of(&l, id), cells(0, 0, 6, 6));
    }

    #[test]
    fn taken_spot_falls_back_to_nearest_at_remembered_size() {
        let mut l = layout(vec![ultrawide()]);
        let (a, _) = l.place("term", (10, 10)).unwrap();
        l.memory.get_mut("term").unwrap().size = (860, 720);
        l.remove(a);
        l.place("other", (10, 10)).unwrap(); // takes cols 0..6

        let (id, step) = l.place("term", (10, 10)).unwrap();
        assert_eq!(step, Step::Nearest);
        // 860x720 = 3x3 cells, as close to the pointer as the free space allows.
        assert_eq!(cells_of(&l, id), cells(6, 0, 3, 3));
    }

    #[test]
    fn closing_leaves_cells_empty() {
        let mut l = layout(vec![ultrawide()]);
        let (a, _) = l.place("a", (10, 10)).unwrap();
        let (b, _) = l.place("b", (3400, 10)).unwrap();
        l.remove(a);
        assert_eq!(l.tiles().len(), 1);
        assert_eq!(cells_of(&l, b), cells(6, 0, 6, 6));
    }

    #[test]
    fn shrinks_tiles_wider_than_they_want() {
        let mut l = layout(vec![ultrawide()]);
        let (browser, _) = l.place("browser", (10, 10)).unwrap();
        let (term, _) = l.place("term", (3400, 10)).unwrap();
        // The browser now wants only 3 columns, but still has 6.
        l.memory.get_mut("browser").unwrap().size = (860, 1440);
        l.memory.insert(
            "editor".into(),
            Memory {
                output: "gone".into(),
                cells: cells(0, 0, 3, 6),
                size: (860, 1440),
            },
        );

        let (editor, step) = l.place("editor", (10, 700)).unwrap();
        assert_eq!(step, Step::Shrunk);
        assert_eq!(cells_of(&l, browser), cells(3, 0, 3, 6));
        assert_eq!(cells_of(&l, editor), cells(0, 0, 3, 6));
        assert_eq!(cells_of(&l, term), cells(6, 0, 6, 6));
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
                    cells(slot as i32 * 4, 0, 4, 6),
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
            assert_eq!(cells_of(&l, id), cells(slot as i32 * 3, 0, 3, 6));
        }
    }

    #[test]
    fn tabs_only_when_every_tile_is_at_minimum() {
        let mut l = layout(vec![ultrawide()]);
        let mut order = Vec::new();
        for n in 1..=6 {
            let (id, step) = l.place(&format!("app{n}"), (3400, 10)).unwrap();
            if n >= 3 {
                assert_eq!(step, Step::Shrunk, "window {n}");
            }
            // The 1st lands on the right, the 2nd left of it, the rest at the right end.
            match n {
                2 => order.insert(0, id),
                _ => order.push(id),
            }
            assert_row(&l, &order, 2);
        }
        assert!(order.iter().all(|&id| cells_of(&l, id).cols == 2));
        let (_, step) = l.place("app7", (3400, 10)).unwrap();
        assert!(matches!(step, Step::Tabbed(_)));
    }

    #[test]
    fn laptop_minimum_stops_at_thirds_and_failure_moves_nothing() {
        // AC6 and AC11: 4 laptop columns are the minimum.
        let mut l = layout(vec![laptop()]);
        let (a, _) = l.place("a", (10, 10)).unwrap();
        let (b, _) = l.place("b", (1100, 10)).unwrap();
        let (c, step) = l.place("c", (1100, 10)).unwrap();
        assert_eq!(step, Step::Shrunk);
        for (slot, id) in [a, b, c].into_iter().enumerate() {
            assert_eq!(cells_of(&l, id), cells(slot as i32 * 4, 0, 4, 6));
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

        for n in 4..=6 {
            let (_, step) = l.place(&format!("app{n}"), (100, 100)).unwrap();
            assert_eq!(step, Step::Shrunk);
        }
        let (g, step) = l.place("app7", (100, 100)).unwrap();
        assert_eq!(step, Step::OtherOutput);
        let t = l.tile(g).unwrap();
        assert_eq!(t.output, "4k");
        // Half the ultrawide (1720x1440 px) is 5x4 4K cells, as near the centre as they go.
        assert_eq!(t.cells, cells(3, 1, 5, 4));
    }

    #[test]
    fn opens_smaller_in_a_gap_before_shrinking() {
        let mut l = layout(vec![ultrawide()]);
        let (a, _) = l.place("a", (10, 10)).unwrap();
        remember(&mut l, "b", (1144, 1440));
        let (b, _) = l.place("b", (3400, 10)).unwrap();
        assert_eq!(cells_of(&l, a), cells(0, 0, 6, 6));
        assert_eq!(cells_of(&l, b), cells(8, 0, 4, 6));

        let (c, step) = l.place("c", (3400, 10)).unwrap();
        assert_eq!(step, Step::Smaller);
        assert_eq!(cells_of(&l, c), cells(6, 0, 2, 6));
        assert_eq!(cells_of(&l, a), cells(0, 0, 6, 6));
        assert_eq!(cells_of(&l, b), cells(8, 0, 4, 6));
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
        assert_eq!(cells_of(&l, b), cells(7, 0, 5, 6));

        let (c, step) = l.place("c", (3400, 10)).unwrap();
        assert_eq!(step, Step::Shrunk);
        assert_eq!(cells_of(&l, c).cols, 4);
        assert_row(&l, &[a, b, c], 2);
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
    fn no_outputs_places_nothing() {
        assert_eq!(Layout::default().place("a", (0, 0)), None);
    }
}
