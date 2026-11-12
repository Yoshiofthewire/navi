//! Right-click menus on dock icons and launcher app rows: what they hold, keyboard moves, and
//! the force-close confirmation, drawn in one panel. main.rs runs the actions.

use std::time::{Duration, Instant};

use crate::{
    desktop::Entry,
    layout::{Item, Rect},
    menu::Row,
    theme::{ACCENT, BAR_BG, DIM, INK, PANEL_EDGE, Panel, SELECTED},
};

pub const MENU_W: i32 = 260;
const PAD: i32 = 12;
const ROW_H: i32 = 32;
/// A 1 px line with 4 px above and below.
const SEP_H: i32 = 9;
const LINE_H: i32 = 24;
const BUTTON_H: i32 = 36;
/// Space around the confirmation's buttons.
const GAP: i32 = 12;
const MARK_W: i32 = 3;
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
    fn h(&self) -> i32 {
        match self {
            Line::Sep => SEP_H,
            _ => ROW_H,
        }
    }
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
    /// The app's windows, as the Close item counted them.
    pub windows: usize,
    /// 0 Cancel, 1 Force close.
    pub selected: usize,
    pub opened: Instant,
    /// The pointer's reference spot, relative to the surface's bottom centre
    /// (`Layout::anchor`), so the motion navi sends when the surface resizes isn't a move.
    /// `None` after the panel moved on screen: the next motion is the new reference.
    pub at: Option<(i32, i32)>,
    /// The pointer really moved since it opened.
    pub moved: bool,
}

