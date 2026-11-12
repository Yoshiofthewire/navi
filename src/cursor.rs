//! The pointer navi draws: named shapes of the first installed XCursor theme, the `default`
//! arrow standing in for any it lacks. Theme files are untrusted, so every read and size is capped and a bad file is an error, never a
//! panic.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use smithay::input::pointer::CursorIcon;
use tracing::{info, warn};

use crate::files::{read_bytes, xdg_dir};

pub const THEMES: [&str; 2] = ["Sweet-cursors", "Adwaita"];
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

/// File names for shape `name`: its CSS name, then its legacy aliases. Names come only from
/// navi's fixed tables and `CursorIcon`, never from a client.
fn names(name: &'static str) -> Vec<&'static str> {
    let icon = name.parse::<CursorIcon>().ok();
    let alt = icon.iter().flat_map(|i| i.alt_names().iter().copied());
    let extra: &[&'static str] = if name == "grabbing" { &["fleur"] } else { &[] };
    std::iter::once(name)
        .chain(alt)
        .chain(extra.iter().copied())
        .collect()
}

/// The file of shape `name`: each of its names is looked for through the whole `chain` before the
/// next.
fn file(dirs: &[PathBuf], chain: &[String], name: &'static str) -> Option<PathBuf> {
    names(name).into_iter().find_map(|n| {
        chain.iter().find_map(|t| {
            dirs.iter()
                .map(|d| d.join(t).join("cursors").join(n))
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

/// What a client asked the pointer to look like over its surface, `S` its cursor surface.
#[derive(Clone, Debug, PartialEq)]
pub enum Request<S> {
    Named(&'static str),
    Hidden,
    Surface(S),
}

impl<S> Default for Request<S> {
    fn default() -> Self {
        Request::Named("default")
    }
}

/// The request of the client under the pointer, `F` its surface.
pub struct Requested<F> {
    focus: Option<F>,
    pub request: Request<F>,
}

impl<F> Default for Requested<F> {
    fn default() -> Self {
        Requested {
            focus: None,
            request: Request::default(),
        }
    }
}

impl<F: PartialEq> Requested<F> {
    /// The pointer focus is now `focus`: a change forgets the old client's request.
    pub fn focus(&mut self, focus: Option<F>) {
        if self.focus != focus {
            self.focus = focus;
            self.request = Request::default();
        }
    }
}

/// What to draw at the pointer.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape<S> {
    Named(&'static str),
    /// The client's cursor surface.
    Client(S),
    Hidden,
}

/// The shape to draw, top rule first: while locked the lock client's request; a drag's cursor;
/// over a Top or Overlay layer its client's; over a zone the zone's; else the client's under the
/// pointer (`default` once the focus moved off it). A cursor surface is drawn only in that last
/// case; locked or over a layer it is `default`.
pub fn pick<'a, S>(
    lock: bool,
    drag: Option<&'static str>,
    layer: bool,
    zone: Option<&'static str>,
    request: &'a Request<S>,
) -> Shape<&'a S> {
    let named = match request {
        Request::Named(n) => Shape::Named(n),
        Request::Hidden => Shape::Hidden,
        Request::Surface(_) => Shape::Named("default"),
    };
    if lock {
        return named;
    }
    match (drag, layer, zone, request) {
        (Some(d), ..) => Shape::Named(d),
        (None, true, ..) => named,
        (None, false, Some(z), _) => Shape::Named(z),
        (.., Request::Surface(s)) => Shape::Client(s),
        _ => named,
    }
}

/// Largest buffer side of a client cursor surface, in px.
const MAX_BUFFER: i32 = 1024;

/// Whether navi draws a client's cursor surface: `size` (logical) at most `MAX_SIDE` and its
/// `buffer` at most `MAX_BUFFER` px a side, both at least 1×1, the hotspot inside, no
/// subsurfaces.
pub fn fits(size: (i32, i32), buffer: (i32, i32), hotspot: (i32, i32), subsurfaces: bool) -> bool {
    let side = |v: i32, max: i32| (1..=max).contains(&v);
    let logical = MAX_SIDE as i32;
    !subsurfaces
        && side(size.0, logical)
        && side(size.1, logical)
        && side(buffer.0, MAX_BUFFER)
        && side(buffer.1, MAX_BUFFER)
        && (0..size.0).contains(&hotspot.0)
        && (0..size.1).contains(&hotspot.1)
}

/// One theme's shapes at one size. Each shape's file is read at most once, hit or miss; a missing
/// or bad shape draws as the arrow.
pub struct Shapes<T> {
    pub theme: String,
    dirs: Vec<PathBuf>,
    chain: Vec<String>,
    want: u32,
    arrow: T,
    found: HashMap<&'static str, Option<T>>,
}

impl<T> Shapes<T> {
    /// The shapes of `theme` nearest `want` px; an error without an arrow.
    pub fn new(
        dirs: Vec<PathBuf>,
        theme: &str,
        want: u32,
        make: impl FnOnce(&Cursor) -> T,
    ) -> Result<Self, String> {
        let chain = chain(&dirs, theme);
        let arrow =
            read(&dirs, &chain, theme, want, "default")?.ok_or("no default or left_ptr cursor")?;
        let (x, y) = arrow.hotspot;
        info!("cursor theme={theme} size={} hotspot=({x},{y})", arrow.size);
        Ok(Shapes {
            theme: theme.into(),
            arrow: make(&arrow),
            dirs,
            chain,
            want,
            found: HashMap::new(),
        })
    }

    /// Shape `name`, built by `make` on its first use.
    pub fn get(&mut self, name: &'static str, make: impl FnOnce(&Cursor) -> T) -> &T {
        if name == "default" {
            return &self.arrow;
        }
        let (dirs, chain, theme, want) = (&self.dirs, &self.chain, &self.theme, self.want);
        let found =
            self.found
                .entry(name)
                .or_insert_with(|| match read(dirs, chain, theme, want, name) {
                    Ok(c) => {
                        info!(name, found = c.is_some(), "cursor shape loaded");
                        c.map(|c| make(&c))
                    }
                    Err(e) => {
                        warn!("cursor {name} of {theme} not loaded, the arrow instead: {e}");
                        None
                    }
                });
        found.as_ref().unwrap_or(&self.arrow)
    }
}

/// Shape `name` decoded at the size nearest `want`, `None` if the theme has none.
fn read(
    dirs: &[PathBuf],
    chain: &[String],
    theme: &str,
    want: u32,
    name: &'static str,
) -> Result<Option<Cursor>, String> {
    let Some(p) = file(dirs, chain, name) else {
        return Ok(None);
    };
    let bytes = read_bytes(&p, MAX_FILE)?.ok_or("no file")?;
    decode(&bytes, theme, want).map(Some)
}

/// The shapes for output `scale` from the user's themes; `None`, after one warning, keeps the
/// host's or no cursor.
pub fn from_env<T>(scale: f64, make: impl FnOnce(&Cursor) -> T) -> Option<Shapes<T>> {
    let var = std::env::var_os;
    let dirs = dirs(
        var("XDG_DATA_HOME").as_deref(),
        var("HOME").as_deref(),
        var("XDG_DATA_DIRS").as_deref(),
    );
    let settings =
        crate::desktop_config::load().unwrap_or_else(|_| crate::desktop_config::defaults());
    let chosen = settings["cursor_theme"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| theme(&dirs).map(str::to_owned));
    let Some(theme) = chosen else {
        warn!(
            ?dirs,
            "no cursor theme of {THEMES:?} found: navi draws no cursor"
        );
        return None;
    };
    let want = (settings["cursor_size"].as_u64().unwrap_or(24) as f64 * scale).round() as u32;
    Shapes::new(dirs, &theme, want, make)
        .inspect_err(|e| warn!("cursor theme {theme} not loaded, navi draws no cursor: {e}"))
        .ok()
}

/// The first frame of the size nearest `SIZE × scale` in the file at `path`.
#[cfg(test)]
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
    // The parser keeps two copies of every image's pixels, so TOC entries aimed at one image
    // multiply it: the images may claim no more bytes than the file has.
    let mut claimed: u64 = 0;
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
        claimed += 4 * u64::from(w) * u64::from(h);
        if claimed > b.len() as u64 {
            return Err("images claim more bytes than the file has".into());
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

    fn arrow(dirs: &[PathBuf], theme: &str) -> Option<PathBuf> {
        file(dirs, &chain(dirs, theme), "default")
    }

    /// Shapes of `theme` whose values are the decoded cursors.
    fn shapes(dirs: &[PathBuf], theme: &str, want: u32) -> Shapes<Cursor> {
        Shapes::new(dirs.to_vec(), theme, want, Cursor::clone).unwrap()
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
        assert_eq!(arrow(&all, "Sweet-cursors"), Some(sweet(&dot)));
        // The XDG_DATA_HOME copy wins over ~/.icons.
        theme_dir(&home, "Sweet-cursors", None, &["default"]);
        assert_eq!(arrow(&all, "Sweet-cursors"), Some(sweet(&home)));
    }

    #[test]
    fn ac4_inherits_cycles_and_left_ptr() {
        let tmp = Tmp::new("inherit");
        let d = vec![tmp.0.clone()];
        // A theme with only an index finds `default` in its parent.
        theme_dir(&tmp.0, "child", Some("mid, nothing"), &[]);
        theme_dir(&tmp.0, "mid", Some("base"), &[]);
        theme_dir(&tmp.0, "base", None, &["default"]);
        assert_eq!(arrow(&d, "child"), Some(tmp.0.join("base/cursors/default")));
        // A cycle ends.
        theme_dir(&tmp.0, "a", Some("b"), &[]);
        theme_dir(&tmp.0, "b", Some("a,../../etc"), &[]);
        assert_eq!(arrow(&d, "a"), None);
        assert_eq!(chain(&d, "a"), ["a", "b"]);
        // No `default` anywhere: `left_ptr`.
        theme_dir(&tmp.0, "old", None, &["left_ptr"]);
        assert_eq!(arrow(&d, "old"), Some(tmp.0.join("old/cursors/left_ptr")));
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
        assert_eq!(arrow(&d, "l1"), Some(tmp.0.join("l5/cursors/default")));
        assert_eq!(arrow(&d, "l0"), None);
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
        // The security review's shape: TOC entries all aimed at one 256 px image (152,000 of
        // them asked the parser for ~74 GiB). The eighth already claims more than the file holds.
        let mut aliased = xcursor(&[(256, 256, (0, 0))]);
        let pos = u32::from_le_bytes(aliased[24..28].try_into().unwrap());
        let n = 152_000u32;
        let mut file = Vec::new();
        for v in [u32::from_le_bytes(*b"Xcur"), 16, 0x1_0000, n] {
            file.extend(v.to_le_bytes());
        }
        let image = aliased.split_off(pos as usize);
        let at = 16 + 12 * n;
        for _ in 0..n {
            file.extend([IMAGE, 256, at].iter().flat_map(|v| v.to_le_bytes()));
        }
        file.extend(image);
        assert!(file.len() < MAX_FILE as usize, "{}", file.len());
        assert_eq!(
            check(&file),
            Err("images claim more bytes than the file has".into())
        );
        for n in 0..wide.len() {
            let _ = decode(&wide[..n], "t", 24);
        }
    }

    #[test]
    fn shapes_ac4_pick_precedence() {
        use Request::*;
        let (text, hidden): (Request<i32>, Request<i32>) = (Named("text"), Hidden);
        let named = |n| Shape::Named(n);
        // Lock beats everything, even a drag left over.
        assert_eq!(
            pick(true, Some("grabbing"), true, Some("n-resize"), &text),
            named("text")
        );
        assert_eq!(
            pick(true, Some("grabbing"), false, None, &hidden),
            Shape::Hidden
        );
        // A drag beats a layer, a zone and the client.
        assert_eq!(
            pick(false, Some("se-resize"), true, Some("grab"), &text),
            named("se-resize")
        );
        assert_eq!(
            pick(false, Some("grabbing"), false, None, &hidden),
            named("grabbing")
        );
        // A Top/Overlay layer beats a zone: its client's request counts.
        assert_eq!(
            pick(false, None, true, Some("w-resize"), &text),
            named("text")
        );
        assert_eq!(
            pick(false, None, true, Some("w-resize"), &hidden),
            Shape::Hidden
        );
        // A zone beats the client's request, Hidden included.
        assert_eq!(
            pick(false, None, false, Some("w-resize"), &text),
            named("w-resize")
        );
        assert_eq!(
            pick(false, None, false, Some("grab"), &hidden),
            named("grab")
        );
        // With none of those, the client's request.
        assert_eq!(pick(false, None, false, None, &text), named("text"));
        assert_eq!(pick(false, None, false, None, &hidden), Shape::Hidden);
        // A focus change resets the request; the old client's no longer applies.
        let mut r = Requested::default();
        r.focus(Some(1));
        r.request = Hidden;
        r.focus(Some(1));
        assert_eq!(pick(false, None, false, None, &r.request), Shape::Hidden);
        r.focus(Some(2));
        assert_eq!(pick(false, None, false, None, &r.request), named("default"));
        r.request = Surface(2);
        r.focus(None);
        assert_eq!(pick(false, None, false, None, &r.request), named("default"));
    }

    #[test]
    fn client_ac1_pick_gives_the_surface_only_over_an_app() {
        let s = Request::Surface(7);
        let pick = |lock, drag, layer, zone| pick(lock, drag, layer, zone, &s);
        assert_eq!(pick(false, None, false, None), Shape::Client(&7));
        // Locked or over a panel, job 025 lets the client's request through, but not a surface.
        assert_eq!(pick(true, None, false, None), Shape::Named("default"));
        assert_eq!(
            pick(true, Some("grabbing"), true, Some("grab")),
            Shape::Named("default")
        );
        assert_eq!(pick(false, None, true, None), Shape::Named("default"));
        assert_eq!(
            pick(false, None, true, Some("grab")),
            Shape::Named("default")
        );
        // A zone's or a drag's cursor wins, as over any request.
        assert_eq!(
            pick(false, None, false, Some("n-resize")),
            Shape::Named("n-resize")
        );
        assert_eq!(
            pick(false, Some("grabbing"), false, None),
            Shape::Named("grabbing")
        );
        assert_eq!(
            pick(false, Some("grabbing"), true, Some("grab")),
            Shape::Named("grabbing")
        );
    }

    #[test]
    fn client_ac2_surface_caps() {
        let ok = |size, buffer, hot| fits(size, buffer, hot, false);
        assert!(ok((256, 256), (256, 256), (0, 0)));
        assert!(ok((256, 256), (1024, 1024), (255, 255)));
        assert!(ok((1, 1), (1, 1), (0, 0)));
        assert!(ok((24, 24), (48, 48), (4, 4)));
        assert!(!ok((257, 24), (257, 24), (0, 0)), "257 wide");
        assert!(!ok((24, 257), (24, 257), (0, 0)), "257 tall");
        assert!(!ok((0, 0), (0, 0), (0, 0)), "0×0");
        assert!(!ok((0, 24), (0, 24), (0, 0)));
        assert!(!ok((256, 256), (1025, 1024), (0, 0)), "1025 px buffer");
        assert!(!ok((256, 256), (1024, 1025), (0, 0)), "1025 px buffer");
        assert!(!ok((24, 24), (24, 24), (-1, 0)));
        assert!(!ok((24, 24), (24, 24), (0, -1)));
        assert!(!ok((24, 24), (24, 24), (24, 0)));
        assert!(!ok((24, 24), (24, 24), (0, 24)));
        assert!(!fits((24, 24), (24, 24), (4, 4), true), "a subsurface");
        let ends = [i32::MIN, -1, 0, 1, i32::MAX];
        for a in ends {
            for b in ends {
                for c in ends {
                    // A hotspot at (w, h) is outside, whatever w and h are.
                    assert!(!ok((a, b), (c, c), (a, b)));
                    let _ = ok((24, 24), (a, b), (c, a));
                }
            }
        }
    }

    #[test]
    fn shapes_ac5_name_alias_inherits_and_fallback() {
        let tmp = Tmp::new("shapes");
        let d = vec![tmp.0.clone()];
        let shape = |hot: (u32, u32), sizes: &[u32]| {
            xcursor(&sizes.iter().map(|&s| (s, s, hot)).collect::<Vec<_>>())
        };
        theme_dir(&tmp.0, "base", None, &["default"]);
        theme_dir(&tmp.0, "kid", Some("base"), &[]);
        let (kid, base) = (tmp.0.join("kid/cursors"), tmp.0.join("base/cursors"));
        // Under its CSS name, with its own hotspot.
        fs::write(kid.join("nw-resize"), shape((3, 4), &[24, 48])).unwrap();
        // Only a legacy alias.
        fs::write(kid.join("closedhand"), shape((5, 6), &[24])).unwrap();
        // Only in the inherited theme.
        fs::write(base.join("e-resize"), shape((7, 8), &[24, 48])).unwrap();
        // The CSS name in a parent beats an alias in the child.
        fs::write(kid.join("bottom_side"), shape((9, 9), &[24])).unwrap();
        fs::write(base.join("s-resize"), shape((10, 10), &[24])).unwrap();

        let mut s = shapes(&d, "kid", 48);
        let arrow = s.get("default", Cursor::clone).clone();
        assert_eq!((arrow.hotspot, arrow.size), ((1, 1), 24));
        let got = |s: &mut Shapes<Cursor>, n| {
            let c = s.get(n, Cursor::clone);
            (c.hotspot, c.size)
        };
        assert_eq!(got(&mut s, "nw-resize"), ((3, 4), 48));
        assert_eq!(got(&mut s, "grabbing"), ((5, 6), 24));
        assert_eq!(got(&mut s, "e-resize"), ((7, 8), 48));
        assert_eq!(got(&mut s, "s-resize"), ((10, 10), 24));
        // Missing everywhere: the arrow.
        assert_eq!(*s.get("ne-resize", Cursor::clone), arrow);
        // The size nearest size × scale.
        assert_eq!(got(&mut shapes(&d, "kid", 24), "nw-resize"), ((3, 4), 24));
        assert_eq!(got(&mut shapes(&d, "kid", 40), "e-resize"), ((7, 8), 48));
        // `grabbing` also tries `fleur`; `grab` its `openhand` alias.
        assert_eq!(names("grabbing"), ["grabbing", "closedhand", "fleur"]);
        assert_eq!(names("grab"), ["grab", "openhand", "fleur"]);
        assert_eq!(names("se-resize"), ["se-resize", "bottom_right_corner"]);
        // A theme with no arrow loads nothing.
        theme_dir(&tmp.0, "bare", None, &["text"]);
        assert!(Shapes::new(d.clone(), "bare", 24, Cursor::clone).is_err());
    }

    #[test]
    fn shapes_ac6_caps_and_one_read() {
        let tmp = Tmp::new("caps");
        let d = vec![tmp.0.clone()];
        theme_dir(&tmp.0, "t", None, &["default"]);
        let dir = tmp.0.join("t/cursors");
        let mut big = xcursor(&[(24, 24, (0, 0))]);
        big.resize(MAX_FILE as usize + 1, 0);
        let mut lying = xcursor(&[(24, 24, (0, 0))]);
        lying[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        let bad = [
            ("n-resize", big),
            ("s-resize", xcursor(&[(24, 257, (0, 0))])),
            ("e-resize", xcursor(&[(24, 24, (24, 0))])),
            ("w-resize", lying),
        ];
        for (n, bytes) in &bad {
            fs::write(dir.join(n), bytes).unwrap();
        }
        let mut s = shapes(&d, "t", 24);
        let arrow = s.get("default", Cursor::clone).clone();
        for (n, _) in &bad {
            assert_eq!(*s.get(n, Cursor::clone), arrow, "{n}");
        }
        // A hit is read once: gone from disk, it still draws.
        fs::write(dir.join("grab"), xcursor(&[(24, 24, (2, 3))])).unwrap();
        assert_eq!(s.get("grab", Cursor::clone).hotspot, (2, 3));
        fs::remove_file(dir.join("grab")).unwrap();
        assert_eq!(s.get("grab", Cursor::clone).hotspot, (2, 3));
        // A miss is remembered too: a file appearing later isn't looked for.
        assert_eq!(*s.get("text", Cursor::clone), arrow);
        fs::write(dir.join("text"), xcursor(&[(24, 24, (4, 4))])).unwrap();
        assert_eq!(*s.get("text", Cursor::clone), arrow);
        // So is a bad file, fixed later.
        fs::write(dir.join("n-resize"), xcursor(&[(24, 24, (4, 4))])).unwrap();
        assert_eq!(*s.get("n-resize", Cursor::clone), arrow);
        // A new load reads the files again.
        assert_eq!(
            shapes(&d, "t", 24).get("text", Cursor::clone).hotspot,
            (4, 4)
        );
    }
}
