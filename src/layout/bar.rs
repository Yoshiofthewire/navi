//! The bar above tabbed and server-decorated tiles: tabs, window buttons, and how they're drawn.

use super::{BAR, Layout, Rect, WinId, drag::Hit};

const TAB_GAP: i32 = 2;
/// The glyph box inside a button, in px.
const GLYPH: i32 = 10;

pub const BAR_BG: u32 = 0x151922;
pub const TAB_BG: u32 = 0x262C38;
pub const ACCENT: u32 = 0x2BA3D8;
pub const INK: u32 = 0xD0D4DC;
pub const INK_ON_ACCENT: u32 = 0x0B0D12;
/// Minimized tiles' strips and tabs are drawn this opaque.
pub const DIM: f32 = 0.4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Minimize,
    Maximize,
    Close,
}

#[derive(Debug, PartialEq)]
pub struct Bar {
    /// The gap above the window, full window width.
    pub rect: Rect,
    /// One per member of a tabbed group, by id.
    pub tabs: Vec<(WinId, Rect)>,
    /// Flush right, when the active tile is decorated and not minimized.
    pub buttons: Vec<(Button, Rect)>,
}

impl Layout {
    pub fn bar(&self, id: WinId) -> Option<Bar> {
        if !self.has_bar(id) {
            return None;
        }
        let (c, r) = self.frame(id)?;
        let rect = Rect {
            x: r.x,
            y: c.y,
            w: r.w,
            h: r.y - c.y,
        };
        let active = self.tile(self.active(id)?)?;
        let right = rect.x + rect.w;
        let all: &[Button] = match active.decorated && !active.minimized {
            true => &[Button::Minimize, Button::Maximize, Button::Close],
            false => &[],
        };
        let buttons: Vec<(Button, Rect)> = all
            .iter()
            .zip([3, 2, 1])
            .map(|(&b, k)| {
                let x = right - k * BAR;
                (b, Rect { x, w: BAR, ..rect })
            })
            .collect();
        let group = self.group(id);
        let tabs = match group.len() {
            1 => Vec::new(),
            _ => {
                let w = rect.w - BAR * buttons.len() as i32;
                partition(Rect { w, ..rect }, &group)
            }
        };
        Some(Bar {
            rect,
            tabs,
            buttons,
        })
    }
}

/// Equal tabs with `TAB_GAP` between them; the last takes the remainder.
fn partition(area: Rect, ids: &[WinId]) -> Vec<(WinId, Rect)> {
    let n = i32::try_from(ids.len()).unwrap_or(i32::MAX).max(1);
    let w = (area.w.saturating_sub(TAB_GAP.saturating_mul(n - 1)) / n).max(1);
    let end = area.x.saturating_add(area.w);
    ids.iter()
        .zip(0..)
        .map(|(&id, k): (&WinId, i32)| {
            let x = area.x.saturating_add(k.saturating_mul(w + TAB_GAP));
            let w = if k == n - 1 { (end - x).max(1) } else { w };
            (id, Rect { x, w, ..area })
        })
        .collect()
}

/// `hex` at `alpha`, premultiplied.
pub fn rgba(hex: u32, alpha: f32) -> [f32; 4] {
    let [_, r, g, b] = hex.to_be_bytes().map(|c| f32::from(c) / 255.0 * alpha);
    [r, g, b, alpha]
}

/// The rects of a button's glyph, centred in `r`.
fn glyph(b: Button, r: Rect) -> Vec<Rect> {
    let (x, y) = (r.x + (r.w - GLYPH) / 2, r.y + (r.h - GLYPH) / 2);
    let at = |dx: i32, dy: i32, w: i32, h: i32| Rect {
        x: x + dx,
        y: y + dy,
        w,
        h,
    };
    match b {
        Button::Minimize => vec![at(0, GLYPH - 2, GLYPH, 2)],
        Button::Maximize => vec![
            at(0, 0, GLYPH, 2),
            at(0, 0, 1, GLYPH),
            at(GLYPH - 1, 0, 1, GLYPH),
            at(0, GLYPH - 1, GLYPH, 1),
        ],
        // Two diagonals of 2 px squares.
        Button::Close => (0..GLYPH / 2)
            .flat_map(|k| [at(2 * k, 2 * k, 2, 2), at(GLYPH - 2 - 2 * k, 2 * k, 2, 2)])
            .collect(),
    }
}

