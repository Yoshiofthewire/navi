//! Rounded outlines: what a window, its bar and its frame are cut to.

use super::{Rect, intersect};

/// A rect with rounded corners. Radii are top-left, top-right, bottom-right, bottom-left, each
/// at most half the rect's shorter side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shape {
    pub rect: Rect,
    pub radii: [i32; 4],
}

/// What a rect of navi's chrome is cut to: a shape, or the ring between it and a `hole` (a
/// frame). Only px in the shape's corner squares are cut; the rest of a rect is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cut {
    pub shape: Shape,
    pub hole: Option<Shape>,
}

impl Shape {
    /// `rect` rounded by `radius` at every corner.
    pub fn new(rect: Rect, radius: i32) -> Shape {
        Shape::with_radii(rect, [radius; 4])
    }

    /// Each radius clamped to `0..=` half the shorter side.
    pub fn with_radii(rect: Rect, radii: [i32; 4]) -> Shape {
        let max = (rect.w.min(rect.h) / 2).max(0);
        Shape {
            rect,
            radii: radii.map(|r| r.clamp(0, max)),
        }
    }

    /// The same shape on an output: `map` takes a rect to its px there, `scale` the radii.
    pub fn scaled(&self, map: impl Fn(Rect) -> Rect, scale: f64) -> Shape {
        let px = |r: i32| {
            (f64::from(r) * scale)
                .round()
                .clamp(0.0, f64::from(i32::MAX)) as i32
        };
        Shape::with_radii(map(self.rect), self.radii.map(px))
    }

    /// The `r × r` square at each corner, in radii order; empty where a radius is 0.
    pub fn corners(&self) -> [Rect; 4] {
        let Rect { x, y, w, h } = self.rect;
        let [a, b, c, d] = self.radii;
        [
            Rect::new(x, y, a, a),
            Rect::new(x + w - b, y, b, b),
            Rect::new(x + w - c, y + h - c, c, c),
            Rect::new(x, y + h - d, d, d),
        ]
    }

    /// Whether px `p` lies in a corner square outside its arc, judged at the px centre.
    pub fn cuts(&self, (px, py): (i32, i32)) -> bool {
        self.corners()
            .into_iter()
            .zip(self.radii)
            .enumerate()
            .any(|(i, (sq, r))| {
                if r == 0 || !sq.contains((px, py)) {
                    return false;
                }
                // The arc's centre is the square's inner corner; doubled to stay whole.
                let cx = i64::from(if i == 0 || i == 3 { sq.x + r } else { sq.x });
                let cy = i64::from(if i < 2 { sq.y + r } else { sq.y });
                let dx = 2 * i64::from(px) + 1 - 2 * cx;
                let dy = 2 * i64::from(py) + 1 - 2 * cy;
                dx * dx + dy * dy > 4 * i64::from(r) * i64::from(r)
            })
    }

    /// Whether px `p` is inside the rounded rect.
    pub fn contains(&self, p: (i32, i32)) -> bool {
        self.rect.contains(p) && !self.cuts(p)
    }

    /// Whether `r` overlaps a corner square, so cutting it to the shape removes px.
    pub fn meets(&self, r: Rect) -> bool {
        self.corners()
            .into_iter()
            .any(|sq| intersect(r, sq).is_some())
    }

    /// `r` without the corner squares, as disjoint rects: the part the shape never cuts.
    pub fn outside_corners(&self, r: Rect) -> Vec<Rect> {
        self.corners()
            .into_iter()
            .filter(|sq| sq.w > 0)
            .fold(vec![r], |rects, sq| {
                rects.into_iter().flat_map(|r| subtract(r, sq)).collect()
            })
    }
}

impl Cut {
    /// The ring `px` wide just outside `inner`, concentric with `inner` rounded by `radius`.
    pub fn ring(inner: Rect, radius: i32, px: i32) -> Cut {
        let outer = Rect {
            x: inner.x - px,
            y: inner.y - px,
            w: inner.w + 2 * px,
            h: inner.h + 2 * px,
        };
        let shape = Shape::new(outer, radius + px);
        Cut {
            shape,
            hole: Some(Shape::with_radii(inner, shape.radii.map(|r| r - px))),
        }
    }

