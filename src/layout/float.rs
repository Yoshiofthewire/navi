//! Floats: windows too big for any tile. The user moves and resizes them inside their bounds, and
//! they tile again once free cells on their own output hold them.

use super::{
    BAR, Cells, GAP, Layout, MIN_H, MIN_W, Need, Output, Rect, Tile, WinId, dist2,
    drag::{ARM_PX, CORNER_PX, Done, Hit, Nudge, Side},
};

/// The band around a float's bar and window that resizes it, in px.
pub(super) const FLOAT_EDGE: i32 = GAP / 2;
/// Further than any output: a double-click fill goes to the bounds or the max.
const FAR: i64 = 1 << 40;

/// Where a float on `o` stays while it fits: a dialog's bounds there.
pub(super) fn bounds(o: &Output) -> Rect {
    super::dialog::bounds(o.area)
}

/// The bar and window as one rect; saturating, since a drag may push either to `i32::MAX`.
fn outer(r: Rect) -> Rect {
    Rect {
        y: r.y.saturating_sub(BAR),
        h: r.h.saturating_add(BAR),
        ..r
    }
}

fn window(r: Rect) -> Rect {
    Rect {
        y: r.y.saturating_add(BAR),
        h: r.h.saturating_sub(BAR),
        ..r
    }
}

/// `(start, len)` of `r` along x, or along y.
fn get(r: Rect, x: bool) -> (i64, i64) {
    match x {
        true => (r.x.into(), r.w.into()),
        false => (r.y.into(), r.h.into()),
    }
}

fn set(r: Rect, x: bool, (s, n): (i64, i64)) -> Rect {
    let px = |v: i64| v.clamp(i32::MIN.into(), i32::MAX.into()) as i32;
    match x {
        true => Rect {
            x: px(s),
            w: px(n),
            ..r
        },
        false => Rect {
            y: px(s),
            h: px(n),
            ..r
        },
    }
}

/// One axis of a float's outer rect, in i64 so no client size or pointer overflows.
#[derive(Clone, Copy, Debug)]
struct Axis {
    /// The floor: the client's min, else navi's (never above the client's max).
    lo: i64,
    /// The client's max, `i64::MAX` without one.
    max: i64,
    /// The ceiling: the max or the bounds.
    hi: i64,
    start: i64,
    len: i64,
}

impl Axis {
    /// `t`'s axis on `o`; `None` where its floor is bigger than the bounds, so it keeps 017's
    /// centred spot on that axis.
    fn of(t: &Tile, o: &Output, x: bool) -> Option<Axis> {
        let b = bounds(o);
        let (min, max, navi, bar) = match x {
            true => (t.hints.min.0, t.hints.max.0, MIN_W, 0),
            false => (t.hints.min.1, t.hints.max.1, MIN_H, BAR),
        };
        let (start, len) = get(b, x);
        let bar = i64::from(bar);
        let max = match max {
            0 => i64::MAX,
            m => i64::from(m) + bar,
        };
        let lo = (i64::from(navi) + bar).min(max).max(i64::from(min) + bar);
        (lo <= len).then_some(Axis {
            lo,
            max,
            hi: max.min(len),
            start,
            len,
        })
    }

    /// `(s, n)` with its length between the floor and the ceiling, then inside the bounds.
    fn fit(&self, s: i64, n: i64) -> (i64, i64) {
        let n = n.clamp(self.lo, self.hi);
        (s.clamp(self.start, self.start + self.len - n), n)
    }

    /// Moves the near (left or top) or far side of `(s, n)` by `d`; the other side stays.
    fn resize(&self, (s, n): (i64, i64), far: bool, d: i64) -> (i64, i64) {
        let end = self.start + self.len;
        match far {
            true => (s, (n + d).clamp(self.lo, self.hi.min(end - s).max(self.lo))),
            false => {
                let to = s + n;
                let n = (n - d).clamp(self.lo, self.hi.min(to - self.start).max(self.lo));
                (to - n, n)
            }
        }
    }
}

/// A float's window: the user's rect, clamped on each axis that fits the bounds, else `centred`.
pub(super) fn placed(t: &Tile, o: &Output, centred: Rect) -> Rect {
    let Some(u) = t.user_rect else {
        return centred;
    };
    let (mut r, c) = (outer(u), outer(centred));
    for x in [true, false] {
        let span = match Axis::of(t, o, x) {
            Some(a) => {
                let (s, n) = get(r, x);
                a.fit(s, n)
            }
            None => get(c, x),
        };
        r = set(r, x, span);
    }
    window(r)
}

