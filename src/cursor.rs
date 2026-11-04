//! The pointer navi draws: the `default` arrow of the first installed XCursor theme. Theme
//! files are untrusted, so every read and size is capped and a bad file is an error, never a
//! panic.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::files::{read_bytes, xdg_dir};

pub const THEMES: [&str; 2] = ["Sweet-cursors", "Adwaita"];
const SHAPES: [&str; 2] = ["default", "left_ptr"];
/// Logical px; times the output scale.
pub const SIZE: f64 = 24.0;
const MAX_FILE: u64 = 2 << 20;
const MAX_INDEX: u64 = 64 << 10;
const MAX_SIDE: u32 = 256;
const MAX_INHERITS: usize = 4;
/// XCursor's image chunk type.
const IMAGE: u32 = 0xfffd_0002;

/// One frame, little-endian ARGB premultiplied (BGRA in memory), as the file stores it.
#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    pub theme: String,
    pub size: u32,
    pub w: u32,
    pub h: u32,
    pub hotspot: (u32, u32),
    pub pixels: Vec<u8>,
}

/// Where themes live, first wins: `$XDG_DATA_HOME/icons`, `~/.icons`, then each
/// `$XDG_DATA_DIRS` entry's `icons`.
pub fn dirs(
    data_home: Option<&OsStr>,
    home: Option<&OsStr>,
    data_dirs: Option<&OsStr>,
) -> Vec<PathBuf> {
    let data_dirs = data_dirs
        .and_then(OsStr::to_str)
        .filter(|d| !d.is_empty())
        .unwrap_or("/usr/local/share:/usr/share");
    let system = data_dirs
        .split(':')
        .map(Path::new)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("icons"));
    xdg_dir(data_home, home, ".local/share")
        .map(|d| d.join("icons"))
        .into_iter()
        .chain(xdg_dir(None, home, ".icons"))
        .chain(system)
        .collect()
}

/// The first of `THEMES` with a `cursors` dir in some dir.
pub fn theme(dirs: &[PathBuf]) -> Option<&'static str> {
    THEMES
        .into_iter()
        .find(|t| dirs.iter().any(|d| d.join(t).join("cursors").is_dir()))
}

/// The arrow's file: `default`, else `left_ptr`, each looked for in `theme` and then the themes
/// it inherits, at most `MAX_INHERITS` deep.
pub fn file(dirs: &[PathBuf], theme: &str) -> Option<PathBuf> {
    let chain = chain(dirs, theme);
    SHAPES.iter().find_map(|shape| {
        chain.iter().find_map(|t| {
            dirs.iter()
                .map(|d| d.join(t).join("cursors").join(shape))
                .find(|p| p.is_file())
        })
    })
}

/// `theme`, then what it inherits, breadth first, each once.
fn chain(dirs: &[PathBuf], theme: &str) -> Vec<String> {
    let mut seen = HashSet::from([theme.to_string()]);
    let mut out = vec![theme.to_string()];
    let mut level = vec![theme.to_string()];
    for _ in 0..MAX_INHERITS {
        let next: Vec<String> = level
            .iter()
            .flat_map(|t| inherits(dirs, t))
            .filter(|t| seen.insert(t.clone()))
            .collect();
        out.extend(next.iter().cloned());
        level = next;
    }
    out
}

/// The `Inherits=` of the first `index.theme` of `theme`.
fn inherits(dirs: &[PathBuf], theme: &str) -> Vec<String> {
    let Some(text) = dirs.iter().find_map(|d| {
        let bytes = read_bytes(&d.join(theme).join("index.theme"), MAX_INDEX).ok()??;
        String::from_utf8(bytes).ok()
    }) else {
        return Vec::new();
    };
    text.lines()
        .find_map(|l| l.trim().strip_prefix("Inherits="))
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty() && !t.contains('/') && *t != "." && *t != "..")
        .map(String::from)
        .collect()
}

/// The arrow for output `scale` from the user's themes; `None`, after one warning, keeps the
/// host's or no cursor.
pub fn from_env(scale: f64) -> Option<Cursor> {
    let var = std::env::var_os;
    let dirs = dirs(
        var("XDG_DATA_HOME").as_deref(),
        var("HOME").as_deref(),
        var("XDG_DATA_DIRS").as_deref(),
    );
    let Some(theme) = theme(&dirs) else {
        warn!(
            ?dirs,
            "no cursor theme of {THEMES:?} found: navi draws no cursor"
        );
        return None;
    };
    let loaded = file(&dirs, theme)
        .ok_or_else(|| "no default or left_ptr cursor".to_string())
        .and_then(|p| load(&p, theme, scale).map_err(|e| format!("{}: {e}", p.display())));
    match loaded {
        Ok(c) => {
            let (x, y) = c.hotspot;
            info!("cursor theme={theme} size={} hotspot=({x},{y})", c.size);
            Some(c)
        }
        Err(e) => {
            warn!("cursor theme {theme} not loaded, navi draws no cursor: {e}");
            None
        }
    }
}

