//! Memory as text, for `persist` to save and load. The file is untrusted on read: every line is
//! checked against the grid and a px bound before it reaches the layout.
//!
//! ```text
//! navi-memory 1
//! <app_id> <monitor> <col> <row> <cols> <rows> <w> <h>
//! ```
//!
//! One line per app, most recently placed, moved or resized first.

use std::collections::HashMap;

use super::{COLS, Cells, Layout, Memory, ROWS};

const HEADER: &str = "navi-memory 1";
const MAX_ENTRIES: usize = 512;
const MAX_NAME: usize = 255;
const MAX_PX: i32 = 65535;

/// Printable ASCII without whitespace, so a name is one field on one line and no escape.
fn safe(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_graphic())
}

fn valid(app: &str, m: &Memory) -> bool {
    let c = m.cells;
    let px = 1..=MAX_PX;
    safe(app)
        && safe(&m.output)
        && c.col >= 0
        && c.row >= 0
        && c.cols >= 1
        && c.rows >= 1
        && c.col <= COLS - c.cols
        && c.row <= ROWS - c.rows
        && px.contains(&m.size.0)
        && px.contains(&m.size.1)
}

/// One line, or `None` if any field is missing, extra or out of range.
fn entry(line: &str, stamp: u64) -> Option<(String, Memory)> {
    let f: Vec<&str> = line.split(' ').collect();
    let [app, output, nums @ ..] = f.as_slice() else {
        return None;
    };
    let n: Vec<i32> = nums.iter().map(|s| s.parse().ok()).collect::<Option<_>>()?;
    let [col, row, cols, rows, w, h] = n[..] else {
        return None;
    };
    let m = Memory {
        output: output.to_string(),
        cells: Cells {
            col,
            row,
            cols,
            rows,
        },
        size: (w, h),
        stamp,
    };
    valid(app, &m).then(|| (app.to_string(), m))
}

/// The entries kept, at most `MAX_ENTRIES`, and how many lines were bad. A repeated app: the
/// later line wins. `Err` names why the whole text is ignored.
fn parse(text: &str) -> Result<(HashMap<String, Memory>, usize), &'static str> {
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Err("not a navi-memory 1 file");
    }
    let lines: Vec<&str> = lines.collect();
    let mut memory = HashMap::new();
    let mut skipped = 0;
    for (i, line) in lines.iter().enumerate() {
        // The first line is the most recent.
        match entry(line, (lines.len() - i) as u64) {
            Some((app, m)) if memory.len() < MAX_ENTRIES || memory.contains_key(&app) => {
                memory.insert(app, m);
            }
            Some(_) => {}
            None => skipped += 1,
        }
    }
    Ok((memory, skipped))
}

impl Layout {
    /// The newest `MAX_ENTRIES` memories that fit the format; the rest stay in RAM only.
    pub fn memory_text(&self) -> String {
        let mut apps: Vec<(&String, &Memory)> =
            self.memory.iter().filter(|(a, m)| valid(a, m)).collect();
        apps.sort_by(|a, b| b.1.stamp.cmp(&a.1.stamp).then(a.0.cmp(b.0)));
        let mut text = format!("{HEADER}\n");
        for (app, m) in apps.into_iter().take(MAX_ENTRIES) {
            let (c, (w, h)) = (m.cells, m.size);
            text += &format!(
                "{app} {} {} {} {} {} {w} {h}\n",
                m.output, c.col, c.row, c.cols, c.rows
            );
        }
        text
    }

