//! The frosted wallpaper under the panels: navi's blurred image of the output, read as untrusted
//! input, since any process of the user can write the runtime dir.

use std::path::Path;

use resvg::tiny_skia::{
    Color, FilterQuality, IntSize, Paint, Pattern, Pixmap, Shader, SpreadMode, Transform,
};
use tracing::warn;

use crate::{files, image, theme};

const MAX_FILE: u64 = 8 << 20;
const MAX_SIDE: u32 = 1024;

/// Output `name`'s frost in `dir`. `None`, and solid panels, if there is none or it's unusable.
pub fn load(dir: &Path, name: &str) -> Option<Pixmap> {
    let path = files::frost_file(dir, name)?;
    decode(&path)
        .inspect_err(|e| warn!(path = %path.display(), "frost ignored: {e}"))
        .ok()
        .flatten()
}

fn decode(path: &Path) -> Result<Option<Pixmap>, String> {
    let Some(bytes) = files::read_bytes(path, MAX_FILE)? else {
        return Ok(None);
    };
    let mut img = image::decode(&bytes, MAX_SIDE)?;
    // Opaque whatever the file says, so window text never shows through a panel.
    for p in img.rgba.as_chunks_mut::<4>().0 {
        p[3] = 255;
    }
    let size = IntSize::from_wh(img.w, img.h).ok_or("empty image")?;
    Ok(Pixmap::from_vec(img.rgba, size))
}

/// What a surface's panel bodies show: the output's frost stretched over the output's logical
/// size, seen from the surface's top-left `at` on it. Without frost, solid `BG`.
#[derive(Clone, Copy)]
pub struct Backdrop<'a> {
    pub frost: Option<&'a Pixmap>,
    pub output: (i32, i32),
    pub at: (i32, i32),
}

impl Backdrop<'_> {
    #[cfg(test)]
    pub const SOLID: Backdrop<'static> = Backdrop {
        frost: None,
        output: (0, 0),
        at: (0, 0),
    };

    /// The body's paint at buffer scale `s`.
    pub fn paint(&self, s: i32) -> Paint<'_> {
        let mut paint = Paint::default();
        match (self.frost, self.output) {
            (Some(img), (w, h)) if w > 0 && h > 0 => {
                let s = s as f32;
                let at = Transform::from_scale(
                    w as f32 * s / img.width() as f32,
                    h as f32 * s / img.height() as f32,
                )
                .post_translate(-self.at.0 as f32 * s, -self.at.1 as f32 * s);
                paint.shader = Pattern::new(
                    img.as_ref(),
                    SpreadMode::Pad,
                    FilterQuality::Bilinear,
                    1.0,
                    at,
                );
            }
            _ => {
                let [_, r, g, b] = theme::resolve(theme::BG).to_be_bytes();
                paint.shader = Shader::SolidColor(Color::from_rgba8(r, g, b, 255));
            }
        }
        paint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Tmp;
    use std::fs;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().unwrap();
        writer
            .write_image_data(&[90, 20, 200, 0].repeat((w * h) as usize))
            .unwrap();
        writer.finish().unwrap();
        out
    }

    #[test]
    fn ac9_hostile_frost_files_fall_back_to_solid() {
        let tmp = Tmp::new("frost-ac9");
        let dir = &tmp.0;
        let good = png(8, 4);
        fs::write(dir.join("ok.png"), &good).unwrap();
        let img = load(dir, "ok").expect("a valid frost loads");
        assert_eq!((img.width(), img.height()), (8, 4));
        // A transparent file still makes an opaque body.
        assert!(img.pixels().iter().all(|p| p.alpha() == 255));

        fs::write(dir.join("big.png"), vec![0u8; 9 << 20]).unwrap();
        fs::write(dir.join("huge.png"), png(4096, 4096)).unwrap();
        fs::write(dir.join("cut.png"), &good[..good.len() / 2]).unwrap();
        fs::create_dir(dir.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(dir.join("elsewhere"), dir.join("link.png")).unwrap();
        let noise: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        fs::write(dir.join("noise.png"), noise).unwrap();
        for name in [
            "big", "huge", "cut", "link", "noise", "missing", "../ok", "",
        ] {
            assert!(load(dir, name).is_none(), "{name}");
        }
        assert!(load(&dir.join("gone"), "ok").is_none(), "a deleted dir");
    }
}
