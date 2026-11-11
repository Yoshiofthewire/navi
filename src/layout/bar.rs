//! The bar above tabbed and server-decorated tiles: tabs, window buttons, and how they're drawn.

use super::{BAR, Layout, Rect, WinId, drag::Hit};
use crate::theme::{ACCENT, BAR_BG, MINIMIZED, OUTLINE, TAB_BG, rgba};

const TAB_GAP: i32 = 2;

/// Who draws a toplevel's title bar and controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decor {
    Client,
    Server,
}

/// What the app asked for, `None` (no preference) being navi's bar. A dialog (a toplevel with a
/// parent) always draws its own, so it never gets a navi bar or buttons.
pub fn decoration(has_parent: bool, requested: Option<Decor>) -> Decor {
    match has_parent {
        true => Decor::Client,
        false => requested.unwrap_or(Decor::Server),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Minimize,
    Maximize,
    Close,
}

#[derive(Debug, PartialEq)]
pub struct Bar {
    /// Directly above the window, full window width, below the grab strip.
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
        let (_, r) = self.frame(self.active(id)?)?;
        let rect = Rect {
            y: r.y - BAR,
            h: BAR,
            ..r
        };
        let active = self.tile(self.active(id)?)?;
        let right = rect.x + rect.w;
        let all: &[Button] = match (active.decorated || active.floating) && !active.minimized {
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

/// The 1 px border inside `r`.
fn outline(r: Rect) -> [Rect; 4] {
    [
        Rect { h: 1, ..r },
        Rect {
            y: r.y + r.h - 1,
            h: 1,
            ..r
        },
        Rect { w: 1, ..r },
        Rect {
            x: r.x + r.w - 1,
            w: 1,
            ..r
        },
    ]
}

/// The attention line on a tab whose family has a waiting dialog, in px.
const ATTENTION_PX: i32 = 3;

/// Front to back: `bar` with `hover` (a tab id) lit. Its buttons are drawn by `crate::glyph`.
pub fn draw(l: &Layout, id: WinId, bar: &Bar, hover: Option<Hit>) -> Vec<(Rect, [f32; 4])> {
    let mut out = Vec::new();
    let active = l.active(id);
    for &(t, r) in &bar.tabs {
        let minimized = l.tile(t).is_some_and(|t| t.minimized);
        let color = match (Some(t) == active && !minimized) || hover == Some(Hit::Tab(t)) {
            true => ACCENT,
            false if minimized => MINIMIZED,
            false => {
                out.extend(outline(r).map(|o| (o, rgba(OUTLINE, 1.0))));
                TAB_BG
            }
        };
        // A dialog waits in this hidden or minimized tab: an accent line along its bottom.
        if color != ACCENT && l.tile(t).is_some_and(|t| t.attention) {
            let line = Rect {
                y: r.y + r.h - ATTENTION_PX,
                h: ATTENTION_PX,
                ..r
            };
            out.push((line, rgba(ACCENT, 1.0)));
        }
        out.push((r, rgba(color, 1.0)));
    }
    out.push((bar.rect, rgba(BAR_BG, 1.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::super::{
        COLS, Cells, FULL, MIN_H, MIN_W, Output, ROWS, STRIP, Tile,
        drag::{
            Done, Drag, Side, buttons,
            tests::{assert_valid, at, c, drag, uw, with, zones},
        },
    };
    use super::*;

    fn out(name: &str, w: i32, h: i32) -> Output {
        Output {
            name: name.into(),
            area: Rect { x: 0, y: 0, w, h },
            full: Rect { x: 0, y: 0, w, h },
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

    /// Round 4: a hidden tab whose dialog waits for the user carries an accent line; the active
    /// tab (its dialog is on screen with an accent frame) and unmarked tabs don't.
    #[test]
    fn a_hidden_tab_with_a_waiting_dialog_is_marked() {
        let mut l = group_on(out("o", 1200, 800), FULL, 2);
        let bar = l.bar(0).unwrap();
        let line = |l: &Layout, t: usize| {
            let r = bar.tabs[t].1;
            let line = Rect {
                y: r.y + r.h - ATTENTION_PX,
                h: ATTENTION_PX,
                ..r
            };
            draw(l, 0, &bar, None).contains(&(line, rgba(ACCENT, 1.0)))
        };
        assert!(!line(&l, 0) && !line(&l, 1));
        assert!(l.set_attention(&[0, 1]));
        assert!(!l.set_attention(&[0, 1]), "no change");
        assert!(line(&l, 0), "hidden tab 0 is marked");
        assert!(!line(&l, 1), "the active tab is accent already");
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
        cols: COLS / 2,
        rows: ROWS,
    };
    const HALF_R: Cells = Cells {
        col: COLS / 2,
        ..HALF_L
    };

    #[test]
    fn ac1_bar_geometry() {
        let o = uw();
        let (cr, plain) = (o.cells_rect(HALF_L), o.tile_rect(HALF_L));
        let mut l = with(&[HALF_L]);
        assert_eq!(l.rect(0), Some(plain));
        assert_eq!(plain.y, cr.y + STRIP);
        l.set_decorated(0, true);
        let r = l.rect(0).unwrap();
        // The bar sits between the strip and the window.
        assert_eq!(r.y, cr.y + STRIP + BAR);
        assert_eq!(r.h, plain.h - BAR);
        assert_eq!(
            l.bar(0).unwrap().rect,
            Rect {
                y: cr.y + STRIP,
                h: BAR,
                ..r
            }
        );
        assert_eq!((r.x, r.w), (plain.x, plain.w));

        let mut l = with(&[HALF_L, HALF_L]);
        assert_eq!(l.rect(0), l.rect(1));
        assert_eq!(l.rect(0).unwrap().y, cr.y + STRIP + BAR);
        l.remove(1);
        assert_eq!(l.rect(0), Some(plain));
    }

    #[test]
    fn ac2_bar_hit_zones() {
        let mut l = with(&[HALF_L]);
        l.set_decorated(0, true);
        let r = l.rect(0).unwrap();
        let (right, top) = (r.x + r.w, r.y - BAR);
        for (k, b) in [
            (3, Button::Minimize),
            (2, Button::Maximize),
            (1, Button::Close),
        ] {
            let x = right - k * BAR;
            for p in [(x, top), (x + BAR - 1, top + BAR - 1), (x + 12, top + 12)] {
                assert_eq!(l.hit(p), Some(Hit::Button(0, b)), "{p:?}");
            }
            // The strip above; its last 26 px are the top-right corner.
            let above = match k {
                1 => Hit::Corner(0, Side::Right, Side::Top),
                _ => Hit::Strip(0),
            };
            assert_eq!(l.hit((x, top - 1)), Some(above));
        }
        assert_eq!(l.hit((right - 3 * BAR - 1, top + 12)), Some(Hit::Strip(0)));
        assert_eq!(l.hit((r.x + 26, 0)), Some(Hit::Strip(0)));
        assert_eq!(
            l.hit((r.x + 25, 0)),
            Some(Hit::Corner(0, Side::Left, Side::Top))
        );
        assert_eq!(l.hit((500, 700)), None);
        assert_eq!(l.hit((500, r.y)), None);

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
            assert_eq!(l.hit((x, bar.rect.y + 5)), want, "x {x}");
            let ends = x < bar.rect.x + 26 || x >= bar.rect.x + bar.rect.w - 26;
            assert_eq!(
                l.hit((x, 5)) == Some(Hit::Strip(1)),
                !ends,
                "the strip above the tabs, x {x}"
            );
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
                                y: STRIP,
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
            zones(&l, None, p, 0);
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

        // Back within half a cell of the press: nothing changes.
        let mut l = with(&[HALF_L, HALF_L]);
        let tiles = l.tiles.clone();
        let back = (from.0 - 60, from.1 + 50);
        assert_eq!(drag(&mut l, from, &[((3000, 700), 0), (back, 10)]), None);
        assert_eq!(l.tiles, tiles);

        // Over another tile: only a hold tabs, and nothing is previewed before it.
        let previews = |l: &Layout, d: &Drag, t| {
            zones(l, Some(d), (2500, 700), t)
                .into_iter()
                .filter(|&(_, k)| k == rgba(ACCENT, 0.35) || k == rgba(ACCENT, 0.55))
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
        assert_eq!(l.rect(0), Some(uw().window_rect(FULL, STRIP + BAR)));
        assert!(!l.visible(1));
        assert_eq!(l.hit((1720, 700)), None, "b's left edge");
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
        let drawn = zones(&l, None, (3000, 700), 0);
        assert_eq!(drawn, vec![(bar.rect, rgba(MINIMIZED, 1.0))]);
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
        assert!(zones(&l, None, (3000, 700), 0).contains(&(t1, rgba(MINIMIZED, 1.0))));
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
        assert_eq!(l.rect(0), Some(uw().window_rect(FULL, STRIP)));
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
            zones(&l, None, (3000, 700), 0)
                .contains(&(l.zone(Hit::Strip(0)).unwrap(), rgba(MINIMIZED, 1.0)))
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
        assert!(zones(&l, None, (100, 2), 0).is_empty());

        for gone in [0, 1] {
            let mut l = with(&[HALF_L, HALF_L, HALF_R]);
            let mut d = Drag::press(&l, mid(tab(&l, 1))).unwrap();
            d.motion(&mut l, (2500, 700), 0);
            l.remove(gone);
            let rest = l.tiles.clone();
            d.motion(&mut l, (2500, 700), 600);
            zones(&l, Some(&d), (2500, 700), 600);
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

    #[test]
    fn ac10_overlay() {
        let mut l = with(&[
            c(0, 0, 8, 12),
            c(8, 0, 8, 12),
            c(16, 0, 8, 12),
            c(16, 0, 8, 12),
        ]);
        l.set_decorated(1, true);
        let away = (500, 700);
        let drawn = zones(&l, None, away, 0);
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

        // Buttons are glyph textures, lit under the pointer or an armed press, in no overlay rect.
        let close = button(&l, 1, Button::Close);
        let lit = zones(&l, None, mid(close), 0);
        assert!(!lit.iter().any(|&(r, _)| r == close));
        let glyphs = buttons(&l, None, mid(close));
        assert_eq!(glyphs.len(), 3);
        assert!(glyphs.contains(&(1, Button::Close, close, true)));
        assert!(glyphs.iter().filter(|g| g.3).count() == 1);
        assert!(buttons(&l, None, away).iter().all(|g| !g.3));
        let d = Drag::press(&l, mid(close)).unwrap();
        assert!(buttons(&l, Some(&d), away).contains(&(1, Button::Close, close, true)));

        // Inactive tabs are outlined; minimized ones are the same dimmed accent as a strip.
        for side in outline(tab(&l, 2)) {
            assert!(drawn.contains(&(side, rgba(OUTLINE, 1.0))), "{side:?}");
        }
        assert!(
            !drawn
                .iter()
                .any(|&(r, k)| k == rgba(OUTLINE, 1.0) && r.x >= tab(&l, 3).x)
        );
    }

    /// Job 016 AC11: no preference is navi's bar for a toplevel; dialogs always draw their own.
    #[test]
    fn dialogs_always_draw_their_own_decorations() {
        assert_eq!(decoration(false, None), Decor::Server);
        assert_eq!(decoration(true, None), Decor::Client);
        for requested in [Decor::Client, Decor::Server] {
            assert_eq!(decoration(true, Some(requested)), Decor::Client);
            assert_eq!(decoration(false, Some(requested)), requested);
        }
        // A tiled toplevel that asks for server-side gets navi's bar and buttons.
        let mut l = with(&[HALF_L]);
        l.set_decorated(0, decoration(false, Some(Decor::Server)) == Decor::Server);
        assert_eq!(l.bar(0).map(|b| b.buttons.len()), Some(3));
        l.set_decorated(0, decoration(true, Some(Decor::Server)) == Decor::Server);
        assert_eq!(l.bar(0), None);
    }

    /// Job 016 AC3: three `BAR`-square buttons, flush right and touching, as tight as KWin's.
    #[test]
    fn window_buttons_are_bar_squares_flush_right_and_touching() {
        for cells in [HALF_L, c(6, 3, 3, 2), FULL] {
            let mut l = with(&[cells]);
            l.set_decorated(0, true);
            let bar = l.bar(0).unwrap();
            let order: Vec<Button> = bar.buttons.iter().map(|&(b, _)| b).collect();
            assert_eq!(order, [Button::Minimize, Button::Maximize, Button::Close]);
            let mut x = bar.rect.x + bar.rect.w;
            for &(b, r) in bar.buttons.iter().rev() {
                assert_eq!(
                    r,
                    Rect {
                        x: x - BAR,
                        y: bar.rect.y,
                        w: BAR,
                        h: BAR
                    },
                    "{b:?} in {cells:?}"
                );
                x -= BAR;
            }
        }
    }

    #[test]
    fn preview_is_where_the_window_lands() {
        let preview = |l: &Layout, d: &Drag, p| {
            let rects = zones(l, Some(d), p, 600);
            rects
                .into_iter()
                .find(|&(_, k)| k == rgba(ACCENT, 0.35) || k == rgba(ACCENT, 0.55))
                .map(|(r, _)| r)
        };
        // A decorated tile moved to free space lands below its bar.
        let mut l = with(&[HALF_L]);
        l.set_decorated(0, true);
        let mut d = Drag::press(&l, (100, 12)).unwrap();
        d.motion(&mut l, (3000, 700), 0);
        assert_eq!(
            preview(&l, &d, (3000, 700)),
            Some(uw().window_rect(HALF_R, STRIP + BAR))
        );
        d.release(&mut l, (3000, 700), 10);
        assert_eq!(l.rect(0), Some(uw().window_rect(HALF_R, STRIP + BAR)));

        // A held tab-in will have a bar; an undecorated untab won't.
        let mut l = with(&[HALF_L, HALF_L, HALF_R]);
        let mut d = Drag::press(&l, mid(tab(&l, 1))).unwrap();
        d.motion(&mut l, (2500, 700), 0);
        assert_eq!(
            preview(&l, &d, (2500, 700)),
            Some(uw().window_rect(HALF_R, STRIP + BAR))
        );
        let mut l = with(&[HALF_L, HALF_L]);
        let mut d = Drag::press(&l, mid(tab(&l, 1))).unwrap();
        d.motion(&mut l, (3000, 700), 0);
        assert_eq!(preview(&l, &d, (3000, 700)), Some(uw().tile_rect(HALF_R)));
    }

    #[test]
    fn a_maximized_strip_lights_on_hover() {
        let mut l = with(&[HALF_L, HALF_R]);
        assert!(l.maximize(0));
        let strip = l.zone(Hit::Strip(0)).unwrap();
        assert_eq!((strip.w, strip.h), (uw().window_rect(FULL, STRIP).w, STRIP));
        assert_eq!(
            zones(&l, None, (100, 2), 0),
            vec![(strip, rgba(ACCENT, 1.0))]
        );
    }

    #[test]
    fn resizing_a_tab_group_resizes_the_group() {
        let mut l = with(&[HALF_L, HALF_L, HALF_R]);
        drag(&mut l, (1718, 700), &[((uw().x_edge(16), 700), 0)]);
        assert_eq!(at(&l, 0), c(0, 0, 16, 12));
        assert_eq!(at(&l, 1), c(0, 0, 16, 12));
        assert_eq!(at(&l, 2), c(16, 0, 8, 12));
        // The group's edge zones belong to its active tab.
        let right = Some(Hit::Edge(1, crate::layout::drag::Side::Right));
        assert_eq!(l.hit((uw().x_edge(16) - 2, 700)), right);
    }
}
