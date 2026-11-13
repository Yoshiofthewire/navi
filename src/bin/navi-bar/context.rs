//! Right-click menus on dock icons and launcher app rows: what they hold, keyboard moves, and
//! the force-close confirmation, drawn in one panel. main.rs runs the actions.

use std::time::{Duration, Instant};

use crate::{
    desktop::Entry,
    layout::{Item, Rect},
    menu::Row,
    theme::{ACCENT, BAR_BG, DIM, HOVER, INK, PANEL_EDGE, Panel, SELECTED},
};

/// The width at `BASE_PX`, and the least at any size.
pub const MENU_W: i32 = 260;
/// The text size the sizes below are for; bigger text scales the menu with it.
const BASE_PX: i32 = 14;
const PAD: i32 = 12;
const ROW_H: i32 = 32;
/// A 1 px line with 4 px above and below.
const SEP_H: i32 = 9;
const LINE_H: i32 = 24;
const BUTTON_H: i32 = 36;
/// Space around the confirmation's buttons.
const GAP: i32 = 12;
const MARK_W: i32 = 3;
/// A label's inset in its row or button.
const TEXT_X: i32 = 12;
/// Untrusted names are cut to this many chars before they are measured.
const NAME_CHARS: usize = 256;
/// navi-bar can't see which windows share a process or X connection, so no count.
const BODY: &str = "It ignored Close for 90 s. Force close ends its process (or X connection) at \
                    once, with every window it has open. Unsaved work is lost.";
/// The confirmation acts on nothing for this long after it appears: a double-click or a double
/// Enter on the item that opened it never confirms.
pub const ARM: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Open,
    NewWindow,
    /// A desktop action, by id.
    Action(String),
    Pin,
    Unpin,
    /// Close the app's windows, this many.
    Close(usize),
    ForceClose,
}

impl Kind {
    /// The same item across a re-resolve: Pin and Unpin are one toggle, Close any count.
    fn same(&self, other: &Kind) -> bool {
        use Kind::*;
        match (self, other) {
            (Pin | Unpin, Pin | Unpin) | (Close(_), Close(_)) => true,
            (a, b) => a == b,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Header(String),
    Item(Kind, String),
    Sep,
}

impl Line {
    fn h(&self, t: &Text) -> i32 {
        match self {
            Line::Sep => SEP_H,
            _ => t.scale(ROW_H),
        }
    }
}

/// The text size in px, and a glyph's advance at it, `None` for one the font lacks.
pub struct Text<'a> {
    pub px: i32,
    pub advance: &'a dyn Fn(char) -> Option<f32>,
}

impl Text<'_> {
    /// `v` px at `BASE_PX`, at this text size; never smaller.
    fn scale(&self, v: i32) -> i32 {
        v * self.px.max(BASE_PX) / BASE_PX
    }

    fn w(&self, s: &str) -> f32 {
        s.chars().filter_map(self.advance).sum()
    }

    /// `text` without controls or bidi overrides, cut with `…` to `max` px.
    fn fit(&self, text: &str, max: f32) -> String {
        let clean = crate::notify::sanitize(text, NAME_CHARS);
        crate::font::fit(&clean, max, self.advance)
            .into_iter()
            .collect()
    }

    /// `Force close {name}?`, cutting only the name. 1 px spare, so the whole heading measured
    /// at once still fits.
    fn heading(&self, name: &str, max: f32) -> String {
        let room = max - self.w("Force close ?") - 1.0;
        format!("Force close {}?", self.fit(name, room))
    }
}

/// The menu's width at text size `px`: no wider than the launcher, which holds it.
pub fn width(px: i32) -> i32 {
    let t = Text {
        px,
        advance: &|_| None,
    };
    t.scale(MENU_W).min(crate::layout::PANEL_W)
}

/// The text room in a row of the menu `w` wide.
fn text_w(w: i32) -> f32 {
    (w - 2 * PAD - 2 * TEXT_X) as f32
}

/// The app a menu acts on: its desktop ID, else its app ID, and whether it's a dock icon's.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub key: String,
    pub dock: bool,
}

/// What the target is now.
pub struct App<'a> {
    pub entry: Option<&'a Entry>,
    pub app_id: &'a str,
    pub pinned: bool,
    pub windows: usize,
    /// Its windows whose close went unanswered for 90 s.
    pub overdue: usize,
}

/// The menu's target for a launcher row: apps only.
pub fn row_target(row: Row, entries: &[Entry]) -> Option<Target> {
    match row {
        Row::App(e) => Some(Target {
            key: entries.get(e)?.id.clone(),
            dock: false,
        }),
        Row::Folder(_) => None,
    }
}

/// The menu for `app`, from the dock or the launcher.
pub fn lines(app: &App, dock: bool) -> Vec<Line> {
    let name = match app.entry {
        Some(e) => e.name.as_str(),
        None if !app.app_id.is_empty() => app.app_id,
        None => "Unknown app",
    };
    let mut out = vec![Line::Header(name.into())];
    let item = |kind: Kind| {
        let label = match &kind {
            Kind::Open => "Open".into(),
            Kind::NewWindow => "New window".into(),
            Kind::Action(id) => app
                .entry
                .and_then(|e| e.actions.iter().find(|a| &a.id == id))
                .map_or_else(String::new, |a| a.name.clone()),
            Kind::Pin => "Pin to dock".into(),
            Kind::Unpin => "Unpin from dock".into(),
            Kind::Close(1) => "Close window".into(),
            Kind::Close(n) => format!("Close all {n} windows"),
            Kind::ForceClose => "Force close…".into(),
        };
        Line::Item(kind, label)
    };
    let running = dock && app.windows > 0;
    if let Some(e) = app.entry {
        out.push(item(if running { Kind::NewWindow } else { Kind::Open }));
        out.extend(e.actions.iter().map(|a| item(Kind::Action(a.id.clone()))));
        out.push(Line::Sep);
        out.push(item(if app.pinned { Kind::Unpin } else { Kind::Pin }));
    }
    if running {
        if app.entry.is_some() {
            out.push(Line::Sep);
        }
        out.push(item(Kind::Close(app.windows)));
        if app.overdue > 0 {
            out.push(item(Kind::ForceClose));
        }
    }
    out
}

