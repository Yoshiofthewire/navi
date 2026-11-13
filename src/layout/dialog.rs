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

/// Where a window belongs in the stacking order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kin {
    /// A tiled or floating window, or a dialog of one: the family of that tile.
    Tiled(WinId),
    /// A dialog without a tiled root: the family of the dialogs under the same `head`.
    Orphan(usize),
    /// An X11 override-redirect window (a menu): rides the window at this index, above its
    /// family's dialogs.
    Menu(usize),
}

/// A window as the stacking order sees it.
#[derive(Clone, Copy, Debug)]
pub struct Stacked {
    pub kin: Kin,
    pub dialog: bool,
    /// Its last raise: any focus that put it in front (a click, the dock, a new window taking
    /// the keyboard), never navi's own refocus.
    pub rank: u64,
    /// Its last raise by the user alone; the only rank a closed window's dialogs inherit.
    pub user_rank: u64,
    /// An orphan family's least group: inherited user raises, else just below the window on top
    /// when it became an orphan.
    pub floor: u64,
    /// The clock when it appeared: a dialog's place in its family until it's raised.
    pub born: u64,
}

/// A shown window's key in the one stacking order: its family's `group`, then its layer (root,
/// dialog, menu), then its own raise, then open order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rank {
    pub group: u64,
    pub layer: u8,
    pub own: u64,
    /// Open order, so equal keys keep a stable order.
    pub seq: usize,
}

/// Each window's key: a tile family by its newest raise, an orphan family by its floor or its
/// newest user raise, a menu with its parent's family.
pub fn ranks(wins: &[Stacked]) -> Vec<Rank> {
    let group = |kin: Kin| -> u64 {
        let family = wins.iter().filter(|w| w.kin == kin);
        match kin {
            Kin::Tiled(_) => family.map(|w| w.rank).max().unwrap_or(0),
            Kin::Orphan(_) => family.map(|w| w.floor.max(w.user_rank)).max().unwrap_or(0),
            Kin::Menu(_) => 0,
        }
    };
    wins.iter()
        .enumerate()
        .map(|(seq, w)| {
            // A menu of a menu rides the first window up its chain that isn't one.
            let mut kin = w.kin;
            for _ in 0..DIALOG_DEPTH {
                match kin {
                    Kin::Menu(p) => kin = wins.get(p).map_or(kin, |p| p.kin),
                    _ => break,
                }
            }
            let layer = match w.kin {
                Kin::Menu(_) => 2,
                _ => u8::from(w.dialog),
            };
            Rank {
                group: group(kin),
                layer,
                own: w.rank.max(w.born),
                seq,
            }
        })
        .collect()
}

/// A new orphan's floor, with nothing inherited: just below `top`, the newest raise of a tile
/// family. Raises step by 2, so it never ties one.
pub fn fresh_floor(top: u64) -> u64 {
    top.saturating_sub(1).max(1)
}

/// What a closed window's dialogs inherit from its family's `(user_rank, floor)`s: user raises
/// only, so a window that raised itself by opening hands nothing on.
pub fn inherited_floor(family: impl Iterator<Item = (u64, u64)>) -> u64 {
    family
        .map(|(user, floor)| user.max(floor))
        .max()
        .unwrap_or(0)
}

/// The windows' indices, back to front.
pub fn stack_order(ranks: &[Rank]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..ranks.len()).collect();
    order.sort_by_key(|&i| ranks[i]);
    order
}

/// The X11 windows in the order navi draws them, bottom first, as indices into `space`: each
/// shown window bottom first, with its float's place in `Layout::floats` (front first; `None`
/// in the tile layer) and whether it is X11. The tile layer goes below the floats, the back
/// float lowest; each keeps its space order within.
pub fn x11_stack(space: &[(Option<usize>, bool)]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..space.len()).filter(|&i| space[i].1).collect();
    order.sort_by_key(|&i| space[i].0.map(std::cmp::Reverse));
    order
}