    pub fn solid(shape: Shape) -> Cut {
        Cut { shape, hole: None }
    }

    pub fn scaled(&self, map: impl Fn(Rect) -> Rect + Copy, scale: f64) -> Cut {
        Cut {
            shape: self.shape.scaled(map, scale),
            hole: self.hole.map(|h| h.scaled(map, scale)),
        }
    }

    /// Whether `p`, a px of a rect cut to this, is drawn.
    #[cfg(test)]
    pub fn keeps(&self, p: (i32, i32)) -> bool {
        !self.shape.cuts(p) && !self.hole.is_some_and(|h| h.contains(p))
    }
}

/// Solid parts and cut corner parts of chrome, each with its rect's index.
pub type Pieces<K> = (Vec<(usize, Rect, K)>, Vec<(usize, Rect, K, Cut)>);

/// How chrome rects are drawn: by rect index, the px outside their cut's corner squares solid,
/// and the parts inside cut. A cut with a hole (a ring, a lit corner) draws each corner once,
/// for the first rect to reach it, over the box around every part its rects have there.
pub fn pieces<K: Copy>(rects: &[(Rect, K, Option<Cut>)]) -> Pieces<K> {
    let (mut plain, mut corners): Pieces<K> = (Vec::new(), Vec::new());
    let mut drawn: Vec<(Cut, usize, usize)> = Vec::new();
    for (i, &(r, k, cut)) in rects.iter().enumerate() {
        let Some(c) = cut.filter(|c| c.shape.meets(r)) else {
            plain.push((i, r, k));
            continue;
        };
        plain.extend(c.shape.outside_corners(r).into_iter().map(|p| (i, p, k)));
        for (at, sq) in c.shape.corners().into_iter().enumerate() {
            let Some(part) = intersect(r, sq) else {
                continue;
            };
            match drawn
                .iter()
                .find(|d| c.hole.is_some() && (d.0, d.1) == (c, at))
            {
                Some(&(.., j)) => corners[j].1 = bounding(corners[j].1, part),
                None => {
                    drawn.push((c, at, corners.len()));
                    corners.push((i, part, k, c));
                }
            }
        }
    }
    (plain, corners)
}

/// The smallest rect holding `a` and `b`.
fn bounding(a: Rect, b: Rect) -> Rect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    let right = (a.x + a.w).max(b.x + b.w);
    let bottom = (a.y + a.h).max(b.y + b.h);
    Rect::new(x, y, right - x, bottom - y)
}