/// The force-close confirmation, in place of the items.
#[derive(Clone, Debug, PartialEq)]
pub struct Confirm {
    pub name: String,
    /// 0 Cancel, 1 Force close.
    pub selected: usize,
    pub opened: Instant,
    /// The pointer's reference spot, relative to the surface's bottom centre
    /// (`Layout::anchor`), so the motion navi sends when the surface resizes isn't a move.
    /// `None` after the panel moved on screen: the next motion is the new reference.
    pub at: Option<(i32, i32)>,
    /// The pointer really moved since it opened.
    pub moved: bool,
    /// The button the pointer rests on, tinted only: hover never selects.
    pub hovered: Option<usize>,
}

impl Confirm {
    /// `BODY` in lines that fit the menu `w` wide.
    fn body(w: i32, t: &Text) -> Vec<String> {
        crate::cards::wrap(BODY, text_w(w), usize::MAX, &t.advance)
    }

    fn aged(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.opened) >= ARM
    }

    /// Whether a press on a button counts: `ARM` passed and the pointer really moved.
    pub fn armed(&self, now: Instant) -> bool {
        self.moved && self.aged(now)
    }

    /// Enter: the selected button once `ARM` has passed; before that the key is swallowed.
    pub fn enter(&self, now: Instant) -> Option<Hit> {
        let hit = [Hit::Cancel, Hit::Confirm][self.selected];
        self.aged(now).then_some(hit)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Line(usize),
    Cancel,
    Confirm,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub target: Target,
    pub lines: Vec<Line>,
    /// An item's index in `lines`.
    pub selected: Option<usize>,
    pub confirm: Option<Confirm>,
    /// Where it opened, relative to the surface's bottom centre (`Layout::anchor`).
    pub anchor: (i32, i32),
}

impl Menu {
    /// Opened by keyboard, its first item is selected.
    pub fn new(target: Target, lines: Vec<Line>, anchor: (i32, i32), keyboard: bool) -> Self {
        let mut m = Menu {
            target,
            lines,
            selected: None,
            confirm: None,
            anchor,
        };
        if keyboard {
            m.home();
        }
        m
    }

