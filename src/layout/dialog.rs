//! Dialogs: kept inside their root's monitor, centred on their parent. The client picks a
//! dialog's size and parent chain, so every sum here is in i64 and every walk is capped.

use super::{FLOAT_MARGIN, Rect, WinId};

/// The most parent lookups a dialog's walk to its root makes.
pub const DIALOG_DEPTH: usize = 8;

/// The tiled window (tile or float) a dialog's parent chain ends at; `Orphan` for a chain that
/// ends without one, is too deep or loops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Root {
    Tiled(WinId),
    Orphan,
}

/// Where a dialog or float on an output with `area` stays: the area, `FLOAT_MARGIN` in.
pub fn bounds(area: Rect) -> Rect {
    let m = FLOAT_MARGIN;
    Rect {
        x: area.x + m,
        y: area.y + m,
        w: (area.w - 2 * m).max(1),
        h: (area.h - 2 * m).max(1),
    }
}

/// A dialog wanting `want` over `parent`: no larger than `b`, centred on `parent`, then slid
/// into `b` on each axis.
pub fn place(parent: Rect, want: (i32, i32), b: Rect) -> Rect {
    let axis = |p: i32, plen: i32, want: i32, s: i32, len: i32| {
        let (s, len) = (i64::from(s), i64::from(len.max(1)));
        let n = i64::from(want).clamp(1, len);
        let at = i64::from(p) + (i64::from(plen) - n) / 2;
        // Inside `b`, so both fit an i32.
        (at.clamp(s, s + len - n) as i32, n as i32)
    };
    let (x, w) = axis(parent.x, parent.w, want.0, b.x, b.w);
    let (y, h) = axis(parent.y, parent.h, want.1, b.y, b.h);
    Rect { x, y, w, h }
}

/// The size to configure a dialog whose geometry is larger than `b`: each oversized axis lowered
/// to the bound, the other kept (0, the client's choice, where it isn't positive). `None` if it
/// fits.
pub fn needs_cap((w, h): (i32, i32), b: Rect) -> Option<(i32, i32)> {
    (w > b.w || h > b.h).then(|| (w.clamp(0, b.w), h.clamp(0, b.h)))
}

/// `start`'s root: `start` itself if tiled, else the first tiled window up its parents, after at
/// most `DIALOG_DEPTH` lookups of `parent_of`.
pub fn root<K: Copy + PartialEq>(
    start: K,
    mut parent_of: impl FnMut(K) -> Option<K>,
    tile_of: impl Fn(K) -> Option<WinId>,
) -> Root {
    if let Some(id) = tile_of(start) {
        return Root::Tiled(id);
    }
    let mut seen = vec![start];
    for _ in 0..DIALOG_DEPTH {
        let Some(p) = seen.last().copied().and_then(&mut parent_of) else {
            break;
        };
        if seen.contains(&p) {
            break;
        }
        if let Some(id) = tile_of(p) {
            return Root::Tiled(id);
        }
        seen.push(p);
    }
    Root::Orphan
}

