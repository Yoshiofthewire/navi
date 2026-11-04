//! Where the bar and menu draw, and what a click hits, in surface-local logical px. The surface is
//! anchored to the bottom edge only, so navi centres it on the output.

use crate::menu::ROWS;
use crate::theme::{ACCENT, BG, DIM, INK, SELECTED};

pub const BAR_H: i32 = 48;
/// A slot per icon: the button, then one per window.
pub const SLOT: i32 = 48;
/// The click target and hover square inside a slot.
const TARGET: i32 = 40;
pub const ICON: i32 = 32;
const PAD: i32 = 8;
const PANEL_W: i32 = 360;
const SEARCH_H: i32 = 40;
pub const ROW_H: i32 = 32;
pub const ROW_ICON: i32 = 24;
pub const PANEL_H: i32 = PAD + SEARCH_H + ROWS as i32 * ROW_H + PAD;

pub const MINIMIZED_ALPHA: f32 = 0.45;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Button,
    Window(usize),
    /// A shown menu row, 0 at the top.
    Row(usize),
}

/// A window's slot as drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Slot {
    pub focused: bool,
    pub minimized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconOf {
    Button,
    Window(usize),
    /// An entry, by index.
    Entry(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Fill(Rect, u32),
    Icon(IconOf, Rect, f32),
    /// Text, left-aligned and vertically centred in the rect, truncated to its width.
    Text(String, Rect, u32),
    /// The search line: the query with a caret after it, or a caret and a placeholder.
    Field(String, Rect),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub w: i32,
    pub h: i32,
    windows: usize,
    open: bool,
}

impl Layout {
    pub fn new(windows: usize, open: bool) -> Layout {
        let bar_w = Self::bar_w(windows);
        let (w, h) = match open {
            true => (bar_w.max(PANEL_W), BAR_H + PANEL_H),
            false => (bar_w, BAR_H),
        };
        Layout {
            w,
            h,
            windows,
            open,
        }
    }

    /// The button and window slots, plus padding.
    pub fn bar_w(windows: usize) -> i32 {
        (windows as i32 + 1) * SLOT + 2 * PAD
    }

    /// The bar row, centred at the bottom.
    pub fn bar(&self) -> Rect {
        let w = Self::bar_w(self.windows);
        Rect {
            x: (self.w - w) / 2,
            y: self.h - BAR_H,
            w,
            h: BAR_H,
        }
    }

    /// Slot 0 is the button; window `i` is slot `i + 1`. The click target, centred in the slot.
    fn target(&self, slot: usize) -> Rect {
        let b = self.bar();
        let inset = (SLOT - TARGET) / 2;
        Rect {
            x: b.x + PAD + slot as i32 * SLOT + inset,
            y: b.y + inset,
            w: TARGET,
            h: TARGET,
        }
    }

    fn panel(&self) -> Option<Rect> {
        self.open.then_some(Rect {
            x: (self.w - PANEL_W) / 2,
            y: 0,
            w: PANEL_W,
            h: PANEL_H,
        })
    }

    fn search(&self, p: Rect) -> Rect {
        Rect {
            x: p.x + PAD,
            y: p.y + PAD,
            w: p.w - 2 * PAD,
            h: SEARCH_H,
        }
    }

    fn row(&self, p: Rect, k: usize) -> Rect {
        Rect {
            x: p.x + PAD,
            y: p.y + PAD + SEARCH_H + k as i32 * ROW_H,
            w: p.w - 2 * PAD,
            h: ROW_H,
        }
    }

    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if let Some(p) = self.panel() {
            return (0..ROWS)
                .find(|&k| self.row(p, k).contains(x, y))
                .map(Hit::Row)
                .or_else(|| self.hit_bar(x, y));
        }
        self.hit_bar(x, y)
    }

    fn hit_bar(&self, x: i32, y: i32) -> Option<Hit> {
        (0..=self.windows)
            .find(|&s| self.target(s).contains(x, y))
            .map(|s| match s {
                0 => Hit::Button,
                s => Hit::Window(s - 1),
            })
    }

    /// What to draw, back to front. `rows` are the menu's shown rows (entry, selected, name);
    /// `scroll` is the first shown match and the number of matches.
    pub fn draw(
        &self,
        slots: &[Slot],
        query: &str,
        rows: &[(usize, bool, &str)],
        (top, total): (usize, usize),
    ) -> Vec<Item> {
        let mut out = Vec::new();
        if let Some(p) = self.panel() {
            out.push(Item::Fill(p, BG));
            out.extend(border(p));
            let s = self.search(p);
            let line = Rect {
                y: s.y + s.h - 2,
                h: 1,
                ..s
            };
            out.push(Item::Fill(line, ACCENT));
            out.push(Item::Field(query.into(), inset_x(s, 8)));
            if rows.is_empty() && !query.is_empty() {
                let r = inset_x(self.row(p, 0), 8);
                out.push(Item::Text("No matching apps".into(), r, DIM));
            }
            if total > ROWS {
                // A scrollbar in the gutter right of the rows.
                let track = Rect {
                    x: p.x + p.w - 6,
                    y: self.row(p, 0).y,
                    w: 3,
                    h: ROWS as i32 * ROW_H,
                };
                let (top, total) = (top as i32, total as i32);
                let thumb = Rect {
                    y: track.y + track.h * top / total,
                    h: (track.h * ROWS as i32 / total).max(8),
                    ..track
                };
                out.push(Item::Fill(thumb, DIM));
            }
            for (k, &(entry, selected, name)) in rows.iter().enumerate() {
                let r = self.row(p, k);
                if selected {
                    out.push(Item::Fill(r, SELECTED));
                    out.push(Item::Fill(Rect { w: 3, ..r }, ACCENT));
                }
                let icon = square(r.x + 8, r, ROW_ICON);
                out.push(Item::Icon(IconOf::Entry(entry), icon, 1.0));
                let text = Rect {
                    x: icon.x + ROW_ICON + 10,
                    w: r.x + r.w - (icon.x + ROW_ICON + 10) - 8,
                    ..r
                };
                out.push(Item::Text(name.into(), text, INK));
            }
        }
        let b = self.bar();
        out.push(Item::Fill(b, BG));
        out.push(Item::Fill(Rect { h: 1, ..b }, ACCENT));
        let button = self.target(0);
        out.push(Item::Icon(IconOf::Button, centred(button, ICON), 1.0));
        for (i, s) in slots.iter().enumerate().take(self.windows) {
            let t = self.target(i + 1);
            let alpha = if s.minimized { MINIMIZED_ALPHA } else { 1.0 };
            out.push(Item::Icon(IconOf::Window(i), centred(t, ICON), alpha));
            if s.focused {
                let line = Rect {
                    x: t.x + 4,
                    y: t.y + t.h + 1,
                    w: t.w - 8,
                    h: 3,
                };
                out.push(Item::Fill(line, ACCENT));
            }
        }
        out
    }
}