    fn items(&self) -> Vec<usize> {
        let items = self.lines.iter().enumerate();
        items
            .filter(|(_, l)| matches!(l, Line::Item(..)))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn selected_kind(&self) -> Option<&Kind> {
        match self.lines.get(self.selected?) {
            Some(Line::Item(k, _)) => Some(k),
            _ => None,
        }
    }

    /// Up (-1) or Down (1) through the items, wrapping; from none, the first or last.
    pub fn step(&mut self, by: i32) {
        if let Some(c) = &mut self.confirm {
            c.selected = 1 - c.selected;
            return;
        }
        let items = self.items();
        if items.is_empty() {
            return;
        }
        let n = items.len() as i32;
        let at = self
            .selected
            .and_then(|s| items.iter().position(|&i| i == s));
        let next = match at {
            Some(at) => (at as i32 + by).rem_euclid(n),
            None if by > 0 => 0,
            None => n - 1,
        };
        self.selected = Some(items[next as usize]);
    }

    pub fn home(&mut self) {
        self.selected = self.items().first().copied();
    }

    pub fn end(&mut self) {
        self.selected = self.items().last().copied();
    }

    /// New contents for the same target; the selection follows its kind.
    pub fn update(&mut self, lines: Vec<Line>) {
        let kind = self.selected_kind().cloned();
        self.lines = lines;
        self.selected = kind.and_then(|k| {
            let found = self.lines.iter().enumerate().find(|(_, l)| match l {
                Line::Item(other, _) => k.same(other),
                _ => false,
            });
            found.map(|(i, _)| i)
        });
    }

    /// Whether the app has a window that may be force closed.
    pub fn overdue(&self) -> bool {
        let force = |l: &Line| matches!(l, Line::Item(Kind::ForceClose, _));
        self.lines.iter().any(force)
    }

    /// Shows the force-close confirmation at `now`, Cancel selected, with the pointer at `at`
    /// (anchored, `Layout::anchor`).
    pub fn ask_force(&mut self, now: Instant, at: (i32, i32)) {
        let name = match self.lines.first() {
            Some(Line::Header(n)) => n.clone(),
            _ => String::new(),
        };
        self.confirm = Some(Confirm {
            name,
            selected: 0,
            opened: now,
            at: Some(at),
            moved: false,
            hovered: None,
        });
    }

    /// Pointer motion to `at` (anchored), for `Confirm::armed`: the same spot isn't a move.
    pub fn pointer(&mut self, at: (i32, i32)) {
        if let Some(c) = &mut self.confirm {
            c.moved |= c.at.is_some_and(|was| was != at);
            c.at.get_or_insert(at);
        }
    }

    /// The pointer entered the surface at `at`: a fresh reference, not a move.
    pub fn entered(&mut self, at: (i32, i32)) {
        if let Some(c) = &mut self.confirm {
            c.at = Some(at);
        }
    }

    /// The panel may have moved on screen (an output or scale change): the confirmation arms
    /// again from `now`, and the pointer's next spot is the new reference.
    pub fn moved_on_screen(&mut self, now: Instant) {
        if let Some(c) = &mut self.confirm {
            c.opened = now;
            c.at = None;
            c.moved = false;
        }
    }

    /// The text size went from `was` to `size`. A new size can move a button under a still
    /// pointer, so it re-arms as `moved_on_screen` does.
    pub fn text_size(&mut self, was: u32, size: u32, now: Instant) {
        if was != size {
            self.moved_on_screen(now);
        }
    }

    pub fn height(&self, t: &Text) -> i32 {
        match &self.confirm {
            Some(_) => {
                let body = Confirm::body(width(t.px), t).len() as i32 * t.scale(LINE_H);
                PAD + t.scale(ROW_H) + GAP + t.scale(BUTTON_H) + GAP + body + PAD
            }
            None => PAD + self.lines.iter().map(|l| l.h(t)).sum::<i32>() + PAD,
        }
    }

    /// Each line's rect in the menu at `r`.
    fn rects<'a>(&'a self, r: Rect, t: &'a Text) -> impl Iterator<Item = (usize, Rect)> + 'a {
        let mut y = r.y + PAD;
        self.lines.iter().enumerate().map(move |(i, l)| {
            let at = Rect {
                x: r.x + PAD,
                y,
                w: r.w - 2 * PAD,
                h: l.h(t),
            };
            y += at.h;
            (i, at)
        })
    }

    /// Cancel and Force close, right under the heading: far above the bottom rows (Close,
    /// Force close…) that open the confirmation, whose bottom edge stays put.
    fn buttons(&self, r: Rect, t: &Text) -> [Rect; 2] {
        let w = (r.w - 3 * PAD) / 2;
        let cancel = Rect {
            x: r.x + PAD,
            y: r.y + PAD + t.scale(ROW_H) + GAP,
            w,
            h: t.scale(BUTTON_H),
        };
        [
            cancel,
            Rect {
                x: cancel.x + w + PAD,
                ..cancel
            },
        ]
    }

    /// What's at `(x, y)` in the menu at `r`: an item or a button.
    pub fn hit(&self, r: Rect, t: &Text, x: i32, y: i32) -> Option<Hit> {
        if self.confirm.is_some() {
            let [cancel, force] = self.buttons(r, t);
            return match (cancel.contains(x, y), force.contains(x, y)) {
                (true, _) => Some(Hit::Cancel),
                (_, true) => Some(Hit::Confirm),
                _ => None,
            };
        }
        let mut lines = self.rects(r, t);
        let (i, _) = lines.find(|(_, at)| at.contains(x, y))?;
        matches!(self.lines[i], Line::Item(..)).then_some(Hit::Line(i))
    }

    /// Hover selects the item under the pointer, and only tints a confirmation's button; returns
    /// whether either changed. Only keys move the confirmation's selection, so Enter acts on
    /// what the user chose.
    pub fn hover(&mut self, hit: Hit) -> bool {
        match (hit, &mut self.confirm) {
            (Hit::Line(i), None) if self.selected != Some(i) => {
                self.selected = Some(i);
                true
            }
            (Hit::Cancel | Hit::Confirm, Some(c)) => {
                let was = c.hovered.replace(usize::from(hit == Hit::Confirm));
                was != c.hovered
            }
            _ => false,
        }
    }

    /// The pointer is on neither button, or left: no tint. Returns whether that changed.
    pub fn unhover(&mut self) -> bool {
        self.confirm
            .as_mut()
            .is_some_and(|c| c.hovered.take().is_some())
    }

    /// The menu at `r`, on top of everything else on the surface.
    pub fn draw(&self, r: Rect, t: &Text) -> Vec<Item> {
        let mut out = vec![Item::Panel(r, Panel::Popup, PANEL_EDGE, 1)];
        let row = |y, h| Rect {
            x: r.x + PAD,
            y,
            w: r.w - 2 * PAD,
            h,
        };
        let text = |s: &str, at: Rect, rgb| {
            let at = inset_x(at, TEXT_X);
            Item::Text(t.fit(s, at.w as f32), at, rgb)
        };
        if let Some(c) = &self.confirm {
            let heading = row(r.y + PAD, t.scale(ROW_H));
            let at = inset_x(heading, TEXT_X);
            out.push(Item::Text(t.heading(&c.name, at.w as f32), at, INK));
            let line = Rect {
                y: heading.y + heading.h + GAP / 2,
                h: 1,
                ..heading
            };
            out.push(Item::Fill(line, PANEL_EDGE));
            let buttons = self.buttons(r, t);
            for (i, (b, label)) in buttons
                .into_iter()
                .zip(["Cancel", "Force close"])
                .enumerate()
            {
                let fill = match (c.selected == i, c.hovered == Some(i)) {
                    (true, _) => SELECTED,
                    (false, true) => HOVER,
                    (false, false) => BAR_BG,
                };
                // Force close, the destructive one, always has an accent rim.
                let inner = if i == 1 {
                    out.push(Item::Round(b, 8, ACCENT));
                    inset(b, 2)
                } else {
                    b
                };
                out.push(Item::Round(inner, 8, fill));
                if c.selected == i {
                    out.push(Item::Fill(mark(inner), ACCENT));
                }
                out.push(text(label, b, INK));
            }
            let (top, line_h) = (buttons[0].y + buttons[0].h + GAP, t.scale(LINE_H));
            for (i, body) in Confirm::body(r.w, t).iter().enumerate() {
                out.push(text(body, row(top + i as i32 * line_h, line_h), DIM));
            }
            return out;
        }
        for (i, at) in self.rects(r, t) {
            match &self.lines[i] {
                Line::Header(name) => out.push(text(name, at, DIM)),
                Line::Sep => out.push(Item::Fill(
                    Rect {
                        y: at.y + 4,
                        h: 1,
                        ..at
                    },
                    PANEL_EDGE,
                )),
                Line::Item(_, label) => {
                    if self.selected == Some(i) {
                        out.push(Item::Round(at, 7, SELECTED));
                        out.push(Item::Fill(mark(at), ACCENT));
                    }
                    out.push(text(label, at, INK));
                }
            }
        }
        out
    }
}

/// The selection's accent mark at `r`'s left edge, inside its rounded corners.
fn mark(r: Rect) -> Rect {
    Rect {
        x: r.x,
        y: r.y + 7,
        w: MARK_W,
        h: r.h - 14,
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

/// The target's menu now, or `None` once it's gone: no dock app with its key, or (launcher) no
/// entry.
pub fn resolve(
    target: &Target,
    apps: &[crate::dock::App],
    entries: &[Entry],
    pins: &[String],
) -> Option<Vec<Line>> {
    if !target.dock {
        let entry = entries.iter().find(|e| e.id == target.key)?;
        let app = App {
            entry: Some(entry),
            app_id: &entry.id,
            pinned: pins.contains(&entry.id),
            windows: 0,
            overdue: 0,
        };
        return Some(lines(&app, false));
    }
    let a = apps.iter().find(|a| key(a, entries) == target.key)?;
    let app = App {
        entry: a.entry.and_then(|e| entries.get(e)),
        app_id: &a.app_id,
        pinned: a.pinned,
        windows: a.windows.len(),
        overdue: a.overdue,
    };
    Some(lines(&app, true))
}

/// navi's offer to force close the window whose handle has object ID `id`: `handles` are the
/// shown windows' IDs and overdue states, in `apps`' window order. Only for a listed, overdue
/// window, `open` becomes its app's dock menu, straight in the confirmation; the pointer is
/// off the surface, so its first spot there is the reference. Returns the app's index.
pub fn offer(
    open: &mut Option<Menu>,
    id: u32,
    handles: &[(u32, bool)],
    apps: &[crate::dock::App],
    entries: &[Entry],
    pins: &[String],
    now: Instant,
) -> Option<usize> {
    let win = handles
        .iter()
        .position(|&(h, overdue)| h == id && overdue)?;
    let at = apps.iter().position(|a| a.windows.contains(&win))?;
    let target = Target {
        key: key(&apps[at], entries),
        dock: true,
    };
    let lines = resolve(&target, apps, entries, pins)?;
    let mut m = Menu::new(target, lines, (0, 0), false);
    m.ask_force(now, (0, 0));
    if let Some(c) = &mut m.confirm {
        c.at = None;
    }
    *open = Some(m);
    Some(at)
}

/// A dock app's menu key: its desktop ID, else its app ID.
pub fn key(app: &crate::dock::App, entries: &[Entry]) -> String {
    match app.entry.and_then(|e| entries.get(e)) {
        Some(e) => e.id.clone(),
        None => app.app_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::{Action, Category};

    fn seven(_: char) -> Option<f32> {
        Some(7.0)
    }

    /// The default size, every glyph 7 px.
    const T14: Text = Text {
        px: 14,
        advance: &seven,
    };
    const T24: Text = Text {
        px: 24,
        advance: &seven,
    };

    /// Whichever of `font::PATHS` this host has; CI's release job has only DejaVu Sans.
    fn fonts() -> Vec<(&'static str, fontdue::Font)> {
        let fonts: Vec<_> = crate::font::PATHS
            .iter()
            .filter_map(|&p| {
                let bytes = std::fs::read(p).ok()?;
                let f = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default());
                Some((p, f.unwrap()))
            })
            .collect();
        assert!(!fonts.is_empty(), "no font from font::PATHS installed");
        fonts
    }

    /// `f` with `font` at `px`, measured as `draw_text` draws.
    fn sized<R>(font: &fontdue::Font, px: i32, f: impl FnOnce(&Text) -> R) -> R {
        let advance = |c: char| {
            (font.lookup_glyph_index(c) != 0).then(|| font.metrics(c, px as f32).advance_width)
        };
        f(&Text {
            px,
            advance: &advance,
        })
    }

    /// The menu's rect at the surface's top-left.
    fn at(m: &Menu, t: &Text) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: width(t.px),
            h: m.height(t),
        }
    }

    fn konsole() -> Entry {
        Entry {
            id: "org.kde.konsole.desktop".into(),
            name: "Konsole".into(),
            icon: None,
            argv: vec!["konsole".into()],
            wm_class: None,
            category: Category::System,
            actions: vec![Action {
                id: "NewTab".into(),
                name: "Open a New Tab".into(),
                argv: vec!["konsole".into(), "--new-tab".into()],
            }],
        }
    }

    fn app(entry: Option<&Entry>, pinned: bool, windows: usize, overdue: usize) -> App<'_> {
        App {
            entry,
            app_id: "org.kde.konsole",
            pinned,
            windows,
            overdue,
        }
    }