impl Confirm {
    fn body(&self) -> Vec<String> {
        let mut out = vec![
            "It ignored Close for 90 s.".to_string(),
            "Force close ends it at once,".into(),
            "and unsaved work is lost.".into(),
        ];
        if self.windows > 1 {
            out.push(format!("Its other {} windows close too.", self.windows - 1));
        }
        out
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
        let windows = self.lines.iter().find_map(|l| match l {
            Line::Item(Kind::Close(n), _) => Some(*n),
            _ => None,
        });
        self.confirm = Some(Confirm {
            name,
            windows: windows.unwrap_or(1),
            selected: 0,
            opened: now,
            at: Some(at),
            moved: false,
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

    pub fn height(&self) -> i32 {
        match &self.confirm {
            Some(c) => PAD + ROW_H + GAP + BUTTON_H + GAP + c.body().len() as i32 * LINE_H + PAD,
            None => PAD + self.lines.iter().map(Line::h).sum::<i32>() + PAD,
        }
    }

    /// Each line's rect in the menu at `r`.
    fn rects(&self, r: Rect) -> impl Iterator<Item = (usize, Rect)> + '_ {
        let mut y = r.y + PAD;
        self.lines.iter().enumerate().map(move |(i, l)| {
            let at = Rect {
                x: r.x + PAD,
                y,
                w: r.w - 2 * PAD,
                h: l.h(),
            };
            y += l.h();
            (i, at)
        })
    }

    /// Cancel and Force close, right under the heading: far above the bottom rows (Close,
    /// Force close…) that open the confirmation, whose bottom edge stays put.
    fn buttons(&self, r: Rect) -> [Rect; 2] {
        let w = (r.w - 3 * PAD) / 2;
        let cancel = Rect {
            x: r.x + PAD,
            y: r.y + PAD + ROW_H + GAP,
            w,
            h: BUTTON_H,
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
    pub fn hit(&self, r: Rect, x: i32, y: i32) -> Option<Hit> {
        if self.confirm.is_some() {
            let [cancel, force] = self.buttons(r);
            return match (cancel.contains(x, y), force.contains(x, y)) {
                (true, _) => Some(Hit::Cancel),
                (_, true) => Some(Hit::Confirm),
                _ => None,
            };
        }
        let mut lines = self.rects(r);
        let (i, _) = lines.find(|(_, at)| at.contains(x, y))?;
        matches!(self.lines[i], Line::Item(..)).then_some(Hit::Line(i))
    }

    /// Hover selects the item under the pointer; returns whether that changed. It never moves
    /// the confirmation's selection: only keys do, so Enter acts on what the user chose.
    pub fn hover(&mut self, hit: Hit) -> bool {
        match (hit, &self.confirm) {
            (Hit::Line(i), None) if self.selected != Some(i) => {
                self.selected = Some(i);
                true
            }
            _ => false,
        }
    }

    /// The menu at `r`, on top of everything else on the surface.
    pub fn draw(&self, r: Rect) -> Vec<Item> {
        let mut out = vec![Item::Panel(r, Panel::Popup, PANEL_EDGE, 1)];
        let row = |y, h| Rect {
            x: r.x + PAD,
            y,
            w: r.w - 2 * PAD,
            h,
        };
        if let Some(c) = &self.confirm {
            let heading = row(r.y + PAD, ROW_H);
            out.push(Item::Text(
                format!("Force close {}?", c.name),
                inset_x(heading, 12),
                INK,
            ));
            let line = Rect {
                y: heading.y + ROW_H + GAP / 2,
                h: 1,
                ..heading
            };
            out.push(Item::Fill(line, PANEL_EDGE));
            let buttons = self.buttons(r);
            for (i, (b, label)) in buttons
                .into_iter()
                .zip(["Cancel", "Force close"])
                .enumerate()
            {
                let fill = if c.selected == i { SELECTED } else { BAR_BG };
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
                out.push(Item::Text(label.into(), inset_x(b, 12), INK));
            }
            let top = buttons[0].y + BUTTON_H + GAP;
            for (i, text) in c.body().into_iter().enumerate() {
                let at = row(top + i as i32 * LINE_H, LINE_H);
                out.push(Item::Text(text, inset_x(at, 12), DIM));
            }
            return out;
        }
        for (i, at) in self.rects(r) {
            match &self.lines[i] {
                Line::Header(name) => out.push(Item::Text(name.clone(), inset_x(at, 12), DIM)),
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
                    out.push(Item::Text(label.clone(), inset_x(at, 12), INK));
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
        let r = Rect {
            x: 0,
            y: 0,
            w: MENU_W,
            h: m.height(),
        };
        for _ in 0..20 {
            m.step(1);
            assert!(m.selected_kind().is_some());
        }
        for (i, at) in m.rects(r).collect::<Vec<_>>() {
            let hit = m.hit(r, at.x + 4, at.y + at.h / 2);
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
        let r = Rect {
            x: 0,
            y: 0,
            w: MENU_W,
            h: m.height(),
        };
        let marks = |items: &[Item]| {
            let n = items
                .iter()
                .filter(|i| matches!(i, Item::Fill(f, ACCENT) if f.w == MARK_W));
            n.count()
        };
        assert_eq!(marks(&m.draw(r)), 0);
        m.home();
        let items = m.draw(r);
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
        let items = m.draw(Rect { h: m.height(), ..r });
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
    /// never lands on a button, and the buttons wait for `ARM` and fresh motion.
    #[test]
    fn a_double_press_never_confirms() {
        let e = konsole();
        let target = Target {
            key: e.id.clone(),
            dock: true,
        };
        for windows in [1, 3] {
            let lines = lines(&app(Some(&e), false, windows, 1), true);
            let mut m = Menu::new(target.clone(), lines, (0, 0), false);
            // Bottom-anchored like the dock menu: both panels share their bottom edge.
            let bottom = 800;
            let at = |m: &Menu| Rect {
                x: 100,
                y: bottom - m.height(),
                w: MENU_W,
                h: m.height(),
            };
            let menu = at(&m);
            let rows: Vec<Rect> = m
                .rects(menu)
                .filter(|(i, _)| {
                    matches!(
                        m.lines[*i],
                        Line::Item(Kind::Close(_) | Kind::ForceClose, _)
                    )
                })
                .map(|(_, r)| r)
                .collect();
            assert_eq!(rows.len(), 2);
            let t = Instant::now();
            m.ask_force(t, (5, -50));
            let confirm = at(&m);
            assert_eq!(confirm.y + confirm.h, bottom);
            for r in &rows {
                for x in r.x..r.x + r.w {
                    for y in r.y..r.y + r.h {
                        assert_eq!(m.hit(confirm, x, y), None, "{windows}: ({x}, {y})");
                    }
                }
            }
            let c = m.confirm.clone().unwrap();
            assert!(!c.armed(t + ARM), "no motion yet");
            // navi's motion for the resize, at the same spot, doesn't arm it.
            m.pointer((5, -50));
            assert!(!m.confirm.as_ref().unwrap().armed(t + ARM));
            m.pointer((6, -50));
            let c = m.confirm.as_ref().unwrap();
            assert!(!c.armed(t + ARM - Duration::from_millis(1)));
            assert!(c.armed(t + ARM));
            // The window count is honest: a line only when there are others.
            let others = c.body().iter().any(|l| l.contains("other"));
            assert_eq!(others, windows > 1);
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
        assert!(!m.hover(Hit::Confirm));
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
}