fn centred(r: Rect, size: i32) -> Rect {
    square(r.x + (r.w - size) / 2, r, size)
}

/// A `size` square at `x`, centred vertically in `r`.
fn square(x: i32, r: Rect, size: i32) -> Rect {
    Rect {
        x,
        y: r.y + (r.h - size) / 2,
        w: size,
        h: size,
    }
}

fn inset_x(r: Rect, by: i32) -> Rect {
    Rect {
        x: r.x + by,
        w: r.w - 2 * by,
        ..r
    }
}

fn border(r: Rect) -> [Item; 4] {
    let line = |x, y, w, h| Item::Fill(Rect { x, y, w, h }, ACCENT);
    [
        line(r.x, r.y, r.w, 1),
        line(r.x, r.y + r.h - 1, r.w, 1),
        line(r.x, r.y, 1, r.h),
        line(r.x + r.w - 1, r.y, 1, r.h),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::Entry;
    use crate::menu::Menu;

    #[test]
    fn ac7_bar_size_and_hits() {
        let l = Layout::new(0, false);
        assert_eq!((l.w, l.h), (SLOT + 2 * PAD, BAR_H));
        let l = Layout::new(3, false);
        assert_eq!((l.w, l.h), (4 * SLOT + 2 * PAD, BAR_H));
        // navi centres the bottom-anchored surface: on a 1920 px output it starts here.
        assert_eq!((1920 - l.w) / 2, 856);
        assert_eq!(
            l.bar(),
            Rect {
                x: 0,
                y: 0,
                w: 208,
                h: 48
            }
        );

        // Slot k spans 8 + 48k .. 8 + 48(k+1); its target is the inner 40 px.
        assert_eq!(l.hit(12, 4), Some(Hit::Button));
        assert_eq!(l.hit(51, 43), Some(Hit::Button));
        for i in 0..3 {
            let x = 8 + 48 * (i as i32 + 1) + 24;
            assert_eq!(l.hit(x, 24), Some(Hit::Window(i)), "slot {i}");
        }
        for (x, y) in [
            (0, 24),
            (7, 24),
            (11, 24),
            (52, 24),
            (55, 24),
            (30, 3),
            (30, 44),
            (199, 24),
            (208, 24),
            (-1, 0),
            (30, 48),
        ] {
            assert_eq!(l.hit(x, y), None, "gap at {x},{y}");
        }
    }

    #[test]
    fn ac7_draw_list() {
        let l = Layout::new(3, false);
        let slots = [
            Slot {
                focused: false,
                minimized: false,
            },
            Slot {
                focused: true,
                minimized: false,
            },
            Slot {
                focused: false,
                minimized: true,
            },
        ];
        let items = l.draw(&slots, "", &[], (0, 0));
        let icon = |i: usize| {
            items.iter().find_map(|it| match it {
                Item::Icon(IconOf::Window(w), r, a) if *w == i => Some((*r, *a)),
                _ => None,
            })
        };
        assert_eq!(icon(0).unwrap().1, 1.0);
        assert_eq!(icon(2).unwrap().1, MINIMIZED_ALPHA, "minimized is dimmed");
        let underlines: Vec<Rect> = items
            .iter()
            .filter_map(|it| match it {
                Item::Fill(r, ACCENT) if r.h == 3 => Some(*r),
                _ => None,
            })
            .collect();
        let under = icon(1).unwrap().0;
        assert_eq!(underlines.len(), 1, "only the focused slot");
        assert!(underlines[0].x >= under.x - 4 && underlines[0].y > under.y + ICON);
        assert!(items.contains(&Item::Icon(
            IconOf::Button,
            Rect {
                x: 16,
                y: 8,
                w: 32,
                h: 32
            },
            1.0
        )));
    }

    #[test]
    fn ac7_menu_rows_hit_after_scrolling() {
        let entries: Vec<Entry> = (0..20)
            .map(|k| Entry {
                id: format!("app{k:02}.desktop"),
                name: format!("App {k:02}"),
                icon: None,
                argv: vec!["x".into()],
                wm_class: None,
            })
            .collect();
        let mut m = Menu::new(&entries);
        m.scroll(3);
        let l = Layout::new(1, true);
        assert_eq!((l.w, l.h), (PANEL_W, BAR_H + PANEL_H));
        // The bar row sits centred below the panel.
        assert_eq!(
            l.bar(),
            Rect {
                x: (360 - 112) / 2,
                y: PANEL_H,
                w: 112,
                h: 48
            }
        );
        assert_eq!(l.hit(l.bar().x + 12, PANEL_H + 10), Some(Hit::Button));
        let y = |k: i32| PAD + SEARCH_H + k * ROW_H + 5;
        assert_eq!(l.hit(100, y(0)), Some(Hit::Row(0)));
        let Some(Hit::Row(k)) = l.hit(100, y(2)) else {
            panic!()
        };
        assert_eq!(m.at_row(k), Some(5));
        assert_eq!(l.hit(100, y(10)), None, "below the last row");
        assert_eq!(l.hit(100, PAD + 5), None, "the search line");
        let rows: Vec<(usize, bool, &str)> = m
            .rows()
            .map(|(e, s)| (e, s, entries[e].name.as_str()))
            .collect();
        let items = l.draw(&[Slot::default()], "kon", &rows, m.scroll_pos());
        assert!(items.contains(&Item::Field(
            "kon".into(),
            Rect {
                x: PAD + 8,
                y: PAD,
                w: PANEL_W - 2 * PAD - 16,
                h: SEARCH_H
            }
        )));
        assert!(
            items
                .iter()
                .any(|it| matches!(it, Item::Icon(IconOf::Entry(3), _, _)))
        );

        // 20 matches from the 4th: a scrollbar thumb half the track, a quarter... down.
        let track_y = PAD + SEARCH_H;
        let thumb = Rect {
            x: PANEL_W - 6,
            y: track_y + 320 * 3 / 20,
            w: 3,
            h: 160,
        };
        assert!(items.contains(&Item::Fill(thumb, DIM)));
        let few = l.draw(&[Slot::default()], "", &rows[..2], (0, 2));
        assert!(
            !few.iter()
                .any(|it| matches!(it, Item::Fill(r, DIM) if r.w == 3)),
            "no bar"
        );
        assert!(
            !few.iter()
                .any(|it| matches!(it, Item::Text(t, ..) if t == "No matching apps"))
        );

        let none = l.draw(&[Slot::default()], "zzz", &[], (0, 0));
        let r = Rect {
            x: PAD + 8,
            y: track_y,
            w: PANEL_W - 2 * PAD - 16,
            h: ROW_H,
        };
        assert!(none.contains(&Item::Text("No matching apps".into(), r, DIM)));
    }
}