    fn labels(lines: &[Line]) -> Vec<String> {
        let label = |l: &Line| match l {
            Line::Header(h) => format!("[{h}]"),
            Line::Item(_, t) => t.clone(),
            Line::Sep => "-".into(),
        };
        lines.iter().map(label).collect()
    }

    /// AC1: one test per row of the contents table.
    #[test]
    fn ac1_dock_app_with_windows() {
        let e = konsole();
        let one = lines(&app(Some(&e), false, 1, 0), true);
        let want = [
            "[Konsole]",
            "New window",
            "Open a New Tab",
            "-",
            "Pin to dock",
            "-",
            "Close window",
        ];
        assert_eq!(labels(&one), want);
        let three = lines(&app(Some(&e), true, 3, 0), true);
        let want = [
            "[Konsole]",
            "New window",
            "Open a New Tab",
            "-",
            "Unpin from dock",
            "-",
            "Close all 3 windows",
        ];
        assert_eq!(labels(&three), want);
        // An overdue window adds Force close right below Close.
        let overdue = lines(&app(Some(&e), true, 3, 1), true);
        assert_eq!(
            labels(&overdue)[6..],
            ["Close all 3 windows", "Force close…"]
        );
        assert_eq!(
            overdue[7],
            Line::Item(Kind::ForceClose, "Force close…".into())
        );
    }

    #[test]
    fn ac1_dock_pin_without_windows() {
        let e = konsole();
        let got = lines(&app(Some(&e), true, 0, 0), true);
        assert_eq!(
            labels(&got),
            [
                "[Konsole]",
                "Open",
                "Open a New Tab",
                "-",
                "Unpin from dock"
            ]
        );
    }

    #[test]
    fn ac1_dock_app_without_an_entry() {
        let got = lines(&app(None, false, 1, 0), true);
        assert_eq!(labels(&got), ["[org.kde.konsole]", "Close window"]);
        let got = lines(&app(None, false, 2, 2), true);
        assert_eq!(
            labels(&got),
            ["[org.kde.konsole]", "Close all 2 windows", "Force close…"]
        );
        let nameless = App {
            app_id: "",
            ..app(None, false, 1, 0)
        };
        assert_eq!(labels(&lines(&nameless, true))[0], "[Unknown app]");
    }

