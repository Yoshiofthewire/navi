//! The app menu's search and selection.

use crate::desktop::Entry;

pub const MAX_QUERY: usize = 64;
/// Rows the panel shows at once.
pub const ROWS: usize = 10;

#[derive(Debug, Default)]
pub struct Menu {
    query: String,
    /// Indexes into the entries, in list order.
    matches: Vec<usize>,
    /// Index into `matches`.
    selected: usize,
    /// The first match shown.
    top: usize,
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

    pub fn set_query(&mut self, q: &str, entries: &[Entry]) {
        self.query = q.chars().take(MAX_QUERY).collect();
        self.filter(entries);
    }

    pub fn push(&mut self, c: char, entries: &[Entry]) {
        let q = format!("{}{c}", self.query);
        self.set_query(&q, entries);
    }

    pub fn pop(&mut self, entries: &[Entry]) {
        let mut q = self.query.clone();
        q.pop();
        self.set_query(&q, entries);
    }

    /// Case-insensitive substring of the name or ID; an empty query matches all.
    fn filter(&mut self, entries: &[Entry]) {
        let q = self.query.to_lowercase();
        self.matches = (0..entries.len())
            .filter(|&i| {
                let e = &entries[i];
                e.name.to_lowercase().contains(&q) || e.id.to_lowercase().contains(&q)
            })
            .collect();
        self.select(self.selected as isize);
    }

    /// Moves the selection by `by` rows, clamped, scrolling to keep it shown.
    pub fn step(&mut self, by: isize) {
        self.select(self.selected as isize + by);
    }

    fn select(&mut self, at: isize) {
        let last = self.matches.len().saturating_sub(1);
        self.selected = (at.max(0) as usize).min(last);
        self.top = self.top.min(self.selected);
        if self.selected >= self.top + ROWS {
            self.top = self.selected + 1 - ROWS;
        }
        self.top = self.top.min(self.matches.len().saturating_sub(ROWS));
    }

    /// Scrolls the list by `by` rows without moving past either end; the selection follows.
    pub fn scroll(&mut self, by: isize) {
        let max = self.matches.len().saturating_sub(ROWS);
        self.top = (self.top as isize + by).clamp(0, max as isize) as usize;
        self.selected = self
            .selected
            .clamp(self.top, (self.top + ROWS).saturating_sub(1));
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    /// The entry selected, if any match.
    pub fn selected(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }

    /// The rows shown: (entry index, selected).
    pub fn rows(&self) -> impl Iterator<Item = (usize, bool)> + '_ {
        let end = (self.top + ROWS).min(self.matches.len());
        (self.top..end).map(|k| (self.matches[k], k == self.selected))
    }

    /// The entry in shown row `row`.
    pub fn at_row(&self, row: usize) -> Option<usize> {
        (row < ROWS).then(|| self.matches.get(self.top + row).copied())?
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
            })
            .collect()
    }

    fn shown(m: &Menu, e: &[Entry]) -> Vec<String> {
        m.rows().map(|(i, _)| e[i].name.clone()).collect()
    }

    #[test]
    fn ac4_search() {
        let e = entries(&["Dolphin", "Firefox", "Konsole", "Kate"]);
        let mut m = Menu::new(&e);
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
        m.step(-1);
        assert_eq!(m.selected(), Some(0));
        m.step(9);
        assert_eq!(m.at_row(0), Some(0), "row 10 is still shown");
        m.step(1);
        assert_eq!(m.selected(), Some(10));
        assert_eq!(m.at_row(0), Some(1), "Down past the last row scrolls");
        assert_eq!(m.rows().filter(|r| r.1).count(), 1);
        m.step(100);
        assert_eq!((m.selected(), m.at_row(0)), (Some(14), Some(5)));
        assert_eq!(m.at_row(10), None);

        // A shrinking list clamps the selection.
        m.set_query("App 0", &e);
        assert_eq!(m.selected(), Some(9));
        assert_eq!(m.at_row(0), Some(0));
        m.set_query("App 03", &e);
        assert_eq!(m.selected(), Some(3));

        m.set_query("", &e);
        m.scroll(3);
        assert_eq!((m.at_row(0), m.selected()), (Some(3), Some(3)));
        m.scroll(100);
        assert_eq!(m.at_row(0), Some(5));
        m.scroll(-100);
        assert_eq!(m.at_row(0), Some(0));
    }
}
