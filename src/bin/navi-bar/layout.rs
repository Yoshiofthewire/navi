//! Dock and launcher geometry in logical pixels; transparent margins stay outside input regions.

use crate::{
    desktop::{Category, Entry},
    menu::{Menu, ROWS, Row},
    theme::{ACCENT, BAR_BG, DIM, INK, INK_ON_ACCENT, INSET, PANEL_EDGE, Panel, SELECTED},
};

/// 40 px of dock, floating `INSET` above the screen edge.
pub const BAR_H: i32 = DOCK_H + INSET;
pub const SLOT: i32 = 36;
const TARGET: i32 = 32;
pub const ICON: i32 = 24;
/// The attention dot on a dock icon whose app has a dialog waiting, in px.
const ATTENTION_DOT: i32 = 8;
const DOCK_PAD: i32 = 8;
const DOCK_H: i32 = 40;
const PAD: i32 = 12;
const PANEL_W: i32 = 420;
const HEADER_H: i32 = 32;
const SEARCH_H: i32 = 40;
const FOOTER_H: i32 = 32;
pub const ROW_H: i32 = 36;
pub const ROW_ICON: i32 = 24;
pub const PANEL_H: i32 = 2 * PAD + HEADER_H + SEARCH_H + 8 + ROWS as i32 * ROW_H + FOOTER_H;
const PANEL_GAP: i32 = INSET;
pub const MINIMIZED_ALPHA: f32 = 0.45;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}
impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Button,
    Window(usize),
    Row(usize),
    Pin(usize),
    Back,
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Slot {
    pub focused: bool,
    pub minimized: bool,
    /// A dialog of the app waits for the user: an accent dot at the icon's corner.
    pub attention: bool,
    pub running: usize,
    pub pinned: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconOf {
    Button,
    Window(usize),
    Entry(usize),
    Folder(Category),
    Session(&'static str),
}
impl IconOf {
    /// Folder and session glyphs are UI marks, drawn in INK so they hold 3:1 on any panel and
    /// scheme; app icons and the logo keep their colours.
    pub fn tint(self) -> Option<u32> {
        matches!(self, IconOf::Folder(_) | IconOf::Session(_)).then_some(INK)
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Fill(Rect, u32),
    Round(Rect, i32, u32),
    /// A floating panel: translucent `BAR_BG` with a rim of this colour and width.
    Panel(Rect, Panel, u32, i32),
    Icon(IconOf, Rect, f32),
    Text(String, Rect, u32),
    Field(String, Rect),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub w: i32,
    pub h: i32,
    windows: usize,
    open: bool,
    rows: usize,
}

impl Layout {
    pub fn new(windows: usize, open: bool) -> Self {
        let bar_w = Self::bar_w(windows);
        Self {
            w: if open { bar_w.max(PANEL_W) } else { bar_w },
            h: if open {
                BAR_H + PANEL_GAP + PANEL_H
            } else {
                BAR_H
            },
            windows,
            open,
            rows: ROWS,
        }
    }

    /// Short search results do not leave an empty, full-height launcher behind.
    pub fn with_rows(mut self, rows: usize) -> Self {
        if self.open {
            self.rows = rows.clamp(1, ROWS);
            self.h -= (ROWS - self.rows) as i32 * ROW_H;
        }
        self
    }
    pub fn bar_w(windows: usize) -> i32 {
        (windows as i32 + 1) * SLOT + 2 * DOCK_PAD
    }
    pub fn bar(&self) -> Rect {
        let w = Self::bar_w(self.windows);
        Rect {
            x: (self.w - w) / 2,
            y: self.h - BAR_H,
            w,
            h: DOCK_H,
        }
    }
    pub fn target(&self, slot: usize) -> Rect {
        let b = self.bar();
        Rect {
            x: b.x + DOCK_PAD + slot as i32 * SLOT + (SLOT - TARGET) / 2,
            y: b.y + (DOCK_H - TARGET) / 2,
            w: TARGET,
            h: TARGET,
        }
    }
    pub fn panel(&self) -> Option<Rect> {
        self.open.then_some(Rect {
            x: (self.w - PANEL_W) / 2,
            y: 0,
            w: PANEL_W,
            h: self.h - BAR_H - PANEL_GAP,
        })
    }
    fn header(&self, p: Rect) -> Rect {
        Rect {
            x: p.x + PAD,
            y: PAD,
            w: p.w - 2 * PAD,
            h: HEADER_H,
        }
    }
    fn search(&self, p: Rect) -> Rect {
        Rect {
            y: PAD + HEADER_H,
            h: SEARCH_H,
            ..self.header(p)
        }
    }
    pub fn row(&self, k: usize) -> Rect {
        let p = self.panel().unwrap_or(Rect {
            x: 0,
            y: 0,
            w: PANEL_W,
            h: PANEL_H,
        });
        Rect {
            x: p.x + PAD,
            y: PAD + HEADER_H + SEARCH_H + 8 + k as i32 * ROW_H,
            w: p.w - 2 * PAD,
            h: ROW_H,
        }
    }
    fn pin(&self, k: usize) -> Rect {
        let r = self.row(k);
        Rect {
            x: r.x + r.w - 60,
            y: r.y + 2,
            w: 56,
            h: ROW_H - 4,
        }
    }
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if let Some(p) = self.panel() {
            if (Rect {
                w: 52,
                ..self.header(p)
            })
            .contains(x, y)
            {
                return Some(Hit::Back);
            }
            for k in 0..self.rows {
                if self.pin(k).contains(x, y) {
                    return Some(Hit::Pin(k));
                }
                if self.row(k).contains(x, y) {
                    return Some(Hit::Row(k));
                }
            }
        }
        (0..=self.windows)
            .find(|&s| self.target(s).contains(x, y))
            .map(|s| {
                if s == 0 {
                    Hit::Button
                } else {
                    Hit::Window(s - 1)
                }
            })
    }
    pub fn draw(
        &self,
        slots: &[Slot],
        menu: Option<&Menu>,
        entries: &[Entry],
        pins: &[String],
        hover: Option<Hit>,
        message: &str,
    ) -> Vec<Item> {
        let mut out = Vec::new();
        if let (Some(p), Some(m)) = (self.panel(), menu) {
            out.push(Item::Panel(p, Panel::Popup, PANEL_EDGE, 1));
            let back = m.folder().is_some() || !m.query().is_empty();
            let mut title = self.header(p);
            if back {
                let r = Rect { w: 52, ..title };
                out.push(Item::Round(
                    r,
                    7,
                    if hover == Some(Hit::Back) {
                        SELECTED
                    } else {
                        BAR_BG
                    },
                ));
                out.push(Item::Text("Back".into(), inset_x(r, 8), INK));
                title.x += 64;
                title.w -= 64;
            }
            out.push(Item::Text(m.title().into(), title, INK));
            let search = self.search(p);
            // A rim gives the field its shape on any panel, high contrast included.
            out.push(Item::Round(search, 8, PANEL_EDGE));
            out.push(Item::Round(inset(search, 1), 7, SELECTED));
            out.push(Item::Field(m.query().into(), inset_x(search, 12)));
            if m.rows().next().is_none() {
                out.push(Item::Text(
                    if m.query().is_empty() {
                        "No applications installed"
                    } else {
                        "No matching apps"
                    }
                    .into(),
                    inset_x(self.row(0), 8),
                    DIM,
                ));
            }
            for (k, (row, selected)) in m.rows().enumerate() {
                let r = self.row(k);
                if selected || matches!(hover, Some(Hit::Row(i) | Hit::Pin(i)) if i == k) {
                    out.push(Item::Round(r, 8, SELECTED));
                }
                let icon = square(r.x + 8, r, ROW_ICON);
                let (name, tail, of) = match row {
                    Row::Folder(c) => {
                        let n = entries.iter().filter(|e| e.category == c).count();
                        (
                            c.name(),
                            format!("{n} {}", if n == 1 { "app" } else { "apps" }),
                            IconOf::Folder(c),
                        )
                    }
                    Row::App(e) => (entries[e].name.as_str(), String::new(), IconOf::Entry(e)),
                };
                out.push(Item::Icon(of, icon, 1.0));
                let text = Rect {
                    x: icon.x + ROW_ICON + 12,
                    w: r.w - ROW_ICON - 96,
                    ..r
                };
                out.push(Item::Text(name.into(), text, INK));
                let pin = self.pin(k);
                match row {
                    Row::Folder(_) => {
                        let highlighted =
                            selected || matches!(hover, Some(Hit::Row(i) | Hit::Pin(i)) if i == k);
                        out.push(Item::Text(tail, pin, if highlighted { INK } else { DIM }));
                    }
                    Row::App(e) => {
                        let pinned = pins.contains(&entries[e].id);
                        out.push(Item::Round(
                            pin,
                            6,
                            if hover == Some(Hit::Pin(k)) {
                                ACCENT
                            } else {
                                BAR_BG
                            },
                        ));
                        let ink = if hover == Some(Hit::Pin(k)) {
                            INK_ON_ACCENT
                        } else if pinned {
                            ACCENT
                        } else {
                            INK
                        };
                        out.push(Item::Text(
                            if pinned { "Unpin" } else { "Pin" }.into(),
                            inset_x(pin, 8),
                            ink,
                        ));
                    }
                }
            }
            let (top, total) = m.scroll_pos();
            if total > ROWS {
                let h = ROWS as i32 * ROW_H;
                let thumb = Rect {
                    x: p.x + p.w - 7,
                    y: self.row(0).y + h * top as i32 / total as i32,
                    w: 2,
                    h: (h * ROWS as i32 / total as i32).max(8),
                };
                out.push(Item::Fill(thumb, DIM));
            }
            let footer = Rect {
                x: p.x + PAD,
                y: p.h - PAD - FOOTER_H,
                w: p.w - 2 * PAD,
                h: FOOTER_H,
            };
            let hint = if message.is_empty() {
                "Pin apps here, or press Insert on a selected app."
            } else {
                message
            };
            out.push(Item::Text(hint.into(), footer, DIM));
        }
        let b = self.bar();
        out.push(Item::Panel(b, Panel::Bar, PANEL_EDGE, 1));
        if hover == Some(Hit::Button) || self.open {
            out.push(Item::Round(self.target(0), 8, SELECTED));
        }
        out.push(Item::Icon(
            IconOf::Button,
            centred(self.target(0), ICON),
            1.0,
        ));
        for (i, slot) in slots.iter().enumerate().take(self.windows) {
            let t = self.target(i + 1);
            if slot.focused || hover == Some(Hit::Window(i)) {
                out.push(Item::Round(t, 8, SELECTED));
            }
            let alpha = if slot.minimized { MINIMIZED_ALPHA } else { 1.0 };
            let icon = centred(t, ICON);
            out.push(Item::Icon(IconOf::Window(i), icon, alpha));
            if slot.attention {
                let dot = Rect {
                    x: icon.x + icon.w - ATTENTION_DOT / 2,
                    y: icon.y - ATTENTION_DOT / 2,
                    w: ATTENTION_DOT,
                    h: ATTENTION_DOT,
                };
                out.push(Item::Round(dot, ATTENTION_DOT / 2, ACCENT));
            }
            if slot.running > 0 {
                let count = slot.running.min(3) as i32;
                let start = t.x + (t.w - (count * 5 - 2)) / 2;
                for n in 0..count {
                    out.push(Item::Round(
                        Rect {
                            x: start + n * 5,
                            y: b.y + b.h - 5,
                            w: 3,
                            h: 2,
                        },
                        1,
                        if slot.focused { ACCENT } else { DIM },
                    ));
                }
            } else if slot.pinned {
                out.push(Item::Round(
                    Rect {
                        x: t.x + t.w / 2 - 1,
                        y: b.y + b.h - 5,
                        w: 2,
                        h: 2,
                    },
                    1,
                    PANEL_EDGE,
                ));
            }
        }
        out
    }
}
fn centred(r: Rect, size: i32) -> Rect {
    square(r.x + (r.w - size) / 2, r, size)
}
fn square(x: i32, r: Rect, size: i32) -> Rect {
    Rect {
        x,
        y: r.y + (r.h - size) / 2,
        w: size,
        h: size,
    }
}
fn inset(r: Rect, by: i32) -> Rect {
    Rect {
        x: r.x + by,
        y: r.y + by,
        w: r.w - 2 * by,
        h: r.h - 2 * by,
    }
}
fn inset_x(r: Rect, by: i32) -> Rect {
    Rect {
        x: r.x + by,
        w: r.w - 2 * by,
        ..r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::Category;
    #[test]
    fn compact_dock_and_folders_use_separate_click_targets() {
        let l = Layout::new(3, false);
        assert_eq!((l.w, l.h), (160, 52));
        assert_eq!(l.bar().h, 40);
        assert_eq!(l.hit(24, 20), Some(Hit::Button));
        assert_eq!(l.hit(60, 20), Some(Hit::Window(0)));
        assert_eq!(l.hit(60, 44), None);
        let l = Layout::new(3, true);
        assert_eq!(l.w, PANEL_W);
        assert!(l.row(ROWS - 1).y + ROW_H <= PANEL_H - FOOTER_H - PAD);
        assert_eq!(l.hit(l.pin(0).x + 2, l.pin(0).y + 2), Some(Hit::Pin(0)));
        assert_eq!(l.hit(l.row(0).x + 20, l.row(0).y + 10), Some(Hit::Row(0)));
        assert_eq!(l.hit(10, PANEL_H + 4), None);
        let short = Layout::new(3, true).with_rows(1);
        assert_eq!(short.h, l.h - 7 * ROW_H);
        assert_eq!(short.panel().unwrap().h, PANEL_H - 7 * ROW_H);
        assert_eq!(short.hit(short.row(1).x + 4, short.row(1).y + 4), None);
        assert_eq!(Layout::new(0, true).with_rows(0).rows, 1);
    }
    #[test]
    fn ac7_dock_and_launcher_float_inset() {
        assert_eq!(BAR_H, 52);
        for windows in [0, 3, 12] {
            let l = Layout::new(windows, false);
            assert_eq!(l.bar().y + l.bar().h, l.h - INSET);
            let open = Layout::new(windows, true).with_rows(3);
            let p = open.panel().unwrap();
            assert_eq!(p.y + p.h + INSET, open.bar().y);
        }
    }

    /// Round 4: an app with a dialog waiting for the user gets an accent dot on its dock icon.
    #[test]
    fn a_waiting_dialog_marks_its_dock_icon() {
        let l = Layout::new(2, false);
        let slots = [
            Slot {
                running: 1,
                attention: true,
                ..Slot::default()
            },
            Slot {
                running: 1,
                ..Slot::default()
            },
        ];
        let items = l.draw(&slots, None, &[], &[], None, "");
        let dots: Vec<_> = items
            .iter()
            .filter(|i| matches!(i, Item::Round(r, _, c) if r.w == ATTENTION_DOT && *c == ACCENT))
            .collect();
        assert_eq!(dots.len(), 1);
    }

    #[test]
    fn launcher_draws_folders_and_explicit_pin_actions() {
        let entries = [Entry {
            id: "terminal.desktop".into(),
            name: "Terminal".into(),
            icon: None,
            argv: vec!["terminal".into()],
            wm_class: None,
            category: Category::System,
        }];
        let mut menu = Menu::new(&entries);
        let l = Layout::new(1, true);
        let draw = |menu: &Menu| {
            l.draw(
                &[Slot {
                    running: 1,
                    ..Slot::default()
                }],
                Some(menu),
                &entries,
                &[],
                None,
                "",
            )
        };
        let items = draw(&menu);
        assert!(
            items
                .iter()
                .any(|i| matches!(i, Item::Icon(IconOf::Folder(Category::System), _, _)))
        );
        assert!(
            items
                .iter()
                .any(|i| matches!(i, Item::Text(t, ..) if t == "1 app"))
        );
        menu.open(Category::System, &entries);
        let items = draw(&menu);
        for name in ["Back", "System", "Terminal", "Pin"] {
            assert!(
                items
                    .iter()
                    .any(|i| matches!(i, Item::Text(t, ..) if t == name))
            );
        }
        assert!(items.iter().any(|i| matches!(i, Item::Panel(..))));
    }
}