    #[test]
    fn ac1_launcher_rows() {
        let e = konsole();
        // Windows never show in the launcher's menu.
        let got = lines(&app(Some(&e), false, 2, 1), false);
        assert_eq!(
            labels(&got),
            ["[Konsole]", "Open", "Open a New Tab", "-", "Pin to dock"]
        );
        let got = lines(&app(Some(&e), true, 0, 0), false);
        assert_eq!(labels(&got)[4], "Unpin from dock");
        let entries = [e];
        assert_eq!(row_target(Row::Folder(Category::System), &entries), None);
        assert_eq!(
            row_target(Row::App(0), &entries),
            Some(Target {
                key: "org.kde.konsole.desktop".into(),
                dock: false
            })
        );
        assert_eq!(row_target(Row::App(1), &entries), None);
    }

    #[test]
    fn ac1_header_and_separators_never_selectable() {
        let e = konsole();
        let target = Target {
            key: e.id.clone(),
            dock: true,
        };
        let mut m = Menu::new(
            target,
            lines(&app(Some(&e), false, 2, 1), true),
            (0, 0),
            false,
        );
        let r = at(&m, &T14);
        for _ in 0..20 {
            m.step(1);
            assert!(m.selected_kind().is_some());
        }
        for (i, at) in m.rects(r, &T14).collect::<Vec<_>>() {
            let hit = m.hit(r, &T14, at.x + 4, at.y + at.h / 2);
            let item = matches!(m.lines[i], Line::Item(..));
            assert_eq!(hit.is_some(), item, "{:?}", m.lines[i]);
        }
    }

    fn menu(windows: usize) -> Menu {
        let e = konsole();
        let target = Target {
            key: e.id.clone(),
            dock: true,
        };
        Menu::new(
            target,
            lines(&app(Some(&e), false, windows, 0), true),
            (0, 0),
            false,
        )
    }

    /// AC2: keyboard moves.
    #[test]
    fn ac2_keyboard_navigation() {
        let mut m = menu(2);
        assert_eq!(m.selected, None);
        m.step(1);
        assert_eq!(m.selected_kind(), Some(&Kind::NewWindow));
        m.step(-1);
        assert_eq!(
            m.selected_kind(),
            Some(&Kind::Close(2)),
            "Up wraps to the last"
        );
        m.step(1);
        assert_eq!(m.selected_kind(), Some(&Kind::NewWindow), "and Down back");
        let mut up = menu(2);
        up.step(-1);
        assert_eq!(up.selected_kind(), Some(&Kind::Close(2)));
        m.end();
        assert_eq!(m.selected_kind(), Some(&Kind::Close(2)));
        m.home();
        assert_eq!(m.selected_kind(), Some(&Kind::NewWindow));
        // Opened by keyboard: the first item.
        let k = Menu::new(m.target.clone(), m.lines.clone(), (0, 0), true);
        assert_eq!(k.selected_kind(), Some(&Kind::NewWindow));

        let mut one = Menu::new(
            m.target.clone(),
            lines(&app(None, false, 1, 0), true),
            (0, 0),
            true,
        );
        for by in [1, -1, 1] {
            one.step(by);
            assert_eq!(one.selected_kind(), Some(&Kind::Close(1)));
        }

        // A window closes: the selection stays on Close, now "Close window".
        m.end();
        m.update(lines(&app(Some(&konsole()), false, 1, 0), true));
        assert_eq!(m.selected_kind(), Some(&Kind::Close(1)));
        assert_eq!(
            m.lines[m.selected.unwrap()],
            Line::Item(Kind::Close(1), "Close window".into())
        );
        // Pin becomes Unpin and stays selected.
        let at = m
            .lines
            .iter()
            .position(|l| matches!(l, Line::Item(Kind::Pin, _)))
            .unwrap();
        m.selected = Some(at);
        m.update(lines(&app(Some(&konsole()), true, 1, 0), true));
        assert_eq!(m.selected_kind(), Some(&Kind::Unpin));
    }

    /// AC2: a gone target closes the menu; `main.rs` closes it when `resolve` finds nothing.
    #[test]
    fn ac2_gone_target_closes() {
        let entries = [konsole()];
        let pins: [String; 0] = [];
        let target = Target {
            key: entries[0].id.clone(),
            dock: true,
        };
        let apps = crate::dock::apps(&pins, &entries, &[]);
        assert!(resolve(&target, &apps, &entries, &pins).is_none());
        let pinned = [entries[0].id.clone()];
        let apps = crate::dock::apps(&pinned, &entries, &[]);
        let got = resolve(&target, &apps, &entries, &pinned).unwrap();
        assert_eq!(labels(&got)[1], "Open");
        // The launcher's target is gone once its entry is uninstalled.
        let row = Target {
            dock: false,
            ..target
        };
        assert!(resolve(&row, &apps, &entries, &pinned).is_some());
        assert!(resolve(&row, &[], &[], &pinned).is_none());
    }

    /// AC6: the selection is a `SELECTED` fill and a 3 px `ACCENT` mark.
    #[test]
    fn ac6_selection_look() {
        let mut m = menu(1);
        let r = at(&m, &T14);
        let marks = |items: &[Item]| {
            let n = items
                .iter()
                .filter(|i| matches!(i, Item::Fill(f, ACCENT) if f.w == MARK_W));
            n.count()
        };
        assert_eq!(marks(&m.draw(r, &T14)), 0);
        m.home();
        let items = m.draw(r, &T14);
        assert_eq!(marks(&items), 1);
        let fill = items.iter().find_map(|i| match i {
            Item::Round(f, 7, SELECTED) => Some(*f),
            _ => None,
        });
        let mark = items.iter().find_map(|i| match i {
            Item::Fill(f, ACCENT) => Some(*f),
            _ => None,
        });
        let (fill, mark) = (fill.expect("a SELECTED fill"), mark.unwrap());
        assert_eq!((mark.x, mark.w), (fill.x, 3));
        assert!(mark.y > fill.y && mark.y + mark.h < fill.y + fill.h);
        assert!(matches!(items[0], Item::Panel(p, Panel::Popup, PANEL_EDGE, 1) if p == r));
        // The confirmation: Cancel selected first.
        m.ask_force(Instant::now(), (0, 0));
        let items = m.draw(at(&m, &T14), &T14);
        assert_eq!(marks(&items), 1);
        assert!(
            items
                .iter()
                .any(|i| matches!(i, Item::Text(t, _, INK) if t == "Force close Konsole?"))
        );
        assert_eq!(m.confirm.as_ref().unwrap().selected, 0);
        m.step(1);
        assert_eq!(m.confirm.as_ref().unwrap().selected, 1);
        m.step(1);
        assert_eq!(m.confirm.as_ref().unwrap().selected, 0);
    }