/// `r` with `sides` moved by `d` (x, y), on the axes that fit.
fn resized(t: &Tile, o: &Output, mut r: Rect, sides: &[Side], d: (i64, i64)) -> Rect {
    for &side in sides {
        let x = matches!(side, Side::Left | Side::Right);
        let far = matches!(side, Side::Right | Side::Bottom);
        if let Some(a) = Axis::of(t, o, x) {
            r = set(r, x, a.resize(get(r, x), far, if x { d.0 } else { d.1 }));
        }
    }
    r
}

/// Its minimum fits `o`'s bounds on both axes.
fn fits(t: &Tile, o: &Output) -> bool {
    Axis::of(t, o, true).is_some() && Axis::of(t, o, false).is_some()
}

/// The band zone `side` of `p` lies in becomes a corner within `CORNER_PX` of the band's ends.
fn corner(band: Rect, id: WinId, side: Side, p: (i32, i32)) -> Hit {
    match side {
        Side::Left | Side::Right if p.1 < band.y + CORNER_PX => Hit::Corner(id, side, Side::Top),
        Side::Left | Side::Right if p.1 >= band.y + band.h - CORNER_PX => {
            Hit::Corner(id, side, Side::Bottom)
        }
        Side::Top | Side::Bottom if p.0 < band.x + CORNER_PX => Hit::Corner(id, Side::Left, side),
        Side::Top | Side::Bottom if p.0 >= band.x + band.w - CORNER_PX => {
            Hit::Corner(id, Side::Right, side)
        }
        _ => Hit::Edge(id, side),
    }
}

impl Layout {
    /// Every float, the front one first: the one order floats are drawn, hit and given input in.
    /// By stacking rank (only a raise moves it), then by when it was last touched.
    pub fn floats(&self) -> Vec<WinId> {
        let mut floats: Vec<&Tile> = self.tiles.iter().filter(|t| t.floating).collect();
        floats.sort_by_key(|t| std::cmp::Reverse((t.rank, t.stamp)));
        floats.into_iter().map(|t| t.id).collect()
    }

    /// `hit(p)`, unless the window under `p` belongs to float `over` (it or its dialog) drawn in
    /// front of the zone: a float's window and dialogs cover the zones of every float behind it
    /// and every tile's.
    pub fn hit_under(&self, p: (i32, i32), over: Option<WinId>) -> Option<Hit> {
        let hit = self.hit(p)?;
        let Some(over) = over else {
            return Some(hit);
        };
        let order = self.floats();
        let rank = |id| order.iter().position(|&f| f == id).unwrap_or(usize::MAX);
        (rank(over) >= rank(hit.id())).then_some(hit)
    }

    /// Brings float `id` to the front, as focusing it does. Returns whether it moved; a tile or
    /// the front float doesn't.
    pub fn raise(&mut self, id: WinId) -> bool {
        let front = self.floats().first() == Some(&id);
        let float = self.tile(id).is_some_and(|t| t.floating);
        if float && !front {
            self.touch(id);
        }
        float && !front
    }

    /// Stores each user rect as it shows, so new hints or a smaller area apply at once.
    pub(super) fn pin_floats(&mut self) {
        let ids: Vec<WinId> = self
            .tiles
            .iter()
            .filter(|t| t.user_rect.is_some())
            .map(|t| t.id)
            .collect();
        for id in ids {
            let shown = self.frame(id).map(|f| f.1);
            if let Some(t) = self.tile_mut(id) {
                t.user_rect = shown;
            }
        }
    }

    /// What the front-most shown float under `p` (its bar, window or band) takes it as: a button,
    /// the rest of its bar, an edge or corner, or `None` inside its window. `None` if no float is
    /// there.
    pub(super) fn float_hit(&self, p: (i32, i32)) -> Option<Option<Hit>> {
        let floats = self.floats().into_iter().filter_map(|id| self.tile(id));
        let mut shown = floats.filter(|t| self.visible(t.id) && !t.fullscreen);
        shown.find_map(|f| {
            let (o, r) = self.frame(f.id)?;
            if r.contains(p) {
                return Some(None);
            }
            if o.contains(p) {
                let bar = self.bar(f.id)?;
                let button = bar.buttons.iter().find(|(_, r)| r.contains(p));
                return Some(Some(
                    button.map_or(Hit::Strip(f.id), |&(b, _)| Hit::Button(f.id, b)),
                ));
            }
            let band = Rect {
                x: o.x - FLOAT_EDGE,
                y: o.y - FLOAT_EDGE,
                w: o.w + 2 * FLOAT_EDGE,
                h: o.h + 2 * FLOAT_EDGE,
            };
            let side = [Side::Left, Side::Right, Side::Top, Side::Bottom]
                .into_iter()
                .find(|&s| self.float_zone(f.id, s).is_some_and(|z| z.contains(p)))?;
            Some(Some(corner(band, f.id, side, p)))
        })
    }

