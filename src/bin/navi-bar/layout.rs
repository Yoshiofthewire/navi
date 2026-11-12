//! Dock and launcher geometry in logical pixels; transparent margins stay outside input regions.

use crate::{
    context::MENU_W,
    desktop::{Category, Entry},
    menu::{Menu, ROWS, Row},
    theme::{ACCENT, BAR_BG, DIM, INK, INSET, PANEL_EDGE, Panel, SELECTED},
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
/// The launcher's footer when it has no message.
pub const HINT: &str = "Right-click or press Menu for app options.";
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
    Back,
    /// Anywhere in the open context menu, above whatever is under it.
    Menu,
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
    /// The open context menu's rect.
    context: Option<Rect>,
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
            context: None,
        }
    }

    /// Surface point `(x, y)` relative to the surface's bottom centre, which stays put on screen
    /// as the bottom-anchored, centred surface resizes.
    pub fn anchor(&self, x: i32, y: i32) -> (i32, i32) {
        (x - self.w / 2, y - self.h)
    }

    /// Opens a context menu `h` tall at `anchor`. A dock menu grows the surface and sits `INSET`
    /// above the dock; a launcher menu stays inside the launcher, which grows to hold it.
    pub fn with_context(mut self, anchor: (i32, i32), h: i32) -> Self {
        let bounds = match self.panel() {
            None => {
                self.w = self.w.max(MENU_W);
                self.h = BAR_H + PANEL_GAP + h;
                Rect {
                    x: 0,
                    y: 0,
                    w: self.w,
                    h,
                }
            }
            Some(p) => {
                self.h += (h - p.h).max(0);
                self.panel().unwrap_or(p)
            }
        };
        let (x, y) = (anchor.0 + self.w / 2, anchor.1 + self.h);
        let flip = |at: i32, len: i32, lo: i32, size: i32| {
            let at = if at + len > lo + size { at - len } else { at };
            at.clamp(lo, (lo + size - len).max(lo))
        };
        self.context = Some(Rect {
            x: flip(x, MENU_W, bounds.x, bounds.w),
            y: if self.open {
                flip(y, h, bounds.y, bounds.h)
            } else {
                0
            },
            w: MENU_W,
            h,
        });
        self
    }

    pub fn context(&self) -> Option<Rect> {
        self.context
    }

    /// Where the input region takes clicks: the dock, the launcher and the context menu.
    pub fn input(&self) -> Vec<Rect> {
        [Some(self.bar()), self.panel(), self.context]
            .into_iter()
            .flatten()
            .collect()
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
    /// A row's tail column: a folder's app count, or `Pinned`.
    fn tail(&self, k: usize) -> Rect {
        let r = self.row(k);
        Rect {
            x: r.x + r.w - 60,
            y: r.y + 2,
            w: 56,
            h: ROW_H - 4,
        }
    }
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if self.context.is_some_and(|c| c.contains(x, y)) {
            return Some(Hit::Menu);
        }
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
                let highlighted = selected || hover == Some(Hit::Row(k));
                if highlighted {
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
                    Row::App(e) => {
                        let pinned = pins.contains(&entries[e].id);
                        let tail = if pinned { "Pinned" } else { "" };
                        (entries[e].name.as_str(), tail.into(), IconOf::Entry(e))
                    }
                };
                out.push(Item::Icon(of, icon, 1.0));
                let x = icon.x + ROW_ICON + 12;
                // The name runs to the row's end, or to its tail.
                let end = if tail.is_empty() {
                    r.x + r.w - PAD
                } else {
                    self.tail(k).x - 8
                };
                let text = Rect { x, w: end - x, ..r };
                out.push(Item::Text(name.into(), text, INK));
                if !tail.is_empty() {
                    let folder = matches!(row, Row::Folder(_));
                    let ink = if folder && highlighted { INK } else { DIM };
                    out.push(Item::Text(tail, self.tail(k), ink));
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
            let hint = if message.is_empty() { HINT } else { message };
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
        // The row's old Pin button column is now part of the row.
        assert_eq!(l.hit(l.tail(0).x + 2, l.tail(0).y + 2), Some(Hit::Row(0)));
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

    fn terminal() -> Entry {
        Entry {
            id: "terminal.desktop".into(),
            name: "Terminal".into(),
            icon: None,
            argv: vec!["terminal".into()],
            wm_class: None,
            category: Category::System,
            actions: Vec::new(),
        }
    }

    fn texts(items: &[Item]) -> Vec<(&str, u32)> {
        let mut out = Vec::new();
        for i in items {
            if let Item::Text(t, _, c) = i {
                out.push((t.as_str(), *c));
            }
        }
        out
    }

    /// AC5 (was `launcher_draws_folders_and_explicit_pin_actions`): rows have no Pin button; a
    /// pinned app says `Pinned` in DIM, and the footer has the new hint.
    #[test]
    fn launcher_draws_folders_and_pinned_rows() {
        let entries = [terminal()];
        let mut menu = Menu::new(&entries);
        let l = Layout::new(1, true);
        let draw = |menu: &Menu, pins: &[String]| {
            let slot = Slot {
                running: 1,
                ..Slot::default()
            };
            l.draw(&[slot], Some(menu), &entries, pins, None, "")
        };
        let items = draw(&menu, &[]);
        assert!(
            items
                .iter()
                .any(|i| matches!(i, Item::Icon(IconOf::Folder(Category::System), _, _)))
        );
        assert!(texts(&items).iter().any(|(t, _)| *t == "1 app"));
        menu.open(Category::System, &entries);
        for pins in [vec![], vec!["terminal.desktop".to_string()]] {
            let items = draw(&menu, &pins);
            let t = texts(&items);
            for name in ["Back", "System", "Terminal", HINT] {
                assert!(t.iter().any(|(s, _)| *s == name), "{name}");
            }
            assert!(
                !t.iter().any(|(s, _)| *s == "Pin" || *s == "Unpin"),
                "{t:?}"
            );
            assert_eq!(t.contains(&("Pinned", DIM)), !pins.is_empty());
            assert!(items.iter().any(|i| matches!(i, Item::Panel(..))));
            // An unpinned name runs to PAD from the row's end.
            let name = items.iter().find_map(|i| match i {
                Item::Text(s, r, _) if s == "Terminal" => Some(*r),
                _ => None,
            });
            let (row, name) = (l.row(0), name.unwrap());
            if pins.is_empty() {
                assert_eq!(name.x + name.w, row.x + row.w - PAD);
            } else {
                assert!(name.x + name.w <= l.tail(0).x);
            }
        }
        // No hit anywhere is a pin button.
        for x in (0..l.w).step_by(4) {
            for y in (0..l.h).step_by(4) {
                assert!(matches!(
                    l.hit(x, y),
                    None | Some(Hit::Back | Hit::Row(_) | Hit::Button | Hit::Window(_))
                ));
            }
        }
    }

    /// AC4: dock menus lie inside the surface, `INSET` above the dock, from either end.
    #[test]
    fn ac4_dock_menu_placement() {
        let h = 300;
        for windows in [1, 3, 12] {
            let dock = Layout::new(windows, false);
            let b = dock.bar();
            for x in [b.x, b.x + b.w / 2, b.x + b.w - 1] {
                let anchor = dock.anchor(x, b.y + 10);
                let l = Layout::new(windows, false).with_context(anchor, h);
                let r = l.context().unwrap();
                assert!(
                    r.x >= 0 && r.x + r.w <= l.w,
                    "{windows} {x}: {r:?} in {}",
                    l.w
                );
                assert_eq!(r.y + r.h + INSET, l.bar().y, "{windows} {x}");
                assert_eq!((l.w, l.h), (dock.w.max(MENU_W), BAR_H + INSET + h));
                assert_eq!(l.bar().w, b.w, "the dock stays put");
                // Hits: the menu above, dock icons beside it.
                assert_eq!(l.hit(r.x + 4, r.y + 4), Some(Hit::Menu));
                assert_eq!(
                    l.hit(l.target(0).x + 2, l.target(0).y + 2),
                    Some(Hit::Button)
                );
                assert!(l.input().contains(&r));
            }
        }
    }

    /// AC4: a launcher menu flips at each corner and stays in the panel; the biggest one fits.
    #[test]
    fn ac4_launcher_menu_placement() {
        let open = Layout::new(3, true);
        let p = open.panel().unwrap();
        let h = 200;
        for (x, y) in [
            (p.x + 1, p.y + 1),
            (p.x + p.w - 2, p.y + 1),
            (p.x + 1, p.y + p.h - 2),
            (p.x + p.w - 2, p.y + p.h - 2),
        ] {
            let l = open.with_context(open.anchor(x, y), h);
            let r = l.context().unwrap();
            let p = l.panel().unwrap();
            assert!(r.x >= p.x && r.x + r.w <= p.x + p.w, "{x},{y}: {r:?}");
            assert!(r.y >= p.y && r.y + r.h <= p.y + p.h, "{x},{y}: {r:?}");
            // Opened at the pointer, or flipped to end there.
            assert!(r.x == x || r.x + r.w == x, "{x}: {r:?}");
            assert!(r.y == y || r.y + r.h == y, "{y}: {r:?}");
            let row = l.row(2);
            if r.contains(row.x + 100, row.y + 4) {
                assert_eq!(
                    l.hit(row.x + 100, row.y + 4),
                    Some(Hit::Menu),
                    "above the rows"
                );
            }
        }
        // The biggest launcher menu: header, Open, six actions, a line and Pin.
        let mut e = terminal();
        e.actions = (0..crate::desktop::MAX_ACTIONS)
            .map(|n| crate::desktop::Action {
                id: n.to_string(),
                name: "A".into(),
                argv: vec!["a".into()],
            })
            .collect();
        let app = crate::context::App {
            entry: Some(&e),
            app_id: "",
            pinned: false,
            windows: 0,
            overdue: 0,
        };
        let target = crate::context::Target {
            key: e.id.clone(),
            dock: false,
        };
        let m = crate::context::Menu::new(target, crate::context::lines(&app, false), (0, 0), true);
        assert!(m.height() <= PANEL_H, "{}", m.height());
        // A short result list grows back to hold it.
        let short = Layout::new(3, true).with_rows(1);
        let l = short.with_context(short.anchor(p.x + 1, short.row(0).y), m.height());
        assert!(l.panel().unwrap().h >= m.height());
        let r = l.context().unwrap();
        assert!(l.panel().unwrap().contains(r.x, r.y));
    }

    /// AC4: anchored at the bottom centre, a menu keeps its spot on screen as the surface grows.
    #[test]
    fn ac4_anchor_survives_a_resize() {
        // Smithay centres a bottom-anchored layer at `W / 2 - w / 2`.
        let origin = |l: &Layout, out_w: i32, out_h: i32| (out_w / 2 - l.w / 2, out_h - l.h);
        for (out_w, windows) in [(3440, 1), (3441, 3), (1800, 12)] {
            let dock = Layout::new(windows, false);
            let (px, py) = (dock.target(1).x + 5, dock.target(1).y + 5);
            let o = origin(&dock, out_w, 1440);
            let on_screen = (o.0 + px, o.1 + py);
            let anchor = dock.anchor(px, py);
            let grown = Layout::new(windows, false).with_context(anchor, 250);
            let g = origin(&grown, out_w, 1440);
            let back = (anchor.0 + grown.w / 2 + g.0, anchor.1 + grown.h + g.1);
            assert_eq!(back, on_screen, "{out_w} {windows}");
            // The dock itself didn't move on screen.
            assert_eq!(
                (g.0 + grown.bar().x, g.1 + grown.bar().y),
                (o.0 + dock.bar().x, o.1 + dock.bar().y)
            );
        }
    }
}