    /// Round 1: a double press on the row that opens the confirmation (Close or Force close…)
    /// never lands on a button, and the buttons wait for `ARM` and fresh motion; at any size.
    #[test]
    fn a_double_press_never_confirms() {
        let e = konsole();
        let target = Target {
            key: e.id.clone(),
            dock: true,
        };
        for (windows, t) in [(1, &T14), (3, &T14), (1, &T24), (3, &T24)] {
            let lines = lines(&app(Some(&e), false, windows, 1), true);
            let mut m = Menu::new(target.clone(), lines, (0, 0), false);
            // Bottom-anchored like the dock menu: both panels share their bottom edge.
            let bottom = 800;
            let at = |m: &Menu| Rect {
                x: 100,
                y: bottom - m.height(t),
                w: width(t.px),
                h: m.height(t),
            };
            let menu = at(&m);
            let rows: Vec<Rect> = m
                .rects(menu, t)
                .filter(|(i, _)| {
                    matches!(
                        m.lines[*i],
                        Line::Item(Kind::Close(_) | Kind::ForceClose, _)
                    )
                })
                .map(|(_, r)| r)
                .collect();
            assert_eq!(rows.len(), 2);
            let now = Instant::now();
            m.ask_force(now, (5, -50));
            let confirm = at(&m);
            assert_eq!(confirm.y + confirm.h, bottom);
            for r in &rows {
                for x in r.x..r.x + r.w {
                    for y in r.y..r.y + r.h {
                        let hit = m.hit(confirm, t, x, y);
                        assert_eq!(hit, None, "{windows}: ({x}, {y})");
                    }
                }
            }
            let c = m.confirm.clone().unwrap();
            assert!(!c.armed(now + ARM), "no motion yet");
            // navi's motion for the resize, at the same spot, doesn't arm it.
            m.pointer((5, -50));
            assert!(!m.confirm.as_ref().unwrap().armed(now + ARM));
            m.pointer((6, -50));
            let c = m.confirm.as_ref().unwrap();
            assert!(!c.armed(now + ARM - Duration::from_millis(1)));
            assert!(c.armed(now + ARM));
        }
    }

    /// Round 2: hover never selects a button, and Enter waits for `ARM`, then acts on the
    /// default Cancel.
    #[test]
    fn hover_never_selects_and_enter_waits() {
        let e = konsole();
        let target = Target {
            key: e.id.clone(),
            dock: true,
        };
        let mut m = Menu::new(
            target,
            lines(&app(Some(&e), false, 1, 1), true),
            (0, 0),
            false,
        );
        let t = Instant::now();
        m.ask_force(t, (0, 0));
        assert!(m.hover(Hit::Confirm), "a tint");
        assert!(!m.hover(Hit::Line(1)), "the items are gone");
        let c = m.confirm.as_ref().unwrap();
        assert_eq!(c.selected, 0, "hover leaves Cancel selected");
        assert_eq!(c.enter(t), None);
        assert_eq!(c.enter(t + ARM - Duration::from_millis(1)), None);
        assert_eq!(c.enter(t + ARM), Some(Hit::Cancel));
        m.step(1);
        assert_eq!(
            m.confirm.as_ref().unwrap().enter(t + ARM),
            Some(Hit::Confirm)
        );
    }

    /// Round 3: a panel move on screen (output resize, scale) restarts the arming; navi's
    /// motion that follows is the new reference, not a move, and so is a pointer enter.
    #[test]
    fn a_panel_move_restarts_arming() {
        let e = konsole();
        let target = Target {
            key: e.id.clone(),
            dock: true,
        };
        let mut m = Menu::new(
            target,
            lines(&app(Some(&e), false, 1, 1), true),
            (0, 0),
            false,
        );
        let t = Instant::now();
        m.ask_force(t, (5, -50));
        let later = t + Duration::from_secs(2);
        m.moved_on_screen(later);
        // navi's motion at the still pointer, now at a new panel-relative spot.
        m.pointer((5, -104));
        let c = m.confirm.as_ref().unwrap();
        assert!(
            !c.armed(later + ARM),
            "the motion after the move is only the reference"
        );
        assert_eq!(c.enter(later + ARM - Duration::from_millis(1)), None);
        assert_eq!(c.enter(later + ARM), Some(Hit::Cancel));
        m.pointer((5, -90));
        let c = m.confirm.as_ref().unwrap();
        assert!(
            !c.armed(later + ARM - Duration::from_millis(1)),
            "the timer restarted"
        );
        assert!(c.armed(later + ARM));
        // A pointer coming back in sets the reference without arming.
        m.moved_on_screen(later);
        m.entered((40, -60));
        m.pointer((40, -60));
        assert!(!m.confirm.as_ref().unwrap().armed(later + ARM));
    }

