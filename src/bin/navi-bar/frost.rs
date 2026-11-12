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

/// The frost of the output navi-bar is on, by the output's name. Its logical size is not kept:
/// xdg-output may report it after the name, so each draw passes the current one.
#[derive(Default)]
pub struct Frost {
    name: Option<String>,
    image: Option<Pixmap>,
}

impl Frost {
    /// The output's current name; loads its file when the name changed or `reload` (navi wrote
    /// new files). Returns whether the image may have changed.
    pub fn update(
        &mut self,
        name: Option<&str>,
        reload: bool,
        load: impl FnOnce(&str) -> Option<Pixmap>,
    ) -> bool {
        if !reload && self.name.as_deref() == name {
            return false;
        }
        self.name = name.map(str::to_string);
        self.image = name.and_then(load);
        true
    }

    /// The backdrop of a surface at `at` on an output of logical `size`, if known yet.
    pub fn backdrop(&self, size: Option<(i32, i32)>, at: (i32, i32)) -> Backdrop<'_> {
        Backdrop {
            frost: self.image.as_ref(),
            output: size.unwrap_or((0, 0)),
            at,
        }
    }
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

    /// Whether the body shows frost: there is an image and the output's size is known.
    pub fn frosted(&self) -> bool {
        self.frost.is_some() && self.output.0 > 0 && self.output.1 > 0
    }

    /// The body's paint at buffer scale `s`.
    pub fn paint(&self, s: i32) -> Paint<'_> {
        let mut paint = Paint::default();
        match (self.frost, self.output) {
            (Some(img), (w, h)) if self.frosted() => {
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

    /// A FIFO swapped in for the file between any check and the open must not block navi-bar.
    #[test]
    fn ac9_a_fifo_swapped_in_never_blocks_the_load() {
        use std::os::unix::fs::symlink;
        use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering::Relaxed, mpsc};
        let tmp = Tmp::new("frost-fifo");
        let dir = tmp.0.clone();
        fs::write(dir.join("reg"), png(8, 4)).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(dir.join("fifo"))
            .status()
            .unwrap();
        assert!(made.success());
        assert!(load(&dir, "fifo").is_none(), "a FIFO is not a frost");
        symlink("reg", dir.join("x.png")).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (stopped, d) = (stop.clone(), dir.clone());
        // Atomically flips x.png between the PNG and the FIFO.
        let swapper = std::thread::spawn(move || {
            for i in 0u64.. {
                if stopped.load(Relaxed) {
                    break;
                }
                let _ = fs::remove_file(d.join("n"));
                symlink(if i % 2 == 0 { "fifo" } else { "reg" }, d.join("n")).unwrap();
                fs::rename(d.join("n"), d.join("x.png")).unwrap();
            }
        });
        let (done, finished) = mpsc::channel();
        let d = dir.clone();
        std::thread::spawn(move || {
            for _ in 0..20_000 {
                let _ = load(&d, "x");
            }
            let _ = done.send(());
        });
        let result = finished.recv_timeout(std::time::Duration::from_secs(30));
        if result.is_err() {
            // Unblock the stuck open so the test process can end.
            let _ = fs::OpenOptions::new().write(true).open(dir.join("fifo"));
        }
        stop.store(true, Relaxed);
        swapper.join().unwrap();
        assert!(result.is_ok(), "load() blocked opening a FIFO");
    }

    /// Review round 1: navi-bar learns the output's name before its xdg-output size. The frost
    /// must show once the size arrives, without waiting for another file change.
    #[test]
    fn frost_shows_once_the_size_arrives_after_the_name() {
        let mut f = Frost::default();
        let mut loads = 0;
        let mut load = |_: &str| {
            loads += 1;
            Pixmap::new(4, 2)
        };
        assert!(!f.update(None, false, &mut load));
        assert!(!f.backdrop(None, (0, 0)).frosted(), "no output yet");
        assert!(f.update(Some("DP-1"), false, &mut load));
        assert!(!f.backdrop(None, (0, 0)).frosted(), "name only");
        // The size arrives: no reload, the same image, now frosted.
        assert!(!f.update(Some("DP-1"), false, &mut load));
        assert!(f.backdrop(Some((3440, 1440)), (12, 12)).frosted());
        // navi wrote new files; then the surfaces move to another output.
        assert!(f.update(Some("DP-1"), true, &mut load));
        assert!(f.update(Some("DP-2"), false, &mut load));
        assert_eq!(loads, 3);
        // An output without a frost file: solid.
        assert!(f.update(Some("DP-3"), false, |_| None));
        assert!(!f.backdrop(Some((3440, 1440)), (0, 0)).frosted());
    }
}