/// `r` minus `hole`: up to four disjoint rects.
fn subtract(r: Rect, hole: Rect) -> Vec<Rect> {
    let Some(i) = intersect(r, hole) else {
        return vec![r];
    };
    let (right, bottom) = (r.x + r.w, r.y + r.h);
    [
        Rect::new(r.x, r.y, r.w, i.y - r.y),
        Rect::new(r.x, i.y + i.h, r.w, bottom - i.y - i.h),
        Rect::new(r.x, i.y, i.x - r.x, i.h),
        Rect::new(i.x + i.w, i.y, right - i.x - i.w, i.h),
    ]
    .into_iter()
    .filter(|p| p.w > 0 && p.h > 0)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::super::POPUP_LIMIT;
    use super::*;
    use crate::theme::RADIUS;

    fn corner_px(r: Rect) -> [(i32, i32); 4] {
        let (x1, y1) = (r.x + r.w - 1, r.y + r.h - 1);
        [(r.x, r.y), (x1, r.y), (x1, y1), (r.x, y1)]
    }

    /// AC1: the radius is `RADIUS` scaled, clamped to half the shorter side; degenerate and far
    /// rects don't panic or overflow.
    #[test]
    fn radius_scales_and_clamps() {
        let r = Rect::new(0, 0, 400, 300);
        assert_eq!(Shape::new(r, RADIUS).radii, [10; 4]);
        assert_eq!(Shape::new(r, RADIUS).scaled(|r| r, 1.6).radii, [16; 4]);
        assert_eq!(Shape::new(r, RADIUS).scaled(|r| r, 1.25).radii, [13; 4]);
        assert_eq!(Shape::new(Rect::new(0, 0, 400, 8), RADIUS).radii, [4; 4]);
        assert_eq!(
            Shape::new(Rect::new(0, 0, 400, 8), RADIUS)
                .scaled(|_| Rect::new(0, 0, 640, 13), 1.6)
                .radii,
            [6; 4],
            "clamped again in px"
        );
        for (w, h) in [(0, 0), (1, 1), (1, 300), (300, 1), (-5, 3), (2, 2)] {
            let s = Shape::new(Rect::new(7, 7, w, h), RADIUS);
            assert!(
                s.radii
                    .iter()
                    .all(|&k| k >= 0 && 2 * k <= w.max(0).min(h.max(0)))
            );
            assert!(!s.contains((7 + w, 7)));
            let _ = (s.cuts((7, 7)), s.meets(s.rect), s.outside_corners(s.rect));
        }
        assert_eq!(Shape::new(Rect::new(0, 0, 1, 1), RADIUS).radii, [0; 4]);
        assert!(Shape::new(Rect::new(0, 0, 1, 1), RADIUS).contains((0, 0)));
        let far = Shape::new(
            Rect::new(-4 * POPUP_LIMIT, 4 * POPUP_LIMIT, POPUP_LIMIT, POPUP_LIMIT),
            RADIUS,
        );
        for p in corner_px(far.rect) {
            assert!(!far.contains(p));
        }
        assert!(far.contains((-4 * POPUP_LIMIT + 10, 4 * POPUP_LIMIT + 10)));
        assert_eq!(
            Shape::new(far.rect, i32::MAX).scaled(|r| r, 1e12).radii,
            [POPUP_LIMIT / 2; 4]
        );
    }

    /// AC1: corner px are out; `(r, r)` in from each corner, the centre and the straight edges
    /// past `r` are in.
    #[test]
    fn corners_are_cut_and_the_rest_is_kept() {
        let r = Rect::new(100, 50, 400, 300);
        let s = Shape::new(r, RADIUS);
        let k = RADIUS;
        for (cx, cy) in corner_px(r) {
            assert!(!s.contains((cx, cy)));
            let inward = (
                if cx == r.x { cx + k } else { cx - k },
                if cy == r.y { cy + k } else { cy - k },
            );
            assert!(s.contains(inward), "{inward:?}");
        }
        assert!(s.contains((r.x + r.w / 2, r.y + r.h / 2)));
        for x in r.x + k..r.x + r.w - k {
            assert!(s.contains((x, r.y)) && s.contains((x, r.y + r.h - 1)));
        }
        for y in r.y + k..r.y + r.h - k {
            assert!(s.contains((r.x, y)) && s.contains((r.x + r.w - 1, y)));
        }
        // The arc itself: one px in from the 45° point is in, one out is out.
        let d = f64::from(k) * (1.0 - std::f64::consts::FRAC_1_SQRT_2);
        let on = (r.x + d as i32, r.y + d as i32);
        assert!(s.contains((on.0 + 1, on.1 + 1)));
        assert!(!s.contains((on.0 - 1, on.1 - 1)));
        // A zero radius cuts nothing.
        let square = Shape::with_radii(r, [0, 0, k, k]);
        assert!(square.contains((r.x, r.y)) && square.contains((r.x + r.w - 1, r.y)));
        assert!(!square.contains((r.x, r.y + r.h - 1)));
    }

    /// AC1: the corner squares are exactly the four `r × r` squares; `meets` and
    /// `outside_corners` agree with them.
    #[test]
    fn corner_squares_meets_and_outside_corners() {
        let r = Rect::new(10, 20, 200, 100);
        let s = Shape::new(r, RADIUS);
        assert_eq!(
            s.corners(),
            [
                Rect::new(10, 20, 10, 10),
                Rect::new(200, 20, 10, 10),
                Rect::new(200, 110, 10, 10),
                Rect::new(10, 110, 10, 10),
            ]
        );
        let middle = Rect::new(20, 20, 180, 100);
        assert!(!s.meets(middle), "a rect between the corner squares");
        assert!(!s.meets(Rect::new(10, 30, 200, 80)));
        assert!(s.meets(Rect::new(19, 29, 1, 1)));
        assert!(!s.meets(Rect::new(20, 30, 1, 1)));
        for rect in [
            r,
            Rect::new(0, 0, 300, 300),
            Rect::new(15, 15, 10, 200),
            middle,
        ] {
            let parts = s.outside_corners(rect);
            let area: i32 = parts.iter().map(|p| p.w * p.h).sum();
            let mut want = 0;
            for x in rect.x..rect.x + rect.w {
                for y in rect.y..rect.y + rect.h {
                    let in_corner = s.corners().iter().any(|sq| sq.contains((x, y)));
                    let covered = parts.iter().filter(|p| p.contains((x, y))).count();
                    assert_eq!(covered, usize::from(!in_corner), "({x}, {y}) in {rect:?}");
                    want += i32::from(!in_corner);
                }
            }
            assert_eq!(area, want);
        }
        assert_eq!(s.outside_corners(middle), vec![middle]);
    }

    /// A ring's four rects draw each corner square whole, once; rects of a cut with a hole
    /// that reach only part of a square draw the box around those parts.
    #[test]
    fn pieces_draw_ring_corners_whole_and_lit_corners_to_their_rects() {
        let inner = Rect::new(100, 100, 300, 200);
        let c = Cut::ring(inner, RADIUS, 2);
        let o = c.shape.rect;
        let frame = [
            Rect::new(o.x, o.y, o.w, 2),
            Rect::new(o.x, inner.y + inner.h, o.w, 2),
            Rect::new(o.x, inner.y, 2, inner.h),
            Rect::new(inner.x + inner.w, inner.y, 2, inner.h),
        ];
        let rects: Vec<_> = frame.iter().map(|&r| (r, (), Some(c))).collect();
        let (_, corners) = pieces(&rects);
        let mut got: Vec<Rect> = corners.iter().map(|p| p.1).collect();
        let mut want = c.shape.corners().to_vec();
        got.sort_by_key(|r| (r.x, r.y));
        want.sort_by_key(|r| (r.x, r.y));
        assert_eq!(got, want);
        // An L reaching 6 px left and 4 px below the corner square's inner part.
        let sq = c.shape.corners()[3];
        let left = Rect::new(sq.x, sq.y - 20, 6, sq.h + 16);
        let below = Rect::new(sq.x, sq.y + sq.h - 4, sq.w + 20, 4);
        let (plain, corners) = pieces(&[(left, (), Some(c)), (below, (), Some(c))]);
        assert_eq!(corners.len(), 1);
        assert_eq!(corners[0].1, Rect::new(sq.x, sq.y, sq.w, sq.h));
        let narrow = Rect::new(sq.x, sq.y - 20, 3, sq.h + 16);
        let (_, corners) = pieces(&[(narrow, (), Some(c))]);
        assert_eq!(corners[0].1, Rect::new(sq.x, sq.y, 3, sq.h - 4));
        assert!(plain.iter().all(|p| {
            !c.shape
                .corners()
                .iter()
                .any(|q| intersect(p.1, *q).is_some())
        }));
        // Without a hole each part is its own piece.
        let solid = Cut::solid(c.shape);
        let (_, corners) = pieces(&[(left, (), Some(solid)), (below, (), Some(solid))]);
        assert_eq!(corners.len(), 2);
    }

    /// A frame's ring: outer radius `RADIUS + px`, its hole `RADIUS`, both clamped.
    #[test]
    fn rings_are_concentric() {
        let inner = Rect::new(100, 100, 300, 200);
        for px in [2, 1] {
            let c = Cut::ring(inner, RADIUS, px);
            assert_eq!(c.shape.radii, [RADIUS + px; 4]);
            assert_eq!(c.hole, Some(Shape::new(inner, RADIUS)));
            assert!(
                c.keeps((inner.x - 1, inner.y + RADIUS + px)),
                "straight run"
            );
            assert!(!c.keeps((inner.x + 50, inner.y + 50)), "the hole");
            assert!(!c.keeps((inner.x - px, inner.y - px)), "outer corner");
            assert!(
                c.keeps((inner.x + 2, inner.y + 2)),
                "between the arcs, in the hole's rect"
            );
            assert!(!c.keeps((inner.x, inner.y)), "past both arcs");
        }
        let tiny = Cut::ring(Rect::new(0, 0, 4, 4), RADIUS, 2);
        assert_eq!(tiny.shape.radii, [4; 4]);
        assert_eq!(tiny.hole.unwrap().radii, [2; 4]);
    }
}