    /// Job 040 AC3: navi's offer opens the app's dock menu in the confirmation, only for a
    /// listed, overdue window.
    #[test]
    fn ac3_force_offer_opens_the_confirmation_only_when_overdue() {
        let entries = [konsole()];
        let pins: [String; 0] = [];
        let running = |overdue| crate::dock::Running {
            app_id: "org.kde.konsole",
            focused: false,
            minimized: false,
            attention: false,
            overdue,
        };
        let other = crate::dock::Running {
            app_id: "other",
            ..running(false)
        };
        let windows = [other, running(false), running(true)];
        let apps = crate::dock::apps(&pins, &entries, &windows);
        let handles = [
            (0xFF00_0001, false),
            (0xFF00_0002, false),
            (0xFF00_0003, true),
        ];
        let t = Instant::now();
        let mut open = None;
        let at = offer(&mut open, 0xFF00_0003, &handles, &apps, &entries, &pins, t);
        assert_eq!(apps[at.unwrap()].entry, Some(0), "Konsole's slot");
        let m = open.as_ref().unwrap();
        assert_eq!(m.target.key, "org.kde.konsole.desktop");
        assert!(m.target.dock);
        let c = m.confirm.as_ref().expect("in the confirmation");
        assert_eq!(c.name, "Konsole");
        assert_eq!(c.selected, 0, "Cancel");
        assert!(!c.armed(t + Duration::from_secs(60)), "no motion yet");
        // The pointer comes from off the surface: its first spot is the reference.
        let mut m = m.clone();
        m.pointer((3, -40));
        assert!(!m.confirm.as_ref().unwrap().armed(t + ARM));
        m.pointer((4, -40));
        assert!(
            !m.confirm
                .as_ref()
                .unwrap()
                .armed(t + ARM - Duration::from_millis(1))
        );
        assert!(m.confirm.as_ref().unwrap().armed(t + ARM));

        // Unknown, listed but not overdue (another app's, or this app's other window): nothing,
        // and an open menu stays as it was.
        let mine = Menu::new(
            Target {
                key: "other".into(),
                dock: true,
            },
            vec![Line::Header("other".into())],
            (7, -9),
            true,
        );
        for id in [0, 0xFF00_0001, 0xFF00_0002, 0xFF00_0009] {
            let mut open = Some(mine.clone());
            assert_eq!(
                offer(&mut open, id, &handles, &apps, &entries, &pins, t),
                None
            );
            assert_eq!(open.as_ref(), Some(&mine), "{id:#x}");
            let mut none = None;
            assert_eq!(
                offer(&mut none, id, &handles, &apps, &entries, &pins, t),
                None
            );
            assert!(none.is_none());
        }
    }

    fn confirming() -> Menu {
        let mut m = menu(3);
        m.lines = lines(&app(Some(&konsole()), false, 3, 1), true);
        m.ask_force(Instant::now(), (0, 0));
        m
    }

    /// Job 041 AC1: hovering a button tints it and changes nothing else.
    #[test]
    fn hover_tints_without_selecting() {
        let mut m = confirming();
        let t = m.confirm.as_ref().unwrap().opened;
        m.pointer((1, 0));
        let before = m.confirm.clone().unwrap();
        assert!(m.hover(Hit::Confirm));
        let c = m.confirm.clone().unwrap();
        assert_eq!(c.hovered, Some(1));
        assert_eq!(c.selected, 0);
        assert_eq!(c.enter(t + ARM), Some(Hit::Cancel));
        assert_eq!(c.armed(t + ARM), before.armed(t + ARM));
        assert_eq!(c.armed(t), before.armed(t));
        assert_eq!(
            (c.moved, c.at, c.opened),
            (before.moved, before.at, before.opened)
        );
        assert!(!m.hover(Hit::Confirm), "no change");
        assert!(m.hover(Hit::Cancel));
        assert_eq!(m.confirm.as_ref().unwrap().hovered, Some(0));
    }

    fn marks(items: &[Item]) -> Vec<Rect> {
        let marks = items.iter().filter_map(|i| match i {
            Item::Fill(f, ACCENT) if f.w == MARK_W => Some(*f),
            _ => None,
        });
        marks.collect()
    }

    /// Job 041 AC2: the hovered button gets the `HOVER` fill and no mark; motion off both
    /// buttons and a leave clear it.
    #[test]
    fn hover_tint_draws_and_clears() {
        let mut m = confirming();
        let r = at(&m, &T14);
        let plain = m.draw(r, &T14);
        let [cancel, force] = m.buttons(r, &T14);
        assert!(m.hover(Hit::Confirm));
        let tinted = m.draw(r, &T14);
        assert_ne!(tinted, plain);
        // Force close's fill sits inside its accent rim.
        let fill = tinted.iter().find_map(|i| match i {
            Item::Round(b, 8, c) if *b == inset(force, 2) => Some(*c),
            _ => None,
        });
        let fill = fill.expect("Force close's fill");
        assert_eq!(fill, HOVER);
        assert!(fill != BAR_BG && fill != SELECTED);
        let cancel_fill = tinted.iter().find_map(|i| match i {
            Item::Round(b, 8, c) if *b == cancel => Some(*c),
            _ => None,
        });
        assert_eq!(cancel_fill, Some(SELECTED));
        let marks = marks(&tinted);
        assert_eq!(marks.len(), 1);
        assert!(
            cancel.contains(marks[0].x, marks[0].y),
            "the mark is on Cancel"
        );

        // A motion off both buttons (main.rs: no `hit`), then a leave, clear it.
        assert!(m.unhover());
        assert_eq!(m.confirm.as_ref().unwrap().hovered, None);
        assert_eq!(m.draw(r, &T14), plain);
        assert!(m.hover(Hit::Confirm));
        assert!(m.unhover());
        assert!(!m.unhover(), "already clear");
        assert_eq!(m.draw(r, &T14), plain);
        // Hovering the selected one keeps the selection's look.
        assert!(m.hover(Hit::Cancel));
        assert_eq!(m.draw(r, &T14), plain);
    }