/// The first frame of the size nearest `SIZE × scale` in the file at `path`.
pub fn load(path: &Path, theme: &str, scale: f64) -> Result<Cursor, String> {
    let bytes = read_bytes(path, MAX_FILE)?.ok_or("no file")?;
    decode(&bytes, theme, (SIZE * scale).round() as u32)
}

fn decode(bytes: &[u8], theme: &str, want: u32) -> Result<Cursor, String> {
    check(bytes)?;
    let images = xcursor::parser::parse_xcursor(bytes).ok_or("not an XCursor file")?;
    let size = images
        .iter()
        .map(|i| i.size)
        .min_by_key(|&s| (s.abs_diff(want), u32::MAX - s))
        .ok_or("no images")?;
    let img = images
        .into_iter()
        .find(|i| i.size == size)
        .ok_or("no images")?;
    if img.xhot >= img.width || img.yhot >= img.height {
        return Err("hotspot outside the image".into());
    }
    Ok(Cursor {
        theme: theme.into(),
        size,
        w: img.width,
        h: img.height,
        hotspot: (img.xhot, img.yhot),
        pixels: img.pixels_rgba,
    })
}

/// Bounds the parser's allocations before it runs: it trusts the table of contents' length and
/// each image's width and height.
fn check(b: &[u8]) -> Result<(), String> {
    let u32_at = |at: usize| -> Result<u32, String> {
        b.get(at..at + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
            .ok_or_else(|| "truncated".to_string())
    };
    if b.get(..4) != Some(b"Xcur") {
        return Err("not an XCursor file".into());
    }
    let (header, ntoc) = (u32_at(4)? as usize, u32_at(12)? as usize);
    if ntoc > b.len() / 12 {
        return Err("truncated".into());
    }
    for k in 0..ntoc {
        let toc = header + 12 * k;
        if u32_at(toc)? != IMAGE {
            continue;
        }
        let pos = u32_at(toc + 8)? as usize;
        let (w, h) = (u32_at(pos + 16)?, u32_at(pos + 20)?);
        if w > MAX_SIDE || h > MAX_SIDE {
            return Err(format!("{w}×{h} image, over {MAX_SIDE} px"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A fresh empty dir, removed on drop.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(name: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!("navi-cursor-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir(&p).unwrap();
            Tmp(p)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// An XCursor file with one `side`² image per `(size, side, hotspot)`, each pixel's blue
    /// byte its image's index.
    fn xcursor(images: &[(u32, u32, (u32, u32))]) -> Vec<u8> {
        let mut out = Vec::new();
        let n = images.len() as u32;
        for v in [u32::from_le_bytes(*b"Xcur"), 16, 0x1_0000, n] {
            out.extend(v.to_le_bytes());
        }
        let mut pos = 16 + 12 * n;
        for &(size, side, _) in images {
            out.extend([IMAGE, size, pos].iter().flat_map(|v| v.to_le_bytes()));
            pos += 36 + 4 * side * side;
        }
        for (k, &(size, side, (x, y))) in images.iter().enumerate() {
            for v in [36, IMAGE, size, 1, side, side, x, y, 50] {
                out.extend(v.to_le_bytes());
            }
            out.extend([k as u8, 0, 0, 255].repeat((side * side) as usize));
        }
        out
    }

    fn theme_dir(base: &Path, name: &str, inherits: Option<&str>, shapes: &[&str]) {
        let cursors = base.join(name).join("cursors");
        fs::create_dir_all(&cursors).unwrap();
        if let Some(i) = inherits {
            let index = format!("[Icon Theme]\nName={name}\nInherits={i}\n");
            fs::write(base.join(name).join("index.theme"), index).unwrap();
        }
        for s in shapes {
            fs::write(cursors.join(s), xcursor(&[(24, 24, (1, 1))])).unwrap();
        }
    }

    #[test]
    fn ac4_dirs_from_the_environment() {
        let os = |s: &str| Some(OsStr::new(s).to_owned());
        let d = dirs(
            os("/x").as_deref(),
            os("/h").as_deref(),
            os("/a:rel:/b").as_deref(),
        );
        let want = ["/x/icons", "/h/.icons", "/a/icons", "/b/icons"];
        assert_eq!(d, want.map(PathBuf::from));
        let d = dirs(None, os("/h").as_deref(), None);
        let want = [
            "/h/.local/share/icons",
            "/h/.icons",
            "/usr/local/share/icons",
            "/usr/share/icons",
        ];
        assert_eq!(d, want.map(PathBuf::from));
    }

    #[test]
    fn ac4_theme_lookup() {
        let tmp = Tmp::new("lookup");
        let (home, dot, sys) = (tmp.0.join("home"), tmp.0.join("dot"), tmp.0.join("sys"));
        let all = vec![home.clone(), dot.clone(), sys.clone()];
        assert_eq!(theme(&all), None);
        theme_dir(&sys, "Adwaita", None, &["default"]);
        assert_eq!(theme(&all), Some("Adwaita"));
        theme_dir(&dot, "Sweet-cursors", None, &["default"]);
        assert_eq!(theme(&all), Some("Sweet-cursors"));
        let sweet = |d: &Path| d.join("Sweet-cursors/cursors/default");
        assert_eq!(file(&all, "Sweet-cursors"), Some(sweet(&dot)));
        // The XDG_DATA_HOME copy wins over ~/.icons.
        theme_dir(&home, "Sweet-cursors", None, &["default"]);
        assert_eq!(file(&all, "Sweet-cursors"), Some(sweet(&home)));
    }

    #[test]
    fn ac4_inherits_cycles_and_left_ptr() {
        let tmp = Tmp::new("inherit");
        let d = vec![tmp.0.clone()];
        // A theme with only an index finds `default` in its parent.
        theme_dir(&tmp.0, "child", Some("mid, nothing"), &[]);
        theme_dir(&tmp.0, "mid", Some("base"), &[]);
        theme_dir(&tmp.0, "base", None, &["default"]);
        assert_eq!(file(&d, "child"), Some(tmp.0.join("base/cursors/default")));
        // A cycle ends.
        theme_dir(&tmp.0, "a", Some("b"), &[]);
        theme_dir(&tmp.0, "b", Some("a,../../etc"), &[]);
        assert_eq!(file(&d, "a"), None);
        assert_eq!(chain(&d, "a"), ["a", "b"]);
        // No `default` anywhere: `left_ptr`.
        theme_dir(&tmp.0, "old", None, &["left_ptr"]);
        assert_eq!(file(&d, "old"), Some(tmp.0.join("old/cursors/left_ptr")));
        // At most 4 levels below the theme.
        for (t, i) in [
            ("l0", "l1"),
            ("l1", "l2"),
            ("l2", "l3"),
            ("l3", "l4"),
            ("l4", "l5"),
        ] {
            theme_dir(&tmp.0, t, Some(i), &[]);
        }
        theme_dir(&tmp.0, "l5", None, &["default"]);
        assert_eq!(file(&d, "l1"), Some(tmp.0.join("l5/cursors/default")));
        assert_eq!(file(&d, "l0"), None);
    }

    #[test]
    fn ac5_nearest_size_first_frame() {
        let sizes = xcursor(&[
            (24, 24, (1, 2)),
            (36, 36, (3, 4)),
            (48, 48, (5, 6)),
            (48, 48, (7, 7)),
        ]);
        let at = |want| decode(&sizes, "t", want).unwrap();
        let px = |scale: f64| (SIZE * scale).round() as u32;
        assert_eq!(at(px(1.0)).size, 24);
        assert_eq!(at(px(1.6)).size, 36);
        let c = at(px(2.0));
        assert_eq!((c.size, c.w, c.h, c.hotspot), (48, 48, 48, (5, 6)));
        assert_eq!(c.pixels.len(), 48 * 48 * 4);
        assert_eq!(c.pixels[..4], [2, 0, 0, 255], "the first 48 px frame");
        // 30 is as near 24 as 36: the larger.
        assert_eq!(at(30).size, 36);
    }

    #[test]
    fn ac5_bad_files_are_errors() {
        let tmp = Tmp::new("bad");
        let big = tmp.0.join("big");
        let mut bytes = xcursor(&[(24, 24, (0, 0))]);
        bytes.resize(MAX_FILE as usize + 1, 0);
        fs::write(&big, &bytes).unwrap();
        assert!(load(&big, "t", 1.0).is_err());
        bytes.truncate(MAX_FILE as usize);
        fs::write(&big, &bytes).unwrap();
        assert_eq!(load(&big, "t", 1.0).unwrap().size, 24);

        assert!(decode(&xcursor(&[(256, 256, (0, 0))]), "t", 24).is_ok());
        assert!(decode(&xcursor(&[(24, 257, (0, 0))]), "t", 24).is_err());
        assert!(decode(&xcursor(&[(24, 24, (24, 0))]), "t", 24).is_err());
        assert!(decode(&xcursor(&[(24, 24, (0, 24))]), "t", 24).is_err());
        assert!(decode(&xcursor(&[]), "t", 24).is_err());
        // Claimed sizes the bytes can't hold are refused before the parser allocates for them.
        let mut huge = xcursor(&[(24, 24, (0, 0))]);
        huge[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&huge, "t", 24).is_err());
        let mut wide = xcursor(&[(24, 24, (0, 0))]);
        let w = 16 + 12 + 16;
        wide[w..w + 4].copy_from_slice(&0x7fffu32.to_le_bytes());
        assert!(decode(&wide, "t", 24).is_err());
        for n in 0..wide.len() {
            let _ = decode(&wide[..n], "t", 24);
        }
    }
}
