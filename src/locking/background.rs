//! Wallpaper-only lock backdrop. Prepared on a worker, never from desktop contents.
use crate::{
    image::{self, Image},
    wallpaper,
};
use std::{
    fs::{self, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};

pub struct Background {
    pub name: String,
    pub path: PathBuf,
}
impl Drop for Background {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn prepare(outputs: Vec<(String, u32, i32, i32)>) -> Vec<Background> {
    let names: Vec<_> = outputs
        .iter()
        .map(|(n, p, _, _)| (n.as_str(), *p))
        .collect();
    let images = wallpaper::open(&names);
    outputs
        .into_iter()
        .zip(images)
        .filter_map(
            |((name, _, w, h), image)| match create(name, image.as_deref(), w, h) {
                Ok(bg) => Some(bg),
                Err(e) => {
                    tracing::warn!("lock backdrop unavailable; using solid protection: {e}");
                    None
                }
            },
        )
        .collect()
}

pub fn create(
    name: String,
    image: Option<&Image>,
    width: i32,
    height: i32,
) -> io::Result<Background> {
    use resvg::tiny_skia::{FilterQuality, IntSize, Pixmap, PixmapPaint, Transform};
    let (width, height) = (width.max(1) as u32, height.max(1) as u32);
    if width > image::MAX_SIDE
        || height > image::MAX_SIDE
        || u64::from(width) * u64::from(height) > image::MAX_AREA
    {
        return Err(io::Error::other("lock backdrop exceeds image size cap"));
    }
    let (w, h, pixels) = render(image, width as i32, height as i32);
    let backdrop = Pixmap::from_vec(pixels, IntSize::from_wh(w as u32, h as u32).unwrap()).unwrap();
    let mut canvas = Pixmap::new(width, height)
        .ok_or_else(|| io::Error::other("lock backdrop allocation failed"))?;
    let paint = PixmapPaint {
        quality: FilterQuality::Bilinear,
        ..Default::default()
    };
    canvas.draw_pixmap(
        0,
        0,
        backdrop.as_ref(),
        &paint,
        Transform::from_scale(width as f32 / w as f32, height as f32 / h as f32),
        None,
    );
    let mut prompts = image::decode(include_bytes!("../../assets/lock/prompts.png"), 4096)
        .map_err(io::Error::other)?;
    for p in prompts.rgba.as_chunks_mut::<4>().0 {
        let a = p[3] as u16;
        for c in &mut p[..3] {
            *c = ((*c as u16 * a + 127) / 255) as u8;
        }
    }
    let scale = (width as f32 / prompts.w as f32).min(height as f32 / prompts.h as f32);
    let at = Transform::from_scale(scale, scale).post_translate(
        (width as f32 - prompts.w as f32 * scale) / 2.0,
        (height as f32 - prompts.h as f32 * scale) / 2.0,
    );
    let prompts = Pixmap::from_vec(
        prompts.rgba,
        IntSize::from_wh(prompts.w, prompts.h).unwrap(),
    )
    .unwrap();
    canvas.draw_pixmap(0, 0, prompts.as_ref(), &paint, at, None);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let path = std::env::temp_dir().join(format!("navi-lock-{}-{stamp}.png", std::process::id()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let background = Background { name, path };
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(canvas.data())?;
    writer.finish()?;
    Ok(background)
}

fn render(image: Option<&Image>, width: i32, height: i32) -> (usize, usize, Vec<u8>) {
    // ponytail: blur a bounded thumbnail once, then scale the backdrop and add
    // crisp prompts. No screenshot protocol, imaging process or new dependency.
    let width = width.max(1);
    let height = height.max(1);
    let scale = (960.0 / width.max(height) as f64).min(1.0);
    let (w, h) = (
        ((width as f64 * scale) as usize).max(1),
        ((height as f64 * scale) as usize).max(1),
    );
    let mut pixels = vec![0; w * h * 4];
    if let Some(image) = image {
        let crop = wallpaper::cover(image.w as i32, image.h as i32, width, height);
        for y in 0..h {
            for x in 0..w {
                let sx = crop.x as usize + x * crop.w as usize / w;
                let sy = crop.y as usize + y * crop.h as usize / h;
                let from = (sy * image.w as usize + sx) * 4;
                let to = (y * w + x) * 4;
                let alpha = image.rgba[from + 3] as f32 / 255.0;
                for c in 0..3 {
                    pixels[to + c] = (image.rgba[from + c] as f32 * alpha * 0.27) as u8;
                }
                pixels[to + 3] = 255;
            }
        }
        // Three separable box passes approximate a soft Gaussian. Fixed running
        // sums make cost linear in the thumbnail's area, independent of radius.
        let mut scratch = pixels.clone();
        for _ in 0..3 {
            blur(&pixels, &mut scratch, w, h, true);
            blur(&scratch, &mut pixels, w, h, false);
        }
    } else {
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[11, 13, 18, 255]);
        }
    }
    (w, h, pixels)
}

fn blur(input: &[u8], output: &mut [u8], w: usize, h: usize, horizontal: bool) {
    const R: isize = 10;
    let (lines, length) = if horizontal { (h, w) } else { (w, h) };
    for line in 0..lines {
        let index = |p: isize| {
            let p = p.clamp(0, length as isize - 1) as usize;
            if horizontal {
                (line * w + p) * 4
            } else {
                (p * w + line) * 4
            }
        };
        let mut sum = [0u32; 3];
        for p in -R..=R {
            let i = index(p);
            for c in 0..3 {
                sum[c] += input[i + c] as u32;
            }
        }
        for p in 0..length as isize {
            let i = index(p);
            let old = index(p - R);
            let new = index(p + R + 1);
            for c in 0..3 {
                output[i + c] = (sum[c] / (2 * R + 1) as u32) as u8;
                sum[c] = sum[c] + input[new + c] as u32 - input[old + c] as u32;
            }
            output[i + 3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backdrop_is_dark_blurred_bounded_and_missing_wallpaper_stays_opaque() {
        let image = Image {
            w: 100,
            h: 60,
            rgba: (0..6000)
                .flat_map(|p| {
                    if p % 100 < 50 {
                        [255, 255, 255, 255]
                    } else {
                        [0, 0, 0, 255]
                    }
                })
                .collect(),
        };
        let (w, h, pixels) = render(Some(&image), 100, 60);
        assert_eq!((w, h), (100, 60));
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[3] == 255 && p[0] <= 69)
        );
        assert!(pixels[(30 * w + 49) * 4] > 0 && pixels[(30 * w + 49) * 4] < 68);
        assert!(pixels[(30 * w + 50) * 4] > 0 && pixels[(30 * w + 50) * 4] < 68);
        let (w, h, pixels) = render(None, 7680, 4320);
        assert!(w <= 960 && h <= 960);
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [11, 13, 18, 255])
        );
    }
}