/// The last window up `start`'s parents: the head of an orphan's family. Capped and loop-safe
/// like `root`.
pub fn head<K: Copy + PartialEq>(start: K, mut parent_of: impl FnMut(K) -> Option<K>) -> K {
    let mut seen = vec![start];
    for _ in 0..DIALOG_DEPTH {
        match seen.last().copied().and_then(&mut parent_of) {
            Some(p) if !seen.contains(&p) => seen.push(p),
            _ => break,
        }
    }
    seen[seen.len() - 1]
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

    fn win(kin: Kin, dialog: bool, rank: u64, user_rank: u64) -> Stacked {
        Stacked {
            kin,
            dialog,
            rank,
            user_rank,
            floor: 0,
            born: 0,
        }
    }

    fn order(wins: &[Stacked]) -> Vec<usize> {
        stack_order(&ranks(wins))
    }

    /// Job 038 AC4: Xwayland gets navi's drawn order of its X11 windows: the tile layer by
    /// rank, a menu above its owner's family, then the floats, back one first. It is a function
    /// of ranks alone, which navi's own refocus never changes (`Navi::focus_by`; the live
    /// check is `x11_lifecycle_and_input_stacking`); a user raise of a tile stays below floats.
    #[test]
    fn x11_stack_is_navis_drawn_order() {
        // 0 Wayland tile, 1 X11 tile, 2 X11 float, 3 X11 dialog of 1, 4 X11 menu of 1,
        // 5 Wayland float (in front), 6 X11 tile never raised.
        let mut wins = vec![
            win(Kin::Tiled(0), false, 1, 1),
            win(Kin::Tiled(1), false, 2, 2),
            win(Kin::Tiled(2), false, 3, 3),
            win(Kin::Tiled(1), true, 2, 2),
            win(Kin::Menu(1), false, 0, 0),
            win(Kin::Tiled(5), false, 4, 4),
            win(Kin::Tiled(6), false, 0, 0),
        ];
        let x11 = [false, true, true, true, true, false, true];
        // `Layout::floats`, front first: 5, then 2.
        let float = |i: usize| match i {
            5 => Some(0),
            2 => Some(1),
            _ => None,
        };
        let stack = |wins: &[Stacked]| {
            let space = order(wins);
            let at: Vec<_> = space.iter().map(|&i| (float(i), x11[i])).collect();
            let out: Vec<usize> = x11_stack(&at).into_iter().map(|k| space[k]).collect();
            out
        };
        assert_eq!(stack(&wins), vec![6, 1, 3, 4, 2]);
        // A user raise of it puts it over tile 1's family, still under the float.
        wins[6].rank = 5;
        wins[6].user_rank = 5;
        assert_eq!(stack(&wins), vec![1, 3, 4, 6, 2]);
        // A float stays above a tile that comes after it in space order.
        assert_eq!(
            x11_stack(&[(None, true), (Some(0), true), (None, true)]),
            vec![0, 2, 1]
        );
    }

    /// Round 3/4: one order from raises. A neighbour raised after a family stays above every
    /// window of that family, whatever order they are mapped or re-shown in.
    #[test]
    fn one_stacking_order_from_raises() {
        // 0 parent (raised at 4), 1 its refused dialog (appeared at 9), 2 neighbour (raised at 6),
        // 3 a never-raised window opened first.
        let mut wins = [
            win(Kin::Tiled(0), false, 4, 4),
            Stacked {
                born: 9,
                ..win(Kin::Tiled(0), true, 0, 0)
            },
            win(Kin::Tiled(2), false, 6, 6),
            win(Kin::Tiled(3), false, 0, 0),
        ];
        assert_eq!(order(&wins), [3, 0, 1, 2]);
        // Re-shown in any order (the fullscreen-exit hop), the result is the same.
        let mut rev = wins;
        rev.reverse();
        let back: Vec<usize> = order(&rev).iter().map(|&i| 3 - i).collect();
        assert_eq!(back, [3, 0, 1, 2]);
        // The user clicks the dialog: its family passes the neighbour.
        wins[1].rank = 10;
        assert_eq!(order(&wins), [3, 2, 0, 1]);
        // Within a family, dialogs by their own raise, above the root.
        let family = [
            win(Kin::Tiled(7), true, 7, 0),
            win(Kin::Tiled(7), true, 2, 0),
            win(Kin::Tiled(7), false, 1, 0),
        ];
        assert_eq!(order(&family), [2, 1, 0]);
    }

    /// Round 5 security (s15-flash): a dialog moved to a window that took the keyboard on its own
    /// (raise 8, no user raise) inherits nothing when that window closes. As an orphan it sits
    /// just below the window on top, the probe the user raised at 6.
    #[test]
    fn a_flash_window_passes_no_rank_to_its_orphans() {
        let wins = [
            win(Kin::Tiled(0), false, 6, 6),
            // D0 after F closed: an orphan, its floor just below the top (6 - 1).
            Stacked {
                floor: 5,
                ..win(Kin::Orphan(1), true, 0, 0)
            },
            win(Kin::Tiled(2), false, 2, 2),
        ];
        assert_eq!(
            order(&wins),
            [2, 1, 0],
            "below the probe, above older windows"
        );
        // A user raise of its old family carries over, as the floor.
        let inherited = [
            win(Kin::Tiled(0), false, 6, 6),
            Stacked {
                floor: 8,
                ..win(Kin::Orphan(1), true, 0, 0)
            },
        ];
        assert_eq!(order(&inherited), [0, 1]);
    }

    /// Round 6 code: the floors `restack` and `forget` give an orphan. F opened (raise 8, no user
    /// raise) and closed: its dialog inherits 0 and then gets a fresh floor below the top tile.
    #[test]
    fn orphan_floors_come_only_from_user_raises() {
        let f_family = [(0, 0), (0, 0)];
        assert_eq!(inherited_floor(f_family.into_iter()), 0);
        let user_family = [(6, 0), (0, 3)];
        assert_eq!(inherited_floor(user_family.into_iter()), 6);
        assert_eq!(inherited_floor(std::iter::empty()), 0);
        // Top tile raised at 8 (the probe): the orphan sits at 7, below it.
        assert_eq!(fresh_floor(8), 7);
        assert_eq!(fresh_floor(0), 1, "never 0, which means unset");
        let wins = [
            win(Kin::Tiled(0), false, 8, 8),
            Stacked {
                floor: fresh_floor(8),
                ..win(Kin::Orphan(1), true, 0, 0)
            },
            win(Kin::Tiled(2), false, 4, 4),
        ];
        assert_eq!(order(&wins), [2, 1, 0]);
    }

    /// Round 5 security (s16-xor): an X11 menu rides its parent's family, above its dialogs, and
    /// a window the user raises later goes over it.
    #[test]
    fn a_menu_stacks_with_its_parent() {
        let mut wins = [
            win(Kin::Tiled(0), false, 4, 4),
            win(Kin::Menu(0), false, 0, 0),
            win(Kin::Tiled(0), true, 4, 0),
            win(Kin::Tiled(2), false, 2, 2),
        ];
        assert_eq!(order(&wins), [3, 0, 2, 1]);
        // A submenu (menu of the menu) rides the same family.
        let with_sub = [
            wins[0],
            wins[1],
            wins[2],
            wins[3],
            win(Kin::Menu(1), false, 0, 0),
        ];
        assert_eq!(order(&with_sub), [3, 0, 2, 1, 4]);
        wins[3].rank = 9;
        assert_eq!(
            order(&wins),
            [0, 2, 1, 3],
            "the probe the user raised covers the menu"
        );
    }

    #[test]
    fn head_is_the_last_parent_within_the_cap() {
        let parents = [None, Some(0), Some(1), Some(4), Some(3)];
        assert_eq!(head(2, |i| parents[i]), 0);
        assert_eq!(head(0, |i| parents[i]), 0);
        // A loop stops before the repeat.
        assert_eq!(head(3, |i| parents[i]), 4);
        let chain: Vec<Option<usize>> = (0..20).map(|i: usize| i.checked_sub(1)).collect();
        assert_eq!(head(19, |i| chain[i]), 19 - DIALOG_DEPTH);
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