    /// A float's `side` band: the sides run the band's whole height, the top and bottom between.
    pub(super) fn float_zone(&self, id: WinId, side: Side) -> Option<Rect> {
        let (o, _) = self.frame(id)?;
        let (e, h) = (FLOAT_EDGE, o.h + 2 * FLOAT_EDGE);
        Some(match side {
            Side::Left => Rect::new(o.x - e, o.y - e, e, h),
            Side::Right => Rect::new(o.x + o.w, o.y - e, e, h),
            Side::Top => Rect {
                y: o.y - e,
                h: e,
                ..o
            },
            Side::Bottom => Rect {
                y: o.y + o.h,
                h: e,
                ..o
            },
        })
    }

    /// Puts float `id` on `output` with outer rect `r`, clamped. Returns whether its window or
    /// output changed; if neither did, nothing does.
    fn put_float(&mut self, id: WinId, output: &str, r: Rect) -> bool {
        let Some(was) = self.tile(id).filter(|t| t.floating).cloned() else {
            return false;
        };
        if self.output(output).is_none() {
            return false;
        }
        let shown = self.frame(id).map(|f| f.1);
        let Some(t) = self.tile_mut(id) else {
            return false;
        };
        t.output = output.to_string();
        t.user_rect = Some(window(r));
        // Stores what shows, so the next change starts from it.
        let now = self.frame(id).map(|f| f.1);
        let changed = was.output != output || now != shown;
        if let Some(t) = self.tile_mut(id) {
            match changed {
                true => t.user_rect = now,
                false => *t = was,
            }
        }
        changed
    }

    /// Alt/Super(+Shift)+Arrow on a float: one cell of its output, or what is left to its bounds.
    pub(super) fn float_nudge(
        &mut self,
        id: WinId,
        side: Side,
        op: Nudge,
    ) -> Result<Done, &'static str> {
        let t = self.tile(id).ok_or("no tile")?.clone();
        let o = self.output(&t.output).ok_or("no output")?.clone();
        let r = self.frame(id).ok_or("no output")?.0;
        let x = matches!(side, Side::Left | Side::Right);
        let step = i64::from(if x { o.cell_w() } else { o.cell_h() });
        let d = match side {
            Side::Right | Side::Bottom => step,
            Side::Left | Side::Top => -step,
        };
        let to = match op {
            Nudge::Move => {
                let (s, n) = get(r, x);
                set(r, x, (s + d, n))
            }
            Nudge::Grow => resized(&t, &o, r, &[side], (d, d)),
            Nudge::Shrink => resized(&t, &o, r, &[side], (-d, -d)),
        };
        if !self.put_float(id, &o.name, to) {
            let at_max = Axis::of(&t, &o, x).is_some_and(|a| get(r, x).1 >= a.max);
            return Err(match op {
                Nudge::Move => "monitor edge",
                Nudge::Shrink => "float at minimum",
                Nudge::Grow if at_max => "float at maximum",
                Nudge::Grow => "monitor edge",
            });
        }
        let what = match op {
            Nudge::Move => "float moved",
            _ => "float resized",
        };
        Ok(Done::Layout(what, self.tiles_in(&[id])))
    }

    /// A double-click on a float's edge: that side out to the bounds, or to the max.
    pub(super) fn float_fill(&mut self, id: WinId, side: Side) -> Option<Done> {
        let t = self.tile(id).filter(|t| t.floating)?.clone();
        let o = self.output(&t.output)?.clone();
        let r = self.frame(id)?.0;
        let d = match side {
            Side::Right | Side::Bottom => FAR,
            Side::Left | Side::Top => -FAR,
        };
        let to = resized(&t, &o, r, &[side], (d, d));
        self.put_float(id, &o.name, to)
            .then(|| Done::Layout("float resized", self.tiles_in(&[id])))
    }

    /// Tiles each shown float, most recent first, in free cells of its own output that hold it,
    /// nearest its centre: the span of its current size, else the largest free one down to its
    /// minimum span. Nothing else moves; a float waits while its output has a maximized group.
    /// Memory learns each tiled float's spot. Returns them with their output and cells.
    pub fn tile_floats(&mut self) -> Vec<(WinId, String, Cells)> {
        let mut floats: Vec<(u64, WinId)> = self
            .tiles
            .iter()
            .filter(|t| t.floating && self.visible(t.id) && !t.fullscreen)
            .map(|t| (t.stamp, t.id))
            .collect();
        floats.sort_unstable_by(|a, b| b.cmp(a));
        let mut tiled = Vec::new();
        for (_, id) in floats {
            let Some(t) = self.tile(id).cloned() else {
                continue;
            };
            let (Some(o), Some((outer, r))) = (self.output(&t.output).cloned(), self.frame(id))
            else {
                continue;
            };
            if self.tiles.iter().any(|u| u.output == o.name && u.maximized) {
                continue;
            }
            let bar = if t.decorated { BAR } else { 0 };
            let need = Need {
                size: (r.w + GAP, r.h + GAP + bar),
                min: t.hints.min,
                bar: t.decorated,
            };
            let Some((want, min)) = need.spans(&o) else {
                continue;
            };
            let centre = outer.center();
            let Some(cells) = self
                .nearest_free(&o, want, centre)
                .or_else(|| self.smaller_free(&o, want, min, centre))
            else {
                continue;
            };
            if let Some(t) = self.tile_mut(id) {
                t.floating = false;
                t.user_rect = None;
                t.cells = cells;
            }
            let tiles = self.tiles_in(&[id]);
            self.learn(&tiles, true);
            tiled.push((id, o.name, cells));
        }
        tiled
    }
}