/// Whether a new dialog with root `dialog` may take the keyboard: only from its own family.
/// `focus` is the root of the window holding the keyboard; `None` while nothing or a layer
/// surface does.
pub fn may_focus_on_map(dialog: Root, focus: Option<Root>) -> bool {
    matches!(dialog, Root::Tiled(_)) && focus == Some(dialog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn inside(a: Rect, b: Rect) -> bool {
        let end = |s: i32, n: i32| i64::from(s) + i64::from(n);
        a.x >= b.x && a.y >= b.y && end(a.x, a.w) <= end(b.x, b.w) && end(a.y, a.h) <= end(b.y, b.h)
    }

    /// The ultrawide under a 32 px status band and above a 72 px dock.
    fn uw_area() -> Rect {
        Rect::new(0, 32, 3440, 1440 - 32 - 72)
    }

    #[test]
    fn ac1_place_centres_caps_and_slides_into_the_bounds() {
        let b = bounds(uw_area());
        assert_eq!(b, Rect::new(8, 40, 3424, 1320));
        // Centred exactly.
        let tile = Rect::new(400, 200, 1600, 1000);
        assert_eq!(place(tile, (600, 400), b), Rect::new(900, 500, 600, 400));
        // Larger than a small tile at the area's left edge: centred on y, slid right on x.
        let small = Rect::new(14, 300, 500, 400);
        let r = place(small, (1400, 900), b);
        assert_eq!((r.w, r.h), (1400, 900));
        assert_eq!(r.x, b.x, "slid in from the left edge");
        assert_eq!(r.y, small.y + (small.h - 900) / 2);
        assert!(
            inside(r, b) && r.x + r.w > small.x + small.w,
            "covers the neighbour"
        );
        // Extremes: no panic, inside and no larger than the bounds.
        let (max, min) = (i32::MAX, i32::MIN);
        for parent in [
            Rect::new(min, min, max, max),
            Rect::new(max, max, max, max),
            Rect::new(min, max, 0, -5),
            Rect::new(max - 1, min, 1, 1),
        ] {
            for want in [(max, max), (0, 0), (-7, max), (min, min), (600, 400)] {
                let r = place(parent, want, b);
                assert!(inside(r, b), "{parent:?} {want:?} -> {r:?}");
                assert!(r.w >= 1 && r.h >= 1);
            }
        }
        assert_eq!(place(tile, (max, max), b), b);
    }

    #[test]
    fn ac1_place_stays_inside_every_rig_output() {
        // Logical sizes of the rig (the laptop at scale 1.6), each without and with panels.
        let rig = [
            Rect::new(1125, 703, 3440, 1440),
            Rect::new(1125, -1457, 3840, 2160),
            Rect::new(0, 703, 1125, 703),
        ];
        let panels = |a: Rect| Rect::new(a.x, a.y + 32, a.w, a.h - 32 - 72);
        let areas = rig.into_iter().flat_map(|a| [a, panels(a)]);
        for area in areas {
            let b = bounds(area);
            assert!(inside(b, area));
            for px in [
                area.x - 5000,
                area.x,
                area.x + area.w / 3,
                area.x + area.w - 10,
            ] {
                for py in [area.y - 5000, area.y, area.y + area.h / 2, area.y + area.h] {
                    for pw in [1, 300, 2000, 8000] {
                        for want in [(1, 1), (640, 480), (1400, 900), (4000, 3000), (9000, 40)] {
                            let parent = Rect::new(px, py, pw, pw / 2);
                            let r = place(parent, want, b);
                            assert!(inside(r, b) && inside(r, area), "{area:?} {want:?} {r:?}");
                        }
                    }
                }
            }
        }
    }

    /// A walk over `parents` (child index -> parent index) where `tiled` windows have tiles,
    /// counting `parent_of` calls.
    fn walk(parents: &[Option<usize>], tiled: &[(usize, WinId)], start: usize) -> (Root, usize) {
        let calls = Cell::new(0);
        let r = root(
            start,
            |i| {
                calls.set(calls.get() + 1);
                parents[i]
            },
            |i| tiled.iter().find(|t| t.0 == i).map(|t| t.1),
        );
        (r, calls.get())
    }

    #[test]
    fn ac2_root_walks_to_the_first_tiled_window_within_the_cap() {
        // 0: tile 10; 1: dialog of 0; 2: float 20; 3: dialog of 2; 4: dialog of 3.
        let parents = [None, Some(0), None, Some(2), Some(3)];
        let tiled = [(0, 10), (2, 20)];
        assert_eq!(walk(&parents, &tiled, 1), (Root::Tiled(10), 1));
        assert_eq!(walk(&parents, &tiled, 4), (Root::Tiled(20), 2));
        assert_eq!(
            walk(&parents, &tiled, 0),
            (Root::Tiled(10), 0),
            "a tile is its own"
        );
        // A chain of n dialogs over tile 0: the dialog at depth n needs n lookups.
        let chain =
            |n: usize| -> Vec<Option<usize>> { (0..=n).map(|i| i.checked_sub(1)).collect() };
        assert_eq!(
            walk(&chain(DIALOG_DEPTH), &[(0, 1)], DIALOG_DEPTH),
            (Root::Tiled(1), DIALOG_DEPTH)
        );
        let deep = DIALOG_DEPTH + 1;
        assert_eq!(
            walk(&chain(deep), &[(0, 1)], deep),
            (Root::Orphan, DIALOG_DEPTH)
        );
        // A two-window loop, and a chain ending at a window with no tile.
        assert_eq!(walk(&[Some(1), Some(0)], &[], 0), (Root::Orphan, 2));
        let (r, n) = walk(&[Some(1), Some(2), None], &[], 0);
        assert_eq!(r, Root::Orphan);
        assert!(n <= DIALOG_DEPTH);
        // A self-parent stops at once.
        assert_eq!(walk(&[Some(0)], &[], 0), (Root::Orphan, 1));
    }

    #[test]
    fn ac3_a_dialog_takes_the_keyboard_on_map_only_from_its_family() {
        let (a, b) = (Root::Tiled(1), Root::Tiled(2));
        assert!(may_focus_on_map(a, Some(a)), "same root");
        assert!(!may_focus_on_map(a, Some(b)), "another root");
        assert!(
            !may_focus_on_map(a, None),
            "nothing, or a layer surface, focused"
        );
        assert!(
            !may_focus_on_map(Root::Orphan, Some(Root::Orphan)),
            "orphan"
        );
        assert!(!may_focus_on_map(Root::Orphan, Some(a)));
        assert!(!may_focus_on_map(Root::Orphan, None));
    }

    #[test]
    fn ac4_cap_lowers_only_the_oversized_axis() {
        let b = Rect::new(14, 56, 1252, 676);
        assert_eq!(needs_cap((600, 400), b), None);
        assert_eq!(needs_cap((1252, 676), b), None, "exactly the bounds fits");
        assert_eq!(needs_cap((4000, 300), b), Some((1252, 300)));
        assert_eq!(needs_cap((300, 4000), b), Some((300, 676)));
        assert_eq!(needs_cap((4000, 3000), b), Some((1252, 676)));
        assert_eq!(needs_cap((0, 0), b), None);
        assert_eq!(needs_cap((-5, -5), b), None);
        assert_eq!(needs_cap((i32::MAX, 0), b), Some((1252, 0)));
        assert_eq!(needs_cap((-5, i32::MAX), b), Some((0, 676)));
        assert_eq!(needs_cap((i32::MAX, i32::MAX), b), Some((1252, 676)));
        assert_eq!(needs_cap((i32::MIN, i32::MIN), b), None);
    }
}