    /// Job 041 AC5 and 040 AC4: the confirmation is true for either kind of kill, counts
    /// nothing, and shows in full in every font navi-bar may load at every size Settings allows.
    #[test]
    fn ac4_confirmation_text_is_honest_and_fits() {
        assert!(BODY.contains("process"), "{BODY}");
        assert!(BODY.contains("(or X connection)"), "{BODY}");
        assert!(BODY.contains("every window it has open"), "{BODY}");
        assert!(BODY.contains("Unsaved work is lost"), "{BODY}");
        let digits: String = BODY.chars().filter(char::is_ascii_digit).collect();
        assert_eq!(digits, "90", "no window count: {BODY}");
        let m = confirming();
        for (path, font) in &fonts() {
            for px in 12..=24 {
                sized(font, px, |t| {
                    let mut body = Vec::new();
                    for item in m.draw(at(&m, t), t) {
                        if let Item::Text(text, at, DIM) = item {
                            let w = t.w(&text);
                            assert!(w <= at.w as f32, "{path} {px}: {text:?} {w} > {}", at.w);
                            assert!(!text.contains('…'), "{path} {px}: {text:?}");
                            body.push(text);
                        }
                    }
                    assert_eq!(body.join(" "), BODY, "{path} {px}");
                });
            }
        }
    }

    /// Job 041 AC6: fixed labels fit whole at every size, and the panel holds what it draws.
    #[test]
    fn fixed_text_fits_and_the_panel_holds_it() {
        let e = konsole();
        let menus = [
            lines(&app(Some(&e), false, 99, 1), true),
            lines(&app(Some(&e), true, 1, 0), true),
            lines(&app(Some(&e), false, 0, 0), false),
        ];
        let fixed = [
            "Open",
            "New window",
            "Pin to dock",
            "Unpin from dock",
            "Close window",
            "Close all 99 windows",
            "Force close…",
            "Cancel",
            "Force close",
        ];
        for (path, font) in &fonts() {
            for px in 12..=24 {
                sized(font, px, |t| {
                    let mut seen = std::collections::HashSet::new();
                    for lines in &menus {
                        let mut m = menu(1);
                        m.lines = lines.clone();
                        for confirm in [false, true] {
                            if confirm {
                                m.ask_force(Instant::now(), (0, 0));
                            }
                            let r = at(&m, t);
                            let items = m.draw(r, t);
                            let mut bottom = 0;
                            for item in &items[1..] {
                                let (Item::Text(_, b, _) | Item::Fill(b, _) | Item::Round(b, ..)) =
                                    item
                                else {
                                    continue;
                                };
                                bottom = bottom.max(b.y + b.h);
                                let Item::Text(text, b, _) = item else {
                                    continue;
                                };
                                let w = t.w(text);
                                if fixed.contains(&text.as_str()) {
                                    assert!(w <= b.w as f32, "{path} {px}: {text:?} {w}");
                                    seen.insert(text.clone());
                                }
                            }
                            // The last row or body line, then the padding.
                            let last = match confirm {
                                true => bottom,
                                false => m.rects(r, t).last().map(|(_, l)| l.y + l.h).unwrap(),
                            };
                            assert_eq!(last, bottom, "{path} {px}");
                            assert_eq!(r.y + m.height(t), bottom + PAD, "{path} {px}");
                        }
                    }
                    // Every fixed label drew whole, so none was cut with `…`.
                    assert_eq!(seen.len(), fixed.len(), "{path} {px}: {seen:?}");
                });
            }
        }
    }

    /// Job 041 AC7: a long, hostile name is cut; "Force close" and "?" stay.
    #[test]
    fn long_names_keep_their_key_words() {
        let name = format!("A\u{202E}B\u{7}{}", "x".repeat(196));
        assert_eq!(name.chars().count(), 200);
        for (path, font) in &fonts() {
            for px in [14, 24] {
                sized(font, px, |t| {
                    let mut m = menu(1);
                    m.lines[0] = Line::Header(name.clone());
                    let texts = |m: &Menu| {
                        let items = m.draw(at(m, t), t);
                        let texts = items.into_iter().filter_map(|i| match i {
                            Item::Text(s, r, _) => Some((s, r)),
                            _ => None,
                        });
                        texts.collect::<Vec<_>>()
                    };
                    let (header, r) = texts(&m)[0].clone();
                    assert!(header.ends_with('…'), "{path} {px}: {header}");
                    assert!(header.starts_with("AB"), "{path} {px}: {header}");
                    assert!(t.w(&header) <= r.w as f32, "{path} {px}: {header}");
                    m.ask_force(Instant::now(), (0, 0));
                    let (heading, r) = texts(&m)[0].clone();
                    assert!(
                        heading.starts_with("Force close AB"),
                        "{path} {px}: {heading}"
                    );
                    assert!(heading.ends_with("…?"), "{path} {px}: {heading}");
                    assert!(t.w(&heading) <= r.w as f32, "{path} {px}: {heading}");
                    for s in [&header, &heading] {
                        assert!(!s.contains('\u{202E}') && !s.contains('\u{7}'), "{s:?}");
                    }
                    // What `draw_text` draws of it: all of it.
                    let drawn: String = crate::font::fit(&heading, r.w as f32, t.advance)
                        .into_iter()
                        .collect();
                    assert_eq!(drawn, heading, "{path} {px}");
                });
            }
        }
    }

    /// Job 041 AC8: a new text size re-arms an open confirmation as a panel move does; the
    /// same size changes nothing.
    #[test]
    fn a_text_size_change_rearms() {
        let mut m = confirming();
        let t = m.confirm.as_ref().unwrap().opened;
        m.pointer((1, 0));
        assert!(m.confirm.as_ref().unwrap().armed(t + ARM));
        let same = m.clone();
        m.text_size(14, 14, t + ARM);
        assert_eq!(m, same);
        let later = t + ARM;
        m.text_size(14, 24, later);
        let c = m.confirm.as_ref().unwrap();
        assert!(!c.armed(later + ARM));
        assert_eq!(c.at, None);
        assert_eq!(c.enter(later + ARM - Duration::from_millis(1)), None);
        assert_eq!(c.enter(later + ARM), Some(Hit::Cancel));
        // Like an output change: the next spot is only the reference.
        m.pointer((1, 0));
        assert!(!m.confirm.as_ref().unwrap().armed(later + ARM));
    }
}