/// A float moved by its bar or resized by its band, px for px.
pub(super) struct FloatDrag {
    pub(super) hit: Hit,
    pub(super) press: (i32, i32),
    /// The pointer has travelled `ARM_PX` since the press.
    pub(super) dragged: bool,
    /// The float at the press, which a cancel puts back.
    before: Tile,
    /// Its outer rect at the press.
    start: Rect,
}

impl FloatDrag {
    pub(super) fn new(l: &Layout, hit: Hit, press: (i32, i32), dragged: bool) -> Option<Self> {
        let t = l.tile(hit.id()).filter(|t| t.floating)?;
        Some(Self {
            hit,
            press,
            dragged,
            before: t.clone(),
            start: l.frame(t.id)?.0,
        })
    }

    pub(super) fn id(&self) -> WinId {
        self.before.id
    }

    /// Follows the pointer to `p`: the grabbed point stays under it, clamped. A move goes to the
    /// output under `p` if its minimum fits there, else stays on its own. Returns whether it changed.
    pub(super) fn motion(&mut self, l: &mut Layout, p: (i32, i32)) -> bool {
        self.dragged |= dist2(self.press, p) >= ARM_PX * ARM_PX;
        let Some(t) = l.tile(self.id()).cloned().filter(|_| self.dragged) else {
            return false;
        };
        let d = (
            i64::from(p.0) - i64::from(self.press.0),
            i64::from(p.1) - i64::from(self.press.1),
        );
        // Its own output, or where an unplug moved it meanwhile.
        let own = match l.output(&self.before.output) {
            Some(_) => &self.before.output,
            None => &t.output,
        };
        let (output, r) = match self.hit {
            Hit::Edge(_, side) | Hit::Corner(_, side, _) => {
                let Some(o) = l.output(own) else {
                    return false;
                };
                let sides = match self.hit {
                    Hit::Corner(_, x, y) => vec![x, y],
                    _ => vec![side],
                };
                (own.clone(), resized(&t, o, self.start, &sides, d))
            }
            _ => {
                let here = l.outputs.iter().find(|o| o.area.contains(p));
                let output = match here.filter(|o| fits(&t, o)) {
                    Some(o) => o.name.clone(),
                    None => own.clone(),
                };
                let r = set(
                    self.start,
                    true,
                    (get(self.start, true).0 + d.0, self.start.w.into()),
                );
                (output, set(r, false, (get(r, false).0 + d.1, r.h.into())))
            }
        };
        l.put_float(self.id(), &output, r)
    }

    /// Puts the float back as it was at the press, unless its output went meanwhile (it stays
    /// where the unplug put it). Returns whether it changed.
    pub(super) fn cancel(self, l: &mut Layout) -> bool {
        if l.output(&self.before.output).is_none() {
            return false;
        }
        let Some(t) = l
            .tiles
            .iter_mut()
            .find(|t| t.id == self.before.id && t.floating)
        else {
            return false;
        };
        let changed = (&t.output, t.user_rect) != (&self.before.output, self.before.user_rect);
        t.output = self.before.output;
        t.user_rect = self.before.user_rect;
        changed
    }

    /// After the release's last motion: raises a float that changed and reports it.
    pub(super) fn finish(self, l: &mut Layout) -> Option<Done> {
        let id = self.id();
        let now = l
            .tile(id)
            .map(|t| (t.output.clone(), l.frame(id).map(|f| f.0)))?;
        if now == (self.before.output.clone(), Some(self.start)) {
            return None;
        }
        l.touch(id);
        let what = match self.hit {
            Hit::Edge(..) | Hit::Corner(..) => "float resized",
            _ => "float moved",
        };
        Some(Done::Layout(what, l.tiles_in(&[id])))
    }
}