/// Front to back: `bar` with `hover` (a tab or button id) lit.
pub fn draw(l: &Layout, id: WinId, bar: &Bar, hover: Option<Hit>) -> Vec<(Rect, [f32; 4])> {
    let mut out = Vec::new();
    for &(b, r) in &bar.buttons {
        let lit = hover == Some(Hit::Button(id, b));
        let ink = if lit { INK_ON_ACCENT } else { INK };
        out.extend(glyph(b, r).into_iter().map(|g| (g, rgba(ink, 1.0))));
        if lit {
            out.push((r, rgba(ACCENT, 1.0)));
        }
    }
    let active = l.active(id);
    for &(t, r) in &bar.tabs {
        let minimized = l.tile(t).is_some_and(|t| t.minimized);
        let color = match (Some(t) == active && !minimized) || hover == Some(Hit::Tab(t)) {
            true => rgba(ACCENT, 1.0),
            false if minimized => rgba(TAB_BG, DIM),
            false => rgba(TAB_BG, 1.0),
        };
        out.push((r, color));
    }
    out.push((bar.rect, rgba(BAR_BG, 1.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::super::{
        Cells, FULL, GAP, MIN_H, MIN_W, Output, Tile,
        drag::{
            Done, Drag, overlay,
            tests::{assert_valid, at, c, drag, uw, with},
        },
    };
    use super::*;

    fn out(name: &str, w: i32, h: i32) -> Output {
        Output {
            name: name.into(),
            area: Rect { x: 0, y: 0, w, h },
            priority: 0,
        }
    }

    /// `n` tiles tabbed at `cells` on `o`; the last is active.
    fn group_on(o: Output, cells: Cells, n: usize) -> Layout {
        let mut l = Layout::default();
        let name = o.name.clone();
        l.set_outputs(vec![o]);
        for i in 0..n {
            l.tiles.push(Tile {
                id: i as WinId,
                app: format!("a{i}"),
                output: name.clone(),
                cells,
                stamp: i as u64,
                ..Tile::default()
            });
        }
        l.clock = n as u64;
        l.next_id = n as WinId;
        l
    }

    fn mid(r: Rect) -> (i32, i32) {
        (r.x + r.w / 2, r.y + r.h / 2)
    }

    fn tab(l: &Layout, id: WinId) -> Rect {
        l.zone(Hit::Tab(id)).expect("a tab")
    }

    fn button(l: &Layout, id: WinId, b: Button) -> Rect {
        l.zone(Hit::Button(id, b)).expect("a button")
    }

    /// AC9: visible tiles don't overlap, a group shows at most one tile, cells stay valid.
    fn assert_invariants(l: &Layout) {
        assert_valid(l);
        let shown: Vec<&Tile> = l.tiles.iter().filter(|t| l.visible(t.id)).collect();
        for (i, t) in shown.iter().enumerate() {
            for u in &shown[i + 1..] {
                assert!(!t.tabbed(u), "{} and {} both shown", t.id, u.id);
                if t.output == u.output {
                    let (a, b) = (l.rect(t.id).unwrap(), l.rect(u.id).unwrap());
                    let apart = a.x + a.w <= b.x
                        || b.x + b.w <= a.x
                        || a.y + a.h <= b.y
                        || b.y + b.h <= a.y;
                    assert!(apart, "{a:?} overlaps {b:?}");
                }
            }
        }
    }

    /// Press at `p`, move 7 px, release: a click.
    fn click(l: &mut Layout, p: (i32, i32)) -> Option<&'static str> {
        let to = (p.0 + 7, p.1);
        let what = drag(l, p, &[(to, 0)]);
        assert_invariants(l);
        what
    }

    fn click_tab(l: &mut Layout, id: WinId) -> Option<&'static str> {
        let p = mid(tab(l, id));
        click(l, p)
    }

    fn click_button(l: &mut Layout, id: WinId, b: Button) -> Option<&'static str> {
        let p = mid(button(l, id, b));
        click(l, p)
    }

    /// Every zone hit on a 4 px lattice over the ultrawide.
    fn hits(l: &Layout) -> Vec<Hit> {
        (0..3440)
            .step_by(4)
            .flat_map(|x| (0..1440).step_by(4).map(move |y| (x, y)))
            .filter_map(|p| l.hit(p))
            .collect()
    }

    const HALF_L: Cells = Cells {
        col: 0,
        row: 0,
        cols: 6,
        rows: 6,
    };
    const HALF_R: Cells = Cells {
        col: 6,
        row: 0,
        cols: 6,
        rows: 6,
    };

    #[test]
    fn ac1_bar_geometry() {
        let o = uw();
        let (cr, plain) = (o.cells_rect(HALF_L), o.tile_rect(HALF_L));
        let mut l = with(&[HALF_L]);
        assert_eq!(l.rect(0), Some(plain));
        assert_eq!(plain.y, cr.y + GAP);
        l.set_decorated(0, true);
        let r = l.rect(0).unwrap();
        assert_eq!(r.y, cr.y + BAR);
        assert_eq!(r.h, plain.h - (BAR - GAP));
        assert_eq!((r.x, r.w), (plain.x, plain.w));

        let mut l = with(&[HALF_L, HALF_L]);
        assert_eq!(l.rect(0), l.rect(1));
        assert_eq!(l.rect(0).unwrap().y, cr.y + BAR);
        l.remove(1);
        assert_eq!(l.rect(0), Some(plain));
    }

    #[test]
    fn ac2_bar_hit_zones() {
        let mut l = with(&[HALF_L]);
        l.set_decorated(0, true);
        let r = l.rect(0).unwrap();
        let right = r.x + r.w;
        for (k, b) in [
            (3, Button::Minimize),
            (2, Button::Maximize),
            (1, Button::Close),
        ] {
            let x = right - k * BAR;
            for p in [(x, 0), (x + BAR - 1, BAR - 1), (x + 12, 12)] {
                assert_eq!(l.hit(p), Some(Hit::Button(0, b)), "{p:?}");
            }
        }
        assert_eq!(l.hit((right - 3 * BAR - 1, 12)), Some(Hit::Strip(0)));
        assert_eq!(l.hit((r.x, 0)), Some(Hit::Strip(0)));
        assert_eq!(l.hit((500, 700)), None);
        assert_eq!(l.hit((500, BAR)), None);

        // Tabs in id order with 2 px gaps; buttons only once the active tile is decorated.
        let mut l = with(&[HALF_L, HALF_L]);
        let bar = l.bar(1).unwrap();
        assert!(bar.buttons.is_empty());
        let [(a, ta), (b, tb)] = bar.tabs[..] else {
            panic!("two tabs");
        };
        assert_eq!((a, b), (0, 1));
        assert_eq!(ta.x, bar.rect.x);
        assert_eq!(tb.x, ta.x + ta.w + TAB_GAP);
        assert_eq!(tb.x + tb.w, bar.rect.x + bar.rect.w);
        for x in bar.rect.x..bar.rect.x + bar.rect.w {
            let want = match x {
                _ if x < ta.x + ta.w => Some(Hit::Tab(0)),
                _ if x < tb.x => None,
                _ => Some(Hit::Tab(1)),
            };
            assert_eq!(l.hit((x, 5)), want, "x {x}");
        }
        l.set_decorated(0, true);
        assert!(
            l.bar(1).unwrap().buttons.is_empty(),
            "0 isn't the active tab"
        );
        l.set_decorated(1, true);
        let bar = l.bar(1).unwrap();
        assert_eq!(bar.buttons.len(), 3);
        assert_eq!(bar.tabs[1].1.x + bar.tabs[1].1.w, bar.buttons[0].1.x);
        assert_eq!(
            l.hit(mid(bar.buttons[2].1)),
            Some(Hit::Button(1, Button::Close))
        );
    }

    #[test]
    fn ac3_tab_partition() {
        for o in [uw(), out("laptop", 1800, 1125)] {
            let (cols, rows) = o.span_for((MIN_W, MIN_H));
            let cells = c(0, 0, cols, rows);
            let area = o.tile_rect(cells);
            for n in 1..=12 {
                let ids: Vec<WinId> = (0..n).collect();
                let tabs = partition(area, &ids);
                assert_eq!(tabs.len(), n as usize);
                assert_eq!(tabs[0].1.x, area.x);
                for (k, &(id, r)) in tabs.iter().enumerate() {
                    assert_eq!(id, k as WinId);
                    assert!(r.w >= 1, "{} n {n}: {r:?}", o.name);
                    if let Some(&(_, next)) = tabs.get(k + 1) {
                        assert_eq!(r.x + r.w + TAB_GAP, next.x, "{} n {n}", o.name);
                    }
                }
                let last = tabs.last().unwrap().1;
                assert_eq!(last.x + last.w, area.x + area.w, "{} n {n}", o.name);
                if n > 1 {
                    let l = group_on(o.clone(), cells, n as usize);
                    let bar = l.bar(0).unwrap();
                    assert_eq!(
                        bar.tabs,
                        partition(
                            Rect {
                                y: 0,
                                h: BAR,
                                ..area
                            },
                            &ids
                        )
                    );
                }
            }
        }
        let mut l = group_on(out("tiny", 10, 5), FULL, 1000);
        l.set_decorated(999, true);
        for p in [(0, 0), (5, 2), (9, 4)] {
            l.hit(p);
            overlay(&l, None, p, 0);
            if let Some(mut d) = Drag::press(&l, p) {
                d.motion(&mut l, (9, 4), 0);
                d.release(&mut l, (9, 4), 0);
            }
        }
    }

    #[test]
    fn ac4_click_to_switch() {
        let mut l = with(&[HALF_L, HALF_L]);
        let (tiles, memory) = (l.tiles.clone(), l.memory.clone());
        assert!(l.visible(1) && !l.visible(0));
        assert_eq!(click_tab(&mut l, 0), Some("tab switched"));
        assert!(l.visible(0) && !l.visible(1));
        assert_eq!(click_tab(&mut l, 0), None, "already shown");
        let cells = |ts: &[Tile]| ts.iter().map(|t| t.cells).collect::<Vec<_>>();
        assert_eq!(cells(&l.tiles), cells(&tiles));
        assert_eq!(l.memory, memory);
    }

    #[test]
    fn ac5_untab() {
        let mut l = with(&[HALF_L, HALF_L]);
        let size = l.memory["a1"].size;
        let from = mid(tab(&l, 1));
        assert_eq!(drag(&mut l, from, &[((3000, 700), 0)]), Some("untabbed"));
        assert_invariants(&l);
        assert_eq!(at(&l, 1), HALF_R);
        assert_eq!(at(&l, 0), HALF_L);
        assert!(l.visible(0) && l.visible(1));
        assert_eq!(l.rect(0), Some(uw().tile_rect(HALF_L)));
        assert_eq!(l.memory["a1"].cells, HALF_R);
        assert_eq!(l.memory["a1"].size, size);

        // Its own group's cells: nothing changes.
        let mut l = with(&[HALF_L, HALF_L]);
        let tiles = l.tiles.clone();
        assert_eq!(
            drag(&mut l, from, &[((3000, 700), 0), ((800, 700), 10)]),
            None
        );
        assert_eq!(l.tiles, tiles);

        // Over another tile: only a hold tabs, and nothing is previewed before it.
        let previews = |l: &Layout, d: &Drag, t| {
            overlay(l, Some(d), (2500, 700), t)
                .into_iter()
                .filter(|&(_, k)| k == rgba(ACCENT, 0.35) || k == rgba(ACCENT, 0.7))
                .count()
        };
        let mut l = with(&[HALF_L, HALF_L, HALF_R]);
        let tiles = l.tiles.clone();
        let mut d = Drag::press(&l, from).unwrap();
        d.motion(&mut l, (2500, 700), 0);
        d.motion(&mut l, (2500, 700), 499);
        assert_eq!(previews(&l, &d, 499), 0);
        assert_eq!(previews(&l, &d, 500), 1);
        assert!(d.release(&mut l, (2500, 700), 499).is_none());
        assert_eq!(l.tiles, tiles);

        let path = [((2500, 700), 0), ((2500, 700), 500)];
        assert_eq!(drag(&mut l, from, &path), Some("tabbed"));
        assert_invariants(&l);
        assert_eq!(at(&l, 1), HALF_R);
        assert_eq!(l.active(2), Some(1));
        assert!(l.visible(1) && !l.visible(2) && l.visible(0));
    }

    #[test]
    fn ac6_maximize() {
        let mut l = with(&[HALF_L, HALF_R]);
        l.set_decorated(0, true);
        let (rects, memory) = ([l.rect(0), l.rect(1)], l.memory.clone());
        let tiles = l.tiles.clone();
        assert_eq!(click_button(&mut l, 0, Button::Maximize), Some("maximized"));
        assert_eq!(l.rect(0), Some(uw().window_rect(FULL, BAR)));
        assert!(!l.visible(1));
        assert_eq!(l.hit((1716, 700)), None, "b's left edge");
        assert_eq!(l.hit((2500, 1437)), None, "b's bottom edge");
        // b's strip is under a's bar now; nothing anywhere is b's, and a has no edges.
        let all = hits(&l);
        assert!(all.iter().all(|h| h.id() == 0), "{all:?}");
        assert!(!all.iter().any(|h| matches!(h, Hit::Edge(..))));
        assert_eq!(drag(&mut l, (100, 12), &[((200, 12), 0)]), None);
        assert!(l.is_maximized(0));
        let cells = |ts: &[Tile]| ts.iter().map(|t| t.cells).collect::<Vec<_>>();
        assert_eq!(cells(&l.tiles), cells(&tiles));
        assert_eq!(l.memory, memory);

        assert_eq!(
            click_button(&mut l, 0, Button::Maximize),
            Some("unmaximized")
        );
        assert_eq!([l.rect(0), l.rect(1)], rects);
        assert!(l.visible(1));

        assert!(l.maximize(0));
        let (n, _) = l.place("new", (3000, 700)).unwrap();
        assert_invariants(&l);
        assert!(!l.is_maximized(0));
        assert!(l.visible(n));
    }

    #[test]
    fn ac7_minimize_and_restore() {
        let mut l = with(&[HALF_L]);
        l.set_decorated(0, true);
        assert_eq!(click_button(&mut l, 0, Button::Minimize), Some("minimized"));
        assert!(!l.visible(0));
        assert_eq!(at(&l, 0), HALF_L);
        let bar = l.bar(0).unwrap();
        assert!(bar.buttons.is_empty());
        let drawn = overlay(&l, None, (3000, 700), 0);
        assert_eq!(drawn, vec![(bar.rect, rgba(ACCENT, DIM))]);
        let mut placed = with(&[HALF_L]);
        placed.tiles[0].minimized = true;
        let (n, _) = placed.place("new", (100, 700)).unwrap();
        assert!(!at(&placed, n).overlaps(&HALF_L));

        assert_eq!(click(&mut l, (100, 12)), Some("restored"));
        assert!(l.visible(0));

        assert!(l.maximize(0));
        assert!(l.minimize(0));
        assert!(!l.is_maximized(0));

        // A minimized tab stays in the bar, dimmed, and its tab brings it back.
        let mut l = with(&[HALF_L, HALF_L]);
        l.set_decorated(1, true);
        assert_eq!(click_button(&mut l, 1, Button::Minimize), Some("minimized"));
        assert!(l.visible(0) && !l.visible(1));
        let t1 = tab(&l, 1);
        assert!(overlay(&l, None, (3000, 700), 0).contains(&(t1, rgba(TAB_BG, DIM))));
        assert_eq!(click(&mut l, mid(t1)), Some("tab switched"));
        assert!(l.visible(1) && !l.tile(1).unwrap().minimized);
        assert_eq!(l.active(0), Some(1));
    }

    #[test]
    fn ac7b_app_requests_are_the_buttons() {
        // The request path and the button give the same layout.
        let mut by_button = with(&[HALF_L, HALF_R]);
        by_button.set_decorated(0, true);
        let mut by_app = with(&[HALF_L, HALF_R]);
        by_app.set_decorated(0, true);
        click_button(&mut by_button, 0, Button::Maximize);
        assert!(by_app.maximize(0));
        assert_invariants(&by_app);
        assert_eq!(by_app.tiles, by_button.tiles);
        assert_eq!(hits(&by_app), hits(&by_button));
        click_button(&mut by_button, 0, Button::Maximize);
        click_button(&mut by_button, 0, Button::Minimize);
        assert!(by_app.unmaximize(0));
        assert!(by_app.minimize(0));
        assert_eq!(by_app.tiles, by_button.tiles);

        let mut l = with(&[HALF_L, HALF_R]);
        let before = (l.tiles.clone(), [l.rect(0), l.rect(1)]);
        assert!(l.maximize(0));
        assert_eq!(l.rect(0), Some(uw().window_rect(FULL, GAP)));
        assert!(!l.visible(1));
        assert!(hits(&l).iter().all(|h| *h == Hit::Strip(0)));
        let maximized = l.tiles.clone();
        assert!(!l.maximize(0));
        assert!(!l.maximize(1), "hidden");
        assert!(!l.minimize(1), "hidden");
        assert!(!l.unmaximize(1), "hidden");
        assert!(!l.maximize(99) && !l.minimize(99) && !l.unmaximize(99) && !l.activate(99));
        assert_eq!(l.tiles, maximized);

        // Navi can always undo it: a drag on the strip does nothing, a click unmaximizes.
        assert_eq!(l.hit((100, 2)), Some(Hit::Strip(0)));
        assert_eq!(drag(&mut l, (100, 2), &[((200, 2), 0)]), None);
        assert_eq!(l.tiles, maximized);
        assert_eq!(click(&mut l, (100, 2)), Some("unmaximized"));
        assert_eq!((l.tiles.clone(), [l.rect(0), l.rect(1)]), before);
        assert!(!l.unmaximize(0));

        assert!(l.minimize(0));
        assert!(!l.visible(0));
        assert!(
            overlay(&l, None, (3000, 700), 0)
                .contains(&(l.zone(Hit::Strip(0)).unwrap(), rgba(ACCENT, DIM)))
        );
        assert_eq!(click(&mut l, (100, 2)), Some("restored"));
        assert!(!l.tiles[0].minimized);

        let mut l = with(&[HALF_L, HALF_L]);
        let tiles = l.tiles.clone();
        assert!(!l.maximize(0), "an inactive tab");
        assert_eq!(l.tiles, tiles);
    }

    #[test]
    fn ac8_closing() {
        let mut l = with(&[HALF_L, HALF_L]);
        l.remove(1);
        assert!(l.visible(0) && l.active(0) == Some(0));
        assert_invariants(&l);

        let mut l = with(&[HALF_L, HALF_R]);
        assert!(l.maximize(0));
        l.remove(0);
        assert!(l.visible(1));
        assert_eq!(l.rect(1), Some(uw().tile_rect(HALF_R)));
        assert_invariants(&l);

        let mut l = with(&[HALF_L]);
        assert!(l.minimize(0));
        l.remove(0);
        assert!(overlay(&l, None, (100, 2), 0).is_empty());

        for gone in [0, 1] {
            let mut l = with(&[HALF_L, HALF_L, HALF_R]);
            let mut d = Drag::press(&l, mid(tab(&l, 1))).unwrap();
            d.motion(&mut l, (2500, 700), 0);
            l.remove(gone);
            let rest = l.tiles.clone();
            d.motion(&mut l, (2500, 700), 600);
            overlay(&l, Some(&d), (2500, 700), 600);
            let done = d.release(&mut l, (2500, 700), 600);
            assert_invariants(&l);
            match gone {
                1 => {
                    assert_eq!(done, None);
                    assert_eq!(l.tiles, rest);
                }
                // The other tab closing doesn't stop the dragged one.
                _ => assert!(matches!(done, Some(Done::Layout("tabbed", _)))),
            }
        }
    }

    /// WCAG 2 relative luminance.
    fn luminance(hex: u32) -> f64 {
        let [_, r, g, b] = hex.to_be_bytes().map(|c| {
            let c = f64::from(c) / 255.0;
            match c <= 0.04045 {
                true => c / 12.92,
                false => ((c + 0.055) / 1.055).powf(2.4),
            }
        });
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn ac10_overlay_and_contrast() {
        let mut l = with(&[c(0, 0, 4, 6), c(4, 0, 4, 6), c(8, 0, 4, 6), c(8, 0, 4, 6)]);
        l.set_decorated(1, true);
        let away = (500, 700);
        let drawn = overlay(&l, None, away, 0);
        let bars: Vec<Rect> = drawn
            .iter()
            .filter(|&&(_, k)| k == rgba(BAR_BG, 1.0))
            .map(|&(r, _)| r)
            .collect();
        assert_eq!(bars, vec![l.bar(1).unwrap().rect, l.bar(3).unwrap().rect]);
        assert!(l.bar(0).is_none());
        assert!(drawn.contains(&(tab(&l, 3), rgba(ACCENT, 1.0))));
        assert!(drawn.contains(&(tab(&l, 2), rgba(TAB_BG, 1.0))));
        assert!(!drawn.contains(&(tab(&l, 2), rgba(ACCENT, 1.0))));

        let close = button(&l, 1, Button::Close);
        let lit = overlay(&l, None, mid(close), 0);
        assert!(lit.contains(&(close, rgba(ACCENT, 1.0))));
        let ink_on_accent = lit.iter().filter(|&&(_, k)| k == rgba(INK_ON_ACCENT, 1.0));
        assert_eq!(ink_on_accent.count(), 10, "the X's squares");
        // Glyphs sit in front of the button, which sits in front of the bar.
        let pos = |want: (Rect, [f32; 4])| lit.iter().position(|&x| x == want).unwrap();
        assert!(pos((close, rgba(ACCENT, 1.0))) < pos((l.bar(1).unwrap().rect, rgba(BAR_BG, 1.0))));

        assert!(contrast(INK, BAR_BG) >= 4.5, "{}", contrast(INK, BAR_BG));
        assert!(contrast(INK_ON_ACCENT, ACCENT) >= 4.5);
        assert!(contrast(ACCENT, TAB_BG) >= 3.0);
    }

    #[test]
    fn glyphs_fit_their_button() {
        let r = Rect {
            x: 100,
            y: 0,
            w: BAR,
            h: BAR,
        };
        let inner = Rect {
            x: 107,
            y: 7,
            w: GLYPH,
            h: GLYPH,
        };
        for b in [Button::Minimize, Button::Maximize, Button::Close] {
            for g in glyph(b, r) {
                assert!(inner.contains((g.x, g.y)), "{b:?} {g:?}");
                assert!(
                    inner.contains((g.x + g.w - 1, g.y + g.h - 1)),
                    "{b:?} {g:?}"
                );
            }
        }
    }

    #[test]
    fn resizing_a_tab_group_resizes_the_group() {
        let mut l = with(&[HALF_L, HALF_L, HALF_R]);
        drag(&mut l, (1714, 700), &[((uw().x_edge(8), 700), 0)]);
        assert_eq!(at(&l, 0), c(0, 0, 8, 6));
        assert_eq!(at(&l, 1), c(0, 0, 8, 6));
        assert_eq!(at(&l, 2), c(8, 0, 4, 6));
        // The group's edge zones belong to its active tab.
        let right = Some(Hit::Edge(1, crate::layout::drag::Side::Right));
        assert_eq!(l.hit((uw().x_edge(8) - 2, 700)), right);
    }
}
