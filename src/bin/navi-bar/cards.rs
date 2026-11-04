//! Where notification cards draw on their surface, and which one a click hits. Surface-local
//! logical px; the surface sits at the output's top-left, below the status band.

use crate::layout::{Item, Rect};
use crate::notify::Note;
use crate::theme::{ACCENT, BG, DIM, INK};

pub const CARD_W: i32 = 360;
const GAP: i32 = 8;
const PAD: i32 = 8;
pub const LINE_H: i32 = 20;
const BODY_LINES: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: u32,
    pub rect: Rect,
    app: String,
    summary: String,
    body: Vec<String>,
}

/// One card per note, in the order given (newest first), stacked down from the top. `advance` is
/// a glyph's width in px, `None` for one the font lacks.
pub fn layout(notes: &[Note], advance: &impl Fn(char) -> Option<f32>) -> Vec<Card> {
    let text_w = (CARD_W - 2 * PAD) as f32;
    let mut y = 0;
    notes
        .iter()
        .map(|n| {
            let body = match n.body.is_empty() {
                true => Vec::new(),
                false => wrap(&n.body, text_w, BODY_LINES, advance),
            };
            let h = 2 * PAD + LINE_H * (2 + body.len() as i32);
            let rect = Rect {
                x: 0,
                y,
                w: CARD_W,
                h,
            };
            y += h + GAP;
            Card {
                id: n.id,
                rect,
                app: n.app.clone(),
                summary: n.summary.replace('\n', " "),
                body,
            }
        })
        .collect()
}

/// The surface size that holds `cards`.
pub fn size(cards: &[Card]) -> (i32, i32) {
    let h = cards.last().map_or(0, |c| c.rect.y + c.rect.h);
    (CARD_W, h)
}

pub fn hit(cards: &[Card], x: i32, y: i32) -> Option<u32> {
    let inside = |r: Rect| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
    cards.iter().find(|c| inside(c.rect)).map(|c| c.id)
}

/// Back to front.
pub fn draw(cards: &[Card]) -> Vec<Item> {
    let mut out = Vec::new();
    for c in cards {
        let r = c.rect;
        out.push(Item::Fill(r, ACCENT));
        out.push(Item::Fill(
            Rect {
                x: r.x + 1,
                y: r.y + 1,
                w: r.w - 2,
                h: r.h - 2,
            },
            BG,
        ));
        let line = |k: i32| Rect {
            x: r.x + PAD,
            y: r.y + PAD + k * LINE_H,
            w: r.w - 2 * PAD,
            h: LINE_H,
        };
        out.push(Item::Text(c.app.clone(), line(0), DIM));
        out.push(Item::Text(c.summary.clone(), line(1), INK));
        for (k, l) in c.body.iter().enumerate() {
            out.push(Item::Text(l.clone(), line(2 + k as i32), INK));
        }
    }
    out
}

/// `text` in lines of at most `max` px, broken at spaces (or anywhere in a word too long for a
/// line) and at `\n`. Past `lines`, the last line ends with `…`.
pub fn wrap(
    text: &str,
    max: f32,
    lines: usize,
    advance: &impl Fn(char) -> Option<f32>,
) -> Vec<String> {
    let w = |s: &str| s.chars().filter_map(advance).sum::<f32>();
    let mut out: Vec<String> = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let joined = match line.is_empty() {
                true => word.to_string(),
                false => format!("{line} {word}"),
            };
            if w(&joined) <= max {
                line = joined;
                continue;
            }
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            for c in word.chars() {
                if !line.is_empty() && w(&line) + advance(c).unwrap_or(0.0) > max {
                    out.push(std::mem::take(&mut line));
                }
                line.push(c);
            }
        }
        out.push(line);
    }
    if out.len() > lines {
        out.truncate(lines);
        if let Some(last) = out.last_mut() {
            let ell = advance('…').unwrap_or(0.0);
            while !last.is_empty() && w(last) + ell > max {
                last.pop();
            }
            last.push('…');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every glyph 10 px: 34 per card line.
    fn adv(_: char) -> Option<f32> {
        Some(10.0)
    }

    fn note(id: u32, body: &str) -> Note {
        Note {
            id,
            sender: ":1.1".into(),
            app: "App".into(),
            summary: "Hi".into(),
            body: body.into(),
            expires_ms: 0,
        }
    }

    #[test]
    fn ac9_stacked_newest_first() {
        let one_line = 2 * PAD + 3 * LINE_H;
        for n in 1..=3 {
            let notes: Vec<Note> = (0..n).map(|k| note(10 - k, "body")).collect();
            let cards = layout(&notes, &adv);
            let rects: Vec<Rect> = cards.iter().map(|c| c.rect).collect();
            let want: Vec<Rect> = (0..n as i32)
                .map(|k| Rect {
                    x: 0,
                    y: k * (one_line + GAP),
                    w: 360,
                    h: one_line,
                })
                .collect();
            assert_eq!(rects, want);
            assert_eq!(cards[0].id, 10, "the newest on top");
            assert_eq!(
                size(&cards),
                (360, n as i32 * one_line + (n as i32 - 1) * GAP)
            );
        }
        // No body: app name and summary only.
        assert_eq!(layout(&[note(1, "")], &adv)[0].rect.h, 2 * PAD + 2 * LINE_H);
    }

    #[test]
    fn ac9_body_wraps_to_three_lines() {
        let body = "word ".repeat(40);
        let lines = wrap(&body, 344.0, 3, &adv);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.chars().count() <= 34), "{lines:?}");
        assert!(lines[2].ends_with('…'));
        assert!(!lines[1].ends_with('…'));
        assert_eq!(wrap("a b\nc", 344.0, 3, &adv), ["a b", "c"]);
        // A word longer than a line breaks inside it.
        let long = wrap(&"x".repeat(50), 344.0, 3, &adv);
        assert_eq!(long.iter().map(|l| l.len()).collect::<Vec<_>>(), [34, 16]);
        let cards = layout(&[note(1, &body)], &adv);
        assert_eq!(cards[0].rect.h, 2 * PAD + 5 * LINE_H);
        let items = draw(&cards);
        assert!(items.contains(&Item::Text(
            "App".into(),
            Rect {
                x: 8,
                y: 8,
                w: 344,
                h: 20
            },
            DIM
        )));
        assert!(
            items.contains(&Item::Fill(cards[0].rect, ACCENT)),
            "the outline"
        );
    }

    #[test]
    fn ac9_hits() {
        let notes = [note(3, "x"), note(2, "x"), note(1, "x")];
        let cards = layout(&notes, &adv);
        let c2 = cards[1].rect;
        assert_eq!(hit(&cards, c2.x + 5, c2.y + 5), Some(2));
        assert_eq!(hit(&cards, 0, c2.y + c2.h - 1), Some(2));
        assert_eq!(hit(&cards, 10, c2.y - 1), None, "the gap above card 2");
        assert_eq!(hit(&cards, 10, c2.y + c2.h), None, "the gap below it");
        assert_eq!(hit(&cards, 360, c2.y + 5), None);
    }
}
