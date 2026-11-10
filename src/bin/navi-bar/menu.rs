//! Folder navigation, global search and keyboard selection in the app launcher.

use crate::desktop::{Category, Entry};

pub const MAX_QUERY: usize = 64;
pub const ROWS: usize = 8;
/// Wayland's usual wheel distance per step, also used for continuous touchpad input.
const SCROLL_PX: f64 = 15.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Folder(Category),
    App(usize),
}

#[derive(Debug, Default)]
pub struct Menu {
    query: String,
    folder: Option<Category>,
    matches: Vec<Row>,
    selected: usize,
    top: usize,
    scroll_pixels: f64,
}

impl Menu {
    pub fn new(entries: &[Entry]) -> Menu {
        let mut m = Menu::default();
        m.filter(entries);
        m
    }
    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn folder(&self) -> Option<Category> {
        self.folder
    }
    pub fn title(&self) -> &str {
        if !self.query.is_empty() {
            "Search results"
        } else {
            self.folder.map_or("Applications", Category::name)
        }
    }
    pub fn open(&mut self, folder: Category, entries: &[Entry]) {
        self.folder = Some(folder);
        self.query.clear();
        self.selected = 0;
        self.top = 0;
        self.filter(entries);
    }
    pub fn back(&mut self, entries: &[Entry]) -> bool {
        if self.folder.is_none() && self.query.is_empty() {
            return false;
        }
        self.folder = None;
        self.query.clear();
        self.selected = 0;
        self.top = 0;
        self.filter(entries);
        true
    }
    pub fn set_query(&mut self, q: &str, entries: &[Entry]) {
        self.query = q
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_QUERY)
            .collect();
        self.filter(entries);
    }
    pub fn push(&mut self, c: char, entries: &[Entry]) {
        self.set_query(&format!("{}{c}", self.query), entries);
    }
    pub fn pop(&mut self, entries: &[Entry]) {
        if self.query.is_empty() {
            self.back(entries);
            return;
        }
        let mut q = self.query.clone();
        q.pop();
        self.set_query(&q, entries);
    }
    fn filter(&mut self, entries: &[Entry]) {
        self.stop_scroll();
        let q = self.query.to_lowercase();
        self.matches = if q.is_empty() && self.folder.is_none() {
            let mut folders: Vec<Category> = entries.iter().map(|e| e.category).collect();
            folders.sort();
            folders.dedup();
            folders.into_iter().map(Row::Folder).collect()
        } else {
            (0..entries.len())
                .filter(|&i| {
                    let e = &entries[i];
                    if q.is_empty() {
                        Some(e.category) == self.folder
                    } else {
                        e.name.to_lowercase().contains(&q) || e.id.to_lowercase().contains(&q)
                    }
                })
                .map(Row::App)
                .collect()
        };
        self.select(self.selected as isize);
    }
    pub fn step(&mut self, by: isize) {
        self.stop_scroll();
        self.select(self.selected as isize + by);
    }
    /// PageUp/PageDown: a page of rows at a time.
    pub fn page(&mut self, pages: isize) {
        self.step(pages * ROWS as isize);
    }
    pub fn home(&mut self) {
        self.stop_scroll();
        self.select(0);
    }
    pub fn end(&mut self) {
        self.stop_scroll();
        self.select(isize::MAX);
    }
    fn select(&mut self, at: isize) {
        self.selected = (at.max(0) as usize).min(self.matches.len().saturating_sub(1));
        self.top = self.top.min(self.selected);
        if self.selected >= self.top + ROWS {
            self.top = self.selected + 1 - ROWS;
        }
        self.top = self.top.min(self.matches.len().saturating_sub(ROWS));
    }
    pub fn scroll(&mut self, by: isize) -> bool {
        let before = (self.top, self.selected);
        let max = self.matches.len().saturating_sub(ROWS);
        self.top = (self.top as isize)
            .saturating_add(by)
            .clamp(0, max as isize) as usize;
        self.selected = self
            .selected
            .clamp(self.top, (self.top + ROWS).saturating_sub(1));
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
        before != (self.top, self.selected)
    }

    /// Wheel notches apply once; fractional two-finger movement survives between frames.
    pub fn scroll_axis(&mut self, pixels: f64, discrete: i32, stop: bool) -> bool {
        let steps = if discrete != 0 {
            self.stop_scroll();
            discrete as isize
        } else {
            if !pixels.is_finite() {
                self.stop_scroll();
                return false;
            }
            self.scroll_pixels += pixels;
            let steps = (self.scroll_pixels / SCROLL_PX).trunc() as isize;
            self.scroll_pixels %= SCROLL_PX;
            steps
        };
        let changed = self.scroll(steps);
        let max = self.matches.len().saturating_sub(ROWS);
        if stop
            || (self.top == 0 && self.scroll_pixels < 0.0)
            || (self.top == max && self.scroll_pixels > 0.0)
        {
            self.stop_scroll();
        }
        changed
    }

    pub fn stop_scroll(&mut self) {
        self.scroll_pixels = 0.0;
    }
    pub fn scroll_pos(&self) -> (usize, usize) {
        (self.top, self.matches.len())
    }
    pub fn selected(&self) -> Option<Row> {
        self.matches.get(self.selected).copied()
    }
    pub fn rows(&self) -> impl Iterator<Item = (Row, bool)> + '_ {
        let end = (self.top + ROWS).min(self.matches.len());
        (self.top..end).map(|k| (self.matches[k], k == self.selected))
    }
    pub fn at_row(&self, row: usize) -> Option<Row> {
        (row < ROWS).then(|| self.matches.get(self.top + row).copied())?
    }

    /// Mouse hover and keyboard selection identify the same row.
    pub fn select_row(&mut self, row: usize) -> bool {
        if self.at_row(row).is_none() || self.selected == self.top + row {
            return false;
        }
        self.selected = self.top + row;
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn entries(names: &[&str]) -> Vec<Entry> {
        names
            .iter()
            .map(|n| Entry {
                id: format!("org.x.{}.desktop", n.to_lowercase().replace(' ', "")),
                name: n.to_string(),
                icon: None,
                argv: vec!["x".into()],
                wm_class: None,
                category: Category::Other,
            })
            .collect()
    }

    fn shown(m: &Menu, e: &[Entry]) -> Vec<String> {
        m.rows()
            .filter_map(|(r, _)| match r {
                Row::App(i) => Some(e[i].name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn ac4_search() {
        let e = entries(&["Dolphin", "Firefox", "Konsole", "Kate"]);
        let mut m = Menu::new(&e);
        m.open(Category::Other, &e);
        assert_eq!(shown(&m, &e).len(), 4, "empty lists all");
        m.set_query("kon", &e);
        assert_eq!(shown(&m, &e), ["Konsole"]);
        m.set_query("KON", &e);
        assert_eq!(shown(&m, &e), ["Konsole"]);
        m.set_query("org.x.dol", &e);
        assert_eq!(shown(&m, &e), ["Dolphin"], "by ID");
        m.set_query("zzz", &e);
        assert!(shown(&m, &e).is_empty());
        assert_eq!(m.selected(), None);
        m.set_query(&"a".repeat(65), &e);
        assert_eq!(m.query().len(), 64);
        m.set_query("", &e);
        m.push('K', &e);
        m.push('a', &e);
        assert_eq!(shown(&m, &e), ["Kate"]);
        m.pop(&e);
        assert_eq!(m.query(), "K");
    }

    #[test]
    fn ac4_selection_clamps_and_scrolls() {
        let names: Vec<String> = (0..15).map(|k| format!("App {k:02}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let e = entries(&names);
        let mut m = Menu::new(&e);
        m.open(Category::Other, &e);
        m.step(-1);
        assert_eq!(m.selected(), Some(Row::App(0)));
        m.step(7);
        assert_eq!(m.at_row(0), Some(Row::App(0)), "row 8 is still shown");
        m.step(1);
        assert_eq!(m.selected(), Some(Row::App(8)));
        assert_eq!(
            m.at_row(0),
            Some(Row::App(1)),
            "Down past the last row scrolls"
        );
        assert_eq!(m.rows().filter(|r| r.1).count(), 1);
        m.step(100);
        assert_eq!(
            (m.selected(), m.at_row(0)),
            (Some(Row::App(14)), Some(Row::App(7)))
        );
        assert_eq!(m.at_row(10), None);

        // A shrinking list clamps the selection.
        m.set_query("App 0", &e);
        assert_eq!(m.selected(), Some(Row::App(9)));
        assert_eq!(m.at_row(0), Some(Row::App(2)));
        m.set_query("App 03", &e);
        assert_eq!(m.selected(), Some(Row::App(3)));

        m.set_query("", &e);
        m.scroll(3);
        assert_eq!(
            (m.at_row(0), m.selected()),
            (Some(Row::App(3)), Some(Row::App(3)))
        );
        m.scroll(100);
        assert_eq!(m.at_row(0), Some(Row::App(7)));
        m.scroll(-100);
        assert_eq!(m.at_row(0), Some(Row::App(0)));
        assert!(m.select_row(1));
        assert_eq!(m.selected(), Some(Row::App(1)));
        assert!(!m.select_row(ROWS));
    }

    #[test]
    fn folders_open_go_back_and_search_across_categories() {
        let mut e = entries(&["Browser", "Editor", "Terminal"]);
        e[0].category = Category::Internet;
        e[1].category = Category::Development;
        e[2].category = Category::System;
        let mut m = Menu::new(&e);
        assert_eq!(
            m.rows().map(|r| r.0).collect::<Vec<_>>(),
            [
                Row::Folder(Category::Internet),
                Row::Folder(Category::Development),
                Row::Folder(Category::System)
            ]
        );
        assert!(!m.back(&e));
        m.open(Category::Internet, &e);
        assert_eq!(m.selected(), Some(Row::App(0)));
        assert_eq!(m.title(), "Internet");
        m.set_query("editor", &e);
        assert_eq!(
            m.selected(),
            Some(Row::App(1)),
            "search is global inside a folder"
        );
        assert!(m.back(&e));
        assert!(m.folder().is_none());
        assert_eq!(m.query(), "");
        assert_eq!(m.selected(), Some(Row::Folder(Category::Internet)));
    }

    #[test]
    fn wheel_and_small_two_finger_events_scroll_folders_and_apps() {
        let categories = [
            Category::Internet,
            Category::Development,
            Category::Office,
            Category::Graphics,
            Category::Media,
            Category::Games,
            Category::Education,
            Category::Science,
            Category::Settings,
            Category::System,
            Category::Utilities,
            Category::Other,
        ];
        let names: Vec<String> = (0..20).map(|i| format!("App {i:02}")).collect();
        let mut e = entries(&names.iter().map(String::as_str).collect::<Vec<_>>());
        for (i, entry) in e.iter_mut().enumerate() {
            entry.category = categories[i % categories.len()];
        }
        for search in [false, true] {
            let mut m = Menu::new(&e);
            if search {
                m.set_query("App", &e);
            }
            assert!(m.scroll_axis(15.0, 1, false));
            assert_eq!(m.scroll_pos().0, 1, "wheel delta isn't counted twice");
            assert!(m.scroll_axis(-15.0, -1, false));
            assert_eq!(m.scroll_pos().0, 0);
            for _ in 0..29 {
                assert!(!m.scroll_axis(0.5, 0, false));
            }
            assert!(m.scroll_axis(0.5, 0, false));
            assert_eq!(m.scroll_pos().0, 1, "small finger deltas accumulate");
            for _ in 0..30 {
                m.scroll_axis(-0.5, 0, false);
            }
            assert_eq!(m.scroll_pos().0, 0);
            m.scroll_axis(15_000.0, 1000, false);
            assert_eq!(m.scroll_pos().0, m.matches.len() - ROWS);
            m.scroll_axis(-15_000.0, -1000, false);
            assert_eq!(m.scroll_pos().0, 0);
        }
    }

    #[test]
    fn scroll_remainders_end_with_gestures_navigation_and_list_bounds() {
        let names: Vec<String> = (0..20).map(|i| format!("App {i:02}")).collect();
        let e = entries(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let mut m = Menu::new(&e);
        m.open(Category::Other, &e);
        m.scroll_axis(10.0, 0, false);
        m.scroll_axis(0.0, 0, true);
        assert!(!m.scroll_axis(5.0, 0, false));
        m.stop_scroll(); // Pointer leave.
        assert!(!m.scroll_axis(10.0, 0, false));
        m.set_query("App", &e);
        assert!(!m.scroll_axis(5.0, 0, false));
        m.scroll_axis(15.0, 1, false); // Changing to a wheel discards the finger remainder.
        assert!(!m.scroll_axis(10.0, 0, false));
        m.scroll(100);
        m.scroll_axis(10.0, 0, false); // Outward movement at the bottom cannot delay reversing.
        assert!(m.scroll_axis(-15.0, 0, false));
        assert_eq!(m.scroll_pos().0, 11);
        m.scroll(-100);
        assert!(!m.scroll_axis(-10.0, 0, false));
        assert!(m.scroll_axis(15.0, 0, false));
        assert_eq!(m.scroll_pos().0, 1);
        assert!(!m.scroll_axis(f64::NAN, 0, false));
        assert_eq!(m.scroll_pixels, 0.0);
    }

    #[test]
    fn page_home_and_end_move_by_pages_and_to_the_ends() {
        let names: Vec<String> = (0..20).map(|i| format!("App {i:02}")).collect();
        let e = entries(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let mut m = Menu::new(&e);
        m.open(Category::Other, &e);
        assert_eq!(ROWS, 8);
        m.page(1);
        assert_eq!(m.selected(), Some(Row::App(8)));
        assert_eq!(m.scroll_pos().0, 1, "PageDown scrolls");
        m.page(-1);
        assert_eq!((m.selected(), m.scroll_pos().0), (Some(Row::App(0)), 0));
        m.page(-1);
        assert_eq!((m.selected(), m.scroll_pos().0), (Some(Row::App(0)), 0));
        m.page(1);
        m.page(1);
        assert_eq!(m.selected(), Some(Row::App(16)));
        m.page(1);
        assert_eq!(m.selected(), Some(Row::App(19)), "clamps at the bottom");
        assert_eq!(m.scroll_pos().0, 12);
        m.home();
        assert_eq!((m.selected(), m.scroll_pos().0), (Some(Row::App(0)), 0));
        m.end();
        assert_eq!(
            (m.selected(), m.scroll_pos().0),
            (Some(Row::App(19)), 20 - ROWS)
        );

        m.set_query("zzz", &e);
        let empty = (m.selected(), m.scroll_pos());
        for key in [
            Menu::home,
            Menu::end,
            |m: &mut Menu| m.page(1),
            |m: &mut Menu| m.page(-1),
        ] {
            key(&mut m);
            assert_eq!((m.selected(), m.scroll_pos()), empty);
        }
        assert_eq!(empty, (None, (0, 0)));
    }
}
