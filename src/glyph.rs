//! Window-button glyphs for navi's bars: Breeze-like line chevrons and a cross, antialiased.

use resvg::tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform,
};

use crate::{layout::Button, theme};

/// In logical px of a 24 px button: the glyph's half-width, and the stroke.
const HALF: f32 = 4.5;
const STROKE: f32 = 1.5;
/// The rounded hover background, inset from the button.
const HOVER_INSET: f32 = 2.0;
const HOVER_RADIUS: f32 = 5.0;

/// `b` as a `size` px square of premultiplied RGBA, in INK, on a rounded SELECTED fill when
/// `lit`. INK holds 4.5:1 on the bar and on SELECTED in every scheme (theme tests).
pub fn button(b: Button, lit: bool, size: u32) -> Option<Pixmap> {
    let mut px = Pixmap::new(size, size)?;
    let k = size as f32 / crate::layout::BAR as f32;
    let paint = |rgb: u32| {
        let [_, r, g, b] = theme::resolve(rgb).to_be_bytes();
        let mut p = Paint::default();
        p.set_color_rgba8(r, g, b, 255);
        p.anti_alias = true;
        p
    };
    if lit {
        let (i, w) = (HOVER_INSET * k, size as f32 - 2.0 * HOVER_INSET * k);
        let path = round(i, w, HOVER_RADIUS * k)?;
        let p = paint(theme::SELECTED);
        px.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
    }
    let c = size as f32 / 2.0;
    let (h, q) = (HALF * k, HALF * k / 2.0);
    let mut path = PathBuilder::new();
    match b {
        Button::Minimize | Button::Maximize => {
            // A chevron pointing down to minimize, up to maximize.
            let d = if b == Button::Minimize { q } else { -q };
            path.move_to(c - h, c - d);
            path.line_to(c, c + d);
            path.line_to(c + h, c - d);
        }
        Button::Close => {
            path.move_to(c - h, c - h);
            path.line_to(c + h, c + h);
            path.move_to(c + h, c - h);
            path.line_to(c - h, c + h);
        }
    }
    let stroke = Stroke {
        width: STROKE * k,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    px.stroke_path(
        &path.finish()?,
        &paint(theme::INK),
        &stroke,
        Transform::identity(),
        None,
    );
    Some(px)
}

/// A rounded square `w` px wide at (`at`, `at`).
fn round(at: f32, w: f32, r: f32) -> Option<resvg::tiny_skia::Path> {
    let k = r * 0.552_284_8;
    let (x0, x1) = (at, at + w);
    let mut p = PathBuilder::new();
    p.move_to(x0 + r, x0);
    p.line_to(x1 - r, x0);
    p.cubic_to(x1 - r + k, x0, x1, x0 + r - k, x1, x0 + r);
    p.line_to(x1, x1 - r);
    p.cubic_to(x1, x1 - r + k, x1 - r + k, x1, x1 - r, x1);
    p.line_to(x0 + r, x1);
    p.cubic_to(x0 + r - k, x1, x0, x1 - r + k, x0, x1 - r);
    p.line_to(x0, x0 + r);
    p.cubic_to(x0, x0 + r - k, x0 + r - k, x0, x0 + r, x0);
    p.close();
    p.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(px: &Pixmap, x: u32, y: u32) -> u8 {
        px.pixel(x, y).unwrap().alpha()
    }

    /// The colour of `px`'s fully covered pixels: one colour, or none.
    fn solid(px: &Pixmap) -> Vec<u32> {
        let mut out: Vec<u32> = px
            .pixels()
            .iter()
            .filter(|p| p.alpha() == 255)
            .map(|p| u32::from(p.red()) << 16 | u32::from(p.green()) << 8 | u32::from(p.blue()))
            .collect();
        out.dedup();
        out
    }

    #[test]
    fn breeze_like_glyphs_in_ink() {
        let (ink, selected) = (theme::resolve(theme::INK), theme::resolve(theme::SELECTED));
        for size in [24, 36, 48] {
            let c = size / 2;
            let k = size as f32 / 24.0;
            let (below, above) = (c + (2.25 * k) as u32, c - (2.25 * k).ceil() as u32);
            // Minimize points down (tip below the centre), maximize up.
            let min = button(Button::Minimize, false, size).unwrap();
            let max = button(Button::Maximize, false, size).unwrap();
            assert!(
                alpha(&min, c, below) > 100 && alpha(&min, c, above) == 0,
                "{size}"
            );
            assert!(
                alpha(&max, c, above) > 100 && alpha(&max, c, below) == 0,
                "{size}"
            );
            // Close is a plain cross: inked at the centre, clear above it.
            let close = button(Button::Close, false, size).unwrap();
            assert!(alpha(&close, c, c) > 200);
            assert_eq!(alpha(&close, c, c - (4.0 * k) as u32), 0);
            for px in [&min, &max, &close] {
                assert!(solid(px).iter().all(|&rgb| rgb == ink), "{size}: only INK");
                // Antialiased, and clear away from the glyph.
                assert!(px.pixels().iter().any(|p| p.alpha() > 0 && p.alpha() < 255));
                assert_eq!(alpha(px, 2, 2), 0);
            }
            // Lit: a rounded SELECTED square with clear corners under the INK glyph.
            let lit = button(Button::Close, true, size).unwrap();
            let p = lit.pixel((3.0 * k) as u32, c).unwrap();
            assert_eq!((p.red(), p.green(), p.blue(), p.alpha()), {
                let [_, r, g, b] = selected.to_be_bytes();
                (r, g, b, 255)
            });
            assert_eq!(alpha(&lit, 0, 0), 0);
            assert_eq!(
                alpha(&lit, (2.5 * k) as u32, (2.5 * k) as u32),
                0,
                "rounded"
            );
        }
    }
}
