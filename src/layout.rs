//! The grid layout from docs/design.md. Pure geometry, no Wayland types.

use std::collections::HashMap;

pub const COLS: i32 = 12;
pub const ROWS: i32 = 6;
/// Space between tiles in logical px. Tiles are inset by half of it, and by all of it at the top,
/// where the grab strip will go.
pub const GAP: i32 = 8;
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        let r = self.cells_rect(c);
        Rect {
            x: r.x + GAP / 2,
            y: r.y + GAP,
            w: (r.w - GAP).max(1),
            h: (r.h - GAP - GAP / 2).max(1),
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

#[derive(Clone, Debug)]
pub struct Tile {
    pub id: WinId,
    pub app: String,
    pub output: String,
    pub cells: Cells,
}

/// What navi remembers per app ID: where it was, and the size it wants, in px.
#[derive(Clone, Debug)]
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
        let t = self.tile(id)?;
        Some(self.output(&t.output)?.tile_rect(t.cells))
    }

    /// Closing leaves the cells empty; nothing else moves.
    pub fn remove(&mut self, id: WinId) {
        self.tiles.retain(|t| t.id != id);
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
        self.tiles.push(Tile {
            id,
            app: app.to_string(),
            output: output.clone(),
            cells,
        });
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
        (0..=ROWS - rows)
            .flat_map(|row| {
                (0..=COLS - cols).map(move |col| Cells {
                    col,
                    row,
                    cols,
                    rows,
                })
            })
            .filter(|&c| self.is_free(&o.name, c))
            .min_by_key(|&c| dist2(o.cells_rect(c).center(), pointer))
    }

    /// Takes columns from tiles wider than their remembered size, largest first, until `size`
    /// fits. Leaves every tile untouched if it never fits.
    fn shrink_for(&mut self, o: &Output, size: (i32, i32), pointer: (i32, i32)) -> Option<Cells> {
        let saved = self.tiles.clone();
        loop {
            if let Some(c) = self.nearest_free(o, size, pointer) {
                return Some(c);
            }
            let memory = &self.memory;
            let Some(victim) = self
                .tiles
                .iter_mut()
                .filter(|t| t.output == o.name)
                .filter(|t| {
                    let want = memory.get(&t.app).map_or(COLS, |m| o.span_for(m.size).0);
                    t.cells.cols > want
                })
                .max_by_key(|t| t.cells.cols * t.cells.rows)
            else {
                self.tiles = saved;
                return None;
            };
            // Free the column on the side nearer the pointer.
            if pointer.0 < o.cells_rect(victim.cells).center().0 {
                victim.cells.col += 1;
            }
            victim.cells.cols -= 1;
        }
    }
}

fn dist2((ax, ay): (i32, i32), (bx, by): (i32, i32)) -> i64 {
    let (dx, dy) = ((ax - bx) as i64, (ay - by) as i64);
    dx * dx + dy * dy
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

    #[test]
    fn full_output_with_nothing_to_shrink_tabs_and_moves_nothing() {
        let mut l = layout(vec![ultrawide()]);
        let (a, _) = l.place("a", (10, 10)).unwrap();
        let (b, _) = l.place("b", (3400, 10)).unwrap();

        let (c, step) = l.place("c", (3000, 10)).unwrap();
        assert_eq!(step, Step::Tabbed(b));
        assert_eq!(cells_of(&l, c), cells_of(&l, b));
        assert_eq!(cells_of(&l, a), cells(0, 0, 6, 6));
        assert_eq!(cells_of(&l, b), cells(6, 0, 6, 6));
    }

    #[test]
    fn full_output_overflows_to_next_output_by_priority() {
        let mut l = layout(vec![
            out("laptop", -1125, 0, 1125, 703, 2),
            out("4k", 0, -2160, 3840, 2160, 1),
            out("uw", 0, 0, 3440, 1440, 0),
        ]);
        l.place("a", (10, 10)).unwrap();
        l.place("b", (3400, 10)).unwrap();

        let (c, step) = l.place("c", (100, 100)).unwrap();
        assert_eq!(step, Step::OtherOutput);
        let t = l.tile(c).unwrap();
        assert_eq!(t.output, "4k");
        // Half the ultrawide (1720x1440 px) is 5x4 4K cells, as near the centre as they go.
        assert_eq!(t.cells, cells(3, 1, 5, 4));
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