    /// Replaces memory with `text`'s. Returns the entries kept and lines skipped.
    pub fn load_memory(&mut self, text: &str) -> Result<(usize, usize), &'static str> {
        let (memory, skipped) = parse(text)?;
        let newest = memory.values().map(|m| m.stamp).max().unwrap_or(0);
        self.clock = self.clock.max(newest);
        self.memory = memory;
        Ok((self.memory.len(), skipped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Output, Rect, Step};

    fn uw() -> Output {
        Output {
            name: "uw".into(),
            area: Rect {
                x: 0,
                y: 0,
                w: 3440,
                h: 1440,
            },
            full: Rect {
                x: 0,
                y: 0,
                w: 3440,
                h: 1440,
            },
            priority: 0,
        }
    }

    fn layout() -> Layout {
        let mut l = Layout::default();
        l.set_outputs(vec![uw()]);
        l
    }

    /// Memory without the stamps, which only order it.
    fn plain(l: &Layout) -> HashMap<String, (String, Cells, (i32, i32))> {
        let m = l.memory.iter();
        m.map(|(a, m)| (a.clone(), (m.output.clone(), m.cells, m.size)))
            .collect()
    }

    fn line(app: &str) -> String {
        format!("{app} uw 0 0 6 6 1720 1440")
    }

    #[test]
    fn round_trip_restores_spots_and_sizes() {
        let mut l = layout();
        let mut tabbed = None;
        for n in 1..=7 {
            let app = format!("app{n}");
            if let (_, Step::Tabbed(_)) = l.place(&app, (3400, 10)).unwrap() {
                tabbed = Some(app);
            }
        }
        // The 7th only learned its size: the grid was full.
        assert_eq!(tabbed.as_deref(), Some("app7"));
        l.memory.get_mut("app1").unwrap().size = (1144, 1440);

        let text = l.memory_text();
        let mut back = layout();
        assert_eq!(back.load_memory(&text), Ok((7, 0)));
        assert_eq!(plain(&back), plain(&l));
        assert_eq!(back.memory_text(), text);

        for app in ["app1", "app7"] {
            let mut fresh = layout();
            fresh.load_memory(&text).unwrap();
            let (id, step) = fresh.place(app, (3400, 700)).unwrap();
            assert_eq!(step, Step::Remembered, "{app}");
            assert_eq!(fresh.tile(id).unwrap().cells, l.memory[app].cells, "{app}");
            assert_eq!(fresh.memory[app].size, l.memory[app].size, "{app}");
        }
    }

    #[test]
    fn format_is_versioned_ordered_and_deterministic() {
        let mut l = layout();
        l.place("a", (10, 10)).unwrap();
        l.place("b", (3400, 10)).unwrap();
        let text = l.memory_text();
        assert_eq!(
            text,
            "navi-memory 1\nb uw 6 0 6 6 1720 1440\na uw 0 0 6 6 1720 1440\n"
        );
        assert_eq!(l.memory_text(), text);
    }

    #[test]
    fn bad_files_give_no_entries() {
        let garbage: Vec<u8> = (0u32..4096)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        for text in [
            String::new(),
            "\n".into(),
            "navi-memory\n".into(),
            "navi-memory 2\n".to_string() + &line("a"),
            line("a") + "\n",
            String::from_utf8_lossy(&garbage).into_owned(),
            "x".repeat(1 << 20),
        ] {
            assert!(parse(&text).is_err(), "{:?}", &text[..text.len().min(40)]);
        }
    }

    #[test]
    fn bad_lines_are_skipped_and_counted() {
        let bad = [
            "a uw 0 0 6 6 1720".to_string(),
            "a uw 0 0 6 6 1720 1440 9".into(),
            "a uw x 0 6 6 1720 1440".into(),
            "a uw 0 0 6 6 1720 1.5".into(),
            "a  uw 0 0 6 6 1720 1440".into(),
            "a uw -1 0 6 6 1720 1440".into(),
            "a uw 0 -1 6 6 1720 1440".into(),
            "a uw 0 0 0 6 1720 1440".into(),
            "a uw 0 0 6 0 1720 1440".into(),
            "a uw 0 0 -6 6 1720 1440".into(),
            "a uw 11 0 2 6 1720 1440".into(),
            "a uw 0 5 6 2 1720 1440".into(),
            "a uw 2147483647 0 1 1 1720 1440".into(),
            "a uw 0 0 6 6 0 1440".into(),
            "a uw 0 0 6 6 1720 0".into(),
            "a uw 0 0 6 6 65536 1440".into(),
            "a uw 0 0 6 6 1720 99999999999".into(),
            "a\u{7}b uw 0 0 6 6 1720 1440".into(),
            "a uw\u{1b}[31m 0 0 6 6 1720 1440".into(),
            line(&"a".repeat(256)),
            line("caf\u{e9}"),
            "y".repeat(1 << 20),
        ];
        let mut text = format!("{HEADER}\n{}\n", line("first"));
        for b in &bad {
            text += &format!("{b}\n");
        }
        let edge = format!("{} uw 0 0 12 6 65535 65535", "z".repeat(255));
        text += &format!("{edge}\n{}\n", line("last"));

        let (memory, skipped) = parse(&text).unwrap();
        assert_eq!(skipped, bad.len());
        let mut kept: Vec<&str> = memory.keys().map(String::as_str).collect();
        kept.sort_unstable();
        assert_eq!(kept, ["first", "last", &"z".repeat(255)]);
    }

    #[test]
    fn later_duplicate_wins() {
        let text = format!("{HEADER}\n{}\na uw 6 0 6 6 1720 1440\n", line("a"));
        let (memory, _) = parse(&text).unwrap();
        assert_eq!(memory["a"].cells.col, 6);
    }

    #[test]
    fn unsafe_ids_stay_in_ram_only() {
        let mut l = layout();
        for app in ["my app", "tab\tapp", "new\nline", "caf\u{e9}", "ok"] {
            l.place(app, (10, 10)).unwrap();
        }
        assert_eq!(l.memory.len(), 5);
        let text = l.memory_text();
        assert_eq!(text.lines().count(), 2);
        assert!(text.lines().nth(1).unwrap().starts_with("ok "));
        let (memory, skipped) = parse(&text).unwrap();
        assert_eq!((memory.len(), skipped), (1, 0));
    }

    #[test]
    fn cap_keeps_the_most_recent() {
        let mut l = layout();
        for n in 0..600 {
            l.place(&format!("app{n}"), (3400, 10)).unwrap();
        }
        let text = l.memory_text();
        let apps: Vec<&str> = text
            .lines()
            .skip(1)
            .map(|s| s.split(' ').next().unwrap())
            .collect();
        assert_eq!(apps.len(), MAX_ENTRIES);
        let newest: Vec<String> = (88..600).rev().map(|n| format!("app{n}")).collect();
        assert_eq!(apps, newest);

        let mut text = format!("{HEADER}\n");
        for n in 0..600 {
            text += &format!("{}\n", line(&format!("app{n}")));
        }
        let (memory, skipped) = parse(&text).unwrap();
        assert_eq!((memory.len(), skipped), (MAX_ENTRIES, 0));
        assert!(memory.contains_key("app511") && !memory.contains_key("app512"));
    }
}
