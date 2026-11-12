//! Icons: freedesktop Icon Theme lookup (simplified) and decoding. Themes and icon files are
//! untrusted: index files, icon files and PNG sizes are capped, SVGs can't load anything else.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use resvg::{tiny_skia, usvg};
use tiny_skia::{FilterQuality, IntSize, Pixmap, PixmapPaint, Transform};

use crate::desktop::Entry;
use crate::files::{read_bytes, xdg_dir};
use crate::image;

pub const PREFERRED: &str = "candy-icons";

/// Use the explicit KDE icon choice when present, otherwise Navi's default.
pub fn preferred(config: Option<&Path>) -> String {
    let chosen = config
        .and_then(|dir| {
            read_bytes(&dir.join("kdeglobals"), MAX_INDEX)
                .ok()
                .flatten()
        })
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| kde_theme(&text));
    chosen.unwrap_or_else(|| PREFERRED.into())
}

fn kde_theme(text: &str) -> Option<String> {
    ini(text).into_iter().find_map(|(group, entries)| {
        (group == "Icons").then(|| {
            entries.into_iter().find_map(|(key, value)| {
                (key == "Theme" && valid_name(value)).then(|| value.to_string())
            })
        })?
    })
}
const FALLBACK: &str = "hicolor";
const MAX_INDEX: u64 = 256 << 10;
const MAX_INHERITS: usize = 8;
pub const MAX_FILE: u64 = 1 << 20;
pub const MAX_PNG_SIDE: u32 = 512;
/// The menu button's own icon, used when no theme has `navi`.
const NAVI_SVG: &[u8] = include_bytes!("../../../assets/icons/hicolor/32x32/apps/navi.svg");

#[derive(Debug, PartialEq)]
enum Kind {
    Fixed,
    Scalable,
    Threshold,
}

#[derive(Debug)]
struct Dir {
    path: String,
    size: u32,
    kind: Kind,
    min: u32,
    max: u32,
    threshold: u32,
}

impl Dir {
    fn matches(&self, size: u32) -> bool {
        match self.kind {
            Kind::Fixed => self.size == size,
            Kind::Scalable => (self.min..=self.max).contains(&size),
            Kind::Threshold => size.abs_diff(self.size) <= self.threshold,
        }
    }

    fn distance(&self, size: u32) -> u32 {
        let (lo, hi) = match self.kind {
            Kind::Fixed => (self.size, self.size),
            Kind::Scalable => (self.min, self.max),
            Kind::Threshold => (
                self.size.saturating_sub(self.threshold),
                self.size + self.threshold,
            ),
        };
        lo.saturating_sub(size) + size.saturating_sub(hi)
    }
}

struct Theme {
    name: String,
    dirs: Vec<Dir>,
}

/// The icon theme chain and where to look.
pub struct Themes {
    bases: Vec<PathBuf>,
    /// The chosen theme, what it inherits, then `hicolor`.
    chain: Vec<Theme>,
    /// Unthemed icons, last.
    pixmaps: Option<PathBuf>,
}

/// `$XDG_DATA_HOME/icons`, `~/.icons`, then `$XDG_DATA_DIRS/*/icons`.
pub fn bases(data_dirs: &[PathBuf], home: Option<&OsStr>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = data_dirs.iter().map(|d| d.join("icons")).collect();
    if let Some(dot) = xdg_dir(None, home, ".icons") {
        out.insert(out.len().min(1), dot);
    }
    out
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains("..")
        && !name.chars().any(char::is_control)
}

impl Themes {
    /// `preferred` if some base has its `index.theme`, else `hicolor`.
    pub fn new(bases: Vec<PathBuf>, pixmaps: Option<PathBuf>, preferred: &str) -> Themes {
        let mut t = Themes {
            bases,
            chain: Vec::new(),
            pixmaps,
        };
        let mut todo = vec![(preferred.to_string(), 0)];
        let mut seen = HashSet::from([FALLBACK.to_string()]);
        while let Some((name, depth)) = todo.pop() {
            if depth > MAX_INHERITS || !seen.insert(name.clone()) {
                continue;
            }
            let Some((theme, inherits)) = t.load(&name) else {
                continue;
            };
            t.chain.push(theme);
            todo.extend(inherits.into_iter().rev().map(|n| (n, depth + 1)));
        }
        if let Some((hicolor, _)) = t.load(FALLBACK) {
            t.chain.push(hicolor);
        }
        t
    }

    /// The theme's directories and what it inherits, from the first `index.theme` found.
    fn load(&self, name: &str) -> Option<(Theme, Vec<String>)> {
        if !valid_name(name) {
            return None;
        }
        let text = self.bases.iter().find_map(|b| {
            let bytes = read_bytes(&b.join(name).join("index.theme"), MAX_INDEX).ok()??;
            String::from_utf8(bytes).ok()
        })?;
        let groups = ini(&text);
        let get = |group: &str, key: &str| -> Option<&str> {
            groups
                .iter()
                .find(|(g, _)| g == group)?
                .1
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| *v)
        };
        let list = |v: Option<&str>| -> Vec<String> {
            v.unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        };
        let dirs = list(get("Icon Theme", "Directories"))
            .into_iter()
            .filter(|d| !d.split('/').any(|c| c == ".."))
            .filter_map(|d| {
                let num = |k: &str| get(&d, k).and_then(|v| v.parse::<u32>().ok());
                let size = num("Size")?;
                let kind = match get(&d, "Type") {
                    Some("Fixed") => Kind::Fixed,
                    Some("Scalable") => Kind::Scalable,
                    _ => Kind::Threshold,
                };
                Some(Dir {
                    min: num("MinSize").unwrap_or(size),
                    max: num("MaxSize").unwrap_or(size),
                    threshold: num("Threshold").unwrap_or(2),
                    path: d,
                    size,
                    kind,
                })
            })
            .collect();
        let theme = Theme {
            name: name.to_string(),
            dirs,
        };
        Some((theme, list(get("Icon Theme", "Inherits"))))
    }

    /// The file for icon `name` at `size` px: an exact size match in the first theme that has
    /// one, else that theme's closest; then the next theme; then the unthemed dir.
    pub fn lookup(&self, name: &str, size: u32) -> Option<PathBuf> {
        if Path::new(name).is_absolute() {
            let p = PathBuf::from(name);
            let ext = p.extension().and_then(OsStr::to_str);
            let ok = matches!(ext, Some("png" | "svg")) && p.is_file();
            return ok.then_some(p);
        }
        if !valid_name(name) {
            return None;
        }
        let file = |dir: &Path| {
            ["png", "svg"]
                .iter()
                .map(|ext| dir.join(format!("{name}.{ext}")))
                .find(|p| p.is_file())
        };
        for theme in &self.chain {
            let in_dir = |d: &Dir| {
                self.bases
                    .iter()
                    .find_map(|b| file(&b.join(&theme.name).join(&d.path)))
            };
            let exact = theme.dirs.iter().filter(|d| d.matches(size));
            if let Some(p) = exact.into_iter().find_map(in_dir) {
                return Some(p);
            }
            let mut best: Option<(u32, PathBuf)> = None;
            for d in &theme.dirs {
                let dist = d.distance(size);
                if best.as_ref().is_none_or(|b| dist < b.0)
                    && let Some(p) = in_dir(d)
                {
                    best = Some((dist, p));
                }
            }
            if let Some((_, p)) = best {
                return Some(p);
            }
        }
        file(self.pixmaps.as_deref()?)
    }

    pub fn chain(&self) -> Vec<&str> {
        self.chain.iter().map(|t| t.name.as_str()).collect()
    }
}

/// Groups of `key=value` lines; `#` comments and anything else skipped.
fn ini(text: &str) -> Vec<(String, Vec<(&str, &str)>)> {
    let mut out: Vec<(String, Vec<(&str, &str)>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(g) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            out.push((g.to_string(), Vec::new()));
        } else if let Some((k, v)) = line.split_once('=')
            && let Some(g) = out.last_mut()
        {
            g.1.push((k.trim(), v.trim()));
        }
    }
    out
}

/// Icon names to try for a running app: its desktop entry's icon (by ID, or by
/// `StartupWMClass`), the app ID itself, then the generic one.
pub fn app_icon_names(app_id: &str, entries: &[Entry]) -> Vec<String> {
    let entry = crate::desktop::entry_for(app_id, entries).map(|i| &entries[i]);
    entry
        .and_then(|e| e.icon.clone())
        .into_iter()
        .chain([app_id.to_string(), "application-x-executable".into()])
        .collect()
}

/// Reads at most `MAX_FILE` bytes of a regular file, never blocking on a FIFO swapped in.
fn read_capped(path: &Path) -> Result<Vec<u8>, String> {
    read_bytes(path, MAX_FILE)?.ok_or_else(|| "no file".into())
}

/// An icon file drawn into a `size`×`size` pixmap, centred, keeping its aspect.
pub fn render_file(path: &Path, size: u32) -> Result<Pixmap, String> {
    let bytes = read_capped(path)?;
    match path.extension().and_then(OsStr::to_str) {
        Some("svg") => render_svg(&bytes, size),
        _ => render_png(&bytes, size),
    }
}

/// SVG with every external or embedded image resolved to nothing.
pub fn render_svg(bytes: &[u8], size: u32) -> Result<Pixmap, String> {
    let opt = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &opt).map_err(|e| format!("svg: {e}"))?;
    let mut px = Pixmap::new(size, size).ok_or("svg: empty size")?;
    let s = tree.size();
    let scale = size as f32 / s.width().max(s.height());
    let (dx, dy) = (
        (size as f32 - s.width() * scale) / 2.0,
        (size as f32 - s.height() * scale) / 2.0,
    );
    let t = Transform::from_scale(scale, scale).post_translate(dx, dy);
    resvg::render(&tree, t, &mut px.as_mut());
    Ok(px)
}

fn render_png(bytes: &[u8], size: u32) -> Result<Pixmap, String> {
    let img = image::decode(bytes, MAX_PNG_SIDE)?;
    let mut rgba = img.rgba;
    for p in rgba.as_chunks_mut::<4>().0 {
        let a = u16::from(p[3]);
        for c in &mut p[..3] {
            *c = ((u16::from(*c) * a + 127) / 255) as u8;
        }
    }
    let src = IntSize::from_wh(img.w, img.h)
        .and_then(|s| Pixmap::from_vec(rgba, s))
        .ok_or("png: bad size")?;
    let mut px = Pixmap::new(size, size).ok_or("png: empty size")?;
    let scale = size as f32 / img.w.max(img.h) as f32;
    let (dx, dy) = (
        (size as f32 - img.w as f32 * scale) / 2.0,
        (size as f32 - img.h as f32 * scale) / 2.0,
    );
    let paint = PixmapPaint {
        quality: FilterQuality::Bicubic,
        ..Default::default()
    };
    let t = Transform::from_scale(scale, scale).post_translate(dx, dy);
    px.draw_pixmap(0, 0, src.as_ref(), &paint, t, None);
    Ok(px)
}

/// Decoded icons by (key, size); `None` once every candidate failed.
pub struct Icons {
    pub themes: Themes,
    cache: HashMap<(String, u32), Option<Pixmap>>,
}

impl Icons {
    pub fn new(themes: Themes) -> Icons {
        Icons {
            themes,
            cache: HashMap::new(),
        }
    }

    /// The first of `names` that resolves and decodes, cached under `key`.
    pub fn get(&mut self, key: &str, names: &[String], size: u32) -> Option<&Pixmap> {
        let k = (key.to_string(), size);
        if !self.cache.contains_key(&k) {
            let px = names.iter().find_map(|n| {
                let path = self.themes.lookup(n, size)?;
                match render_file(&path, size) {
                    Ok(px) => Some(px),
                    Err(e) => {
                        tracing::warn!(path = %path.display(), "icon skipped: {e}");
                        None
                    }
                }
            });
            self.cache.insert(k.clone(), px);
        }
        self.cache[&k].as_ref()
    }

    pub fn category(&mut self, category: crate::desktop::Category, size: u32) -> Option<&Pixmap> {
        let name = category.icon();
        self.get(
            name,
            &[format!("{name}-symbolic"), name.into(), "folder".into()],
            size,
        )
    }

    /// The menu button's icon: `navi` from the theme, else the embedded one.
    pub fn navi(&mut self, size: u32) -> Option<&Pixmap> {
        let k = ("\0navi".to_string(), size);
        if !self.cache.contains_key(&k) {
            let themed = self.get("navi", &["navi".into()], size).cloned();
            let px = themed.or_else(|| render_svg(NAVI_SVG, size).ok());
            self.cache.insert(k.clone(), px);
        }
        self.cache[&k].as_ref()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::testutil::Tmp;

    const SVG_RED: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="#ff0000"/></svg>"##;

    /// A Wayland client's app_id may be an absolute icon path. Swapping a FIFO in for that file
    /// must not block navi-bar's main thread (job 035 review).
    #[test]
    fn a_fifo_swapped_in_for_an_icon_never_blocks() {
        use std::os::unix::fs::symlink;
        use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering::Relaxed, mpsc};
        let tmp = Tmp::new("icon-fifo");
        let dir = tmp.0.clone();
        fs::write(dir.join("reg.svg"), SVG_RED).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(dir.join("fifo.svg"))
            .status()
            .unwrap();
        assert!(made.success());
        symlink("reg.svg", dir.join("x.svg")).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (stopped, d) = (stop.clone(), dir.clone());
        // Atomically flips x.svg between the SVG and the FIFO.
        let swapper = std::thread::spawn(move || {
            for i in 0u64.. {
                if stopped.load(Relaxed) {
                    break;
                }
                let _ = fs::remove_file(d.join("n"));
                symlink(if i % 2 == 0 { "fifo.svg" } else { "reg.svg" }, d.join("n")).unwrap();
                fs::rename(d.join("n"), d.join("x.svg")).unwrap();
            }
        });
        let (done, finished) = mpsc::channel();
        let name = dir.join("x.svg").to_str().unwrap().to_string();
        std::thread::spawn(move || {
            let mut icons = Icons::new(Themes::new(vec![], None, PREFERRED));
            for i in 0..20_000 {
                let _ = icons.get(&format!("app:{i}"), std::slice::from_ref(&name), 32);
            }
            let _ = done.send(());
        });
        let result = finished.recv_timeout(std::time::Duration::from_secs(30));
        if result.is_err() {
            // Unblock the stuck open so the test process can end.
            let _ = fs::OpenOptions::new()
                .write(true)
                .open(dir.join("fifo.svg"));
        }
        stop.store(true, Relaxed);
        swapper.join().unwrap();
        assert!(result.is_ok(), "Icons::get blocked opening a FIFO");
    }

    #[test]
    fn kde_choice_and_category_icons_use_the_selected_theme() {
        let tmp = Tmp::new("kde-icons");
        assert_eq!(preferred(Some(&tmp.0)), PREFERRED);
        fs::write(
            tmp.0.join("kdeglobals"),
            "[Sounds]\nTheme=other\n[Icons]\nTheme=test-theme\n",
        )
        .unwrap();
        assert_eq!(preferred(Some(&tmp.0)), "test-theme");
        assert_eq!(kde_theme("[Icons]\nTheme=../../bad\n"), None);
        assert_eq!(kde_theme("[Sounds]\nTheme=other\n"), None);
        theme(
            &tmp.0,
            "test-theme",
            "[Icon Theme]\nDirectories=24\n[24]\nSize=24\nType=Fixed\n",
            &[
                "24/applications-office-symbolic.svg",
                "24/applications-internet.svg",
                "24/folder.svg",
            ],
        );
        let mut icons = Icons::new(Themes::new(vec![tmp.0.clone()], None, "test-theme"));
        for category in [
            crate::desktop::Category::Office,
            crate::desktop::Category::Internet,
            crate::desktop::Category::Other,
        ] {
            assert!(icons.category(category, 24).is_some());
        }
        fs::write(tmp.0.join("kdeglobals"), vec![b'x'; MAX_INDEX as usize + 1]).unwrap();
        assert_eq!(preferred(Some(&tmp.0)), PREFERRED);
    }

    fn theme(base: &Path, name: &str, index: &str, icons: &[&str]) {
        let dir = base.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("index.theme"), index).unwrap();
        for icon in icons {
            let p = dir.join(icon);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, SVG_RED).unwrap();
        }
    }

    fn index(inherits: &str, dirs: &str) -> String {
        let mut s = format!("[Icon Theme]\nName=x\nInherits={inherits}\nDirectories=");
        let names: Vec<&str> = dirs
            .lines()
            .filter_map(|l| l.strip_prefix('[')?.strip_suffix(']'))
            .collect();
        s += &names.join(",");
        s + "\n" + dirs
    }

    const DIRS: &str = "[16]\nSize=16\nType=Fixed\n[48]\nSize=48\nType=Fixed\n\
                        [scalable]\nSize=64\nType=Scalable\nMinSize=64\nMaxSize=256\n";

    #[test]
    fn ac5_theme_chain_and_lookup() {
        let tmp = Tmp::new("icons");
        let (user, sys) = (tmp.0.join("user"), tmp.0.join("sys"));
        theme(
            &user,
            "candy-icons",
            &index("mid", DIRS),
            &["48/both.svg", "16/both.svg"],
        );
        theme(&sys, "mid", &index("hicolor", DIRS), &[]);
        theme(&sys, "hicolor", &index("", DIRS), &["48/only-hicolor.svg"]);
        theme(&sys, "a", &index("b", DIRS), &[]);
        theme(&sys, "b", &index("a", DIRS), &[]);
        let bases = vec![user.clone(), sys.clone()];

        let t = Themes::new(bases.clone(), None, PREFERRED);
        assert_eq!(t.chain(), ["candy-icons", "mid", "hicolor"]);
        assert_eq!(
            t.lookup("only-hicolor", 32),
            Some(sys.join("hicolor/48/only-hicolor.svg"))
        );
        assert_eq!(t.lookup("nope", 32), None);

        // A cycle ends.
        assert_eq!(
            Themes::new(bases.clone(), None, "a").chain(),
            ["a", "b", "hicolor"]
        );
        // No candy-icons: hicolor alone.
        assert_eq!(
            Themes::new(vec![sys.clone()], None, PREFERRED).chain(),
            ["hicolor"]
        );

        // Target 32 among 16, 48 and scalable 64-256: 16 and 48 are both 16 away; the first
        // listed wins. With a scalable copy, it's 32 away, so still 16.
        assert_eq!(
            t.lookup("both", 32),
            Some(user.join("candy-icons/16/both.svg"))
        );
        theme(
            &user,
            "candy-icons",
            &index("mid", DIRS),
            &["scalable/both.svg"],
        );
        let t = Themes::new(bases.clone(), None, PREFERRED);
        assert_eq!(
            t.lookup("both", 32),
            Some(user.join("candy-icons/16/both.svg"))
        );
        assert_eq!(
            t.lookup("both", 128),
            Some(user.join("candy-icons/scalable/both.svg"))
        );
        assert_eq!(
            t.lookup("both", 47),
            Some(user.join("candy-icons/48/both.svg"))
        );

        for bad in ["../x", "a/b", "..", ""] {
            assert_eq!(t.lookup(bad, 32), None, "{bad}");
        }
        let abs = user.join("candy-icons/48/both.svg");
        assert_eq!(t.lookup(abs.to_str().unwrap(), 32), Some(abs.clone()));
        assert_eq!(t.lookup("/etc/passwd", 32), None, "not an icon type");

        // Unthemed, last.
        fs::create_dir_all(tmp.0.join("pixmaps")).unwrap();
        fs::write(tmp.0.join("pixmaps/legacy.png"), b"x").unwrap();
        let t = Themes::new(bases, Some(tmp.0.join("pixmaps")), PREFERRED);
        assert_eq!(
            t.lookup("legacy", 32),
            Some(tmp.0.join("pixmaps/legacy.png"))
        );
    }

    #[test]
    fn ac5_running_app_icon_names() {
        let e = |id: &str, icon: &str, class: Option<&str>| Entry {
            id: id.into(),
            name: id.into(),
            icon: Some(icon.into()),
            argv: vec!["x".into()],
            wm_class: class.map(Into::into),
            category: crate::desktop::Category::Other,
        };
        let entries = [
            e("org.kde.konsole.desktop", "utilities-terminal", None),
            e("firefox.desktop", "firefox", Some("Navigator")),
        ];
        let first = |app: &str| app_icon_names(app, &entries)[0].clone();
        assert_eq!(first("org.kde.konsole"), "utilities-terminal");
        assert_eq!(first("navigator"), "firefox");
        assert_eq!(
            app_icon_names("unknown", &entries),
            ["unknown", "application-x-executable"]
        );

        // Resolved: the unknown app ID isn't an icon, the generic one is.
        let tmp = Tmp::new("appicons");
        theme(
            &tmp.0,
            "hicolor",
            &index("", DIRS),
            &["48/application-x-executable.svg"],
        );
        let mut icons = Icons::new(Themes::new(vec![tmp.0.clone()], None, PREFERRED));
        let names = app_icon_names("unknown", &entries);
        assert!(icons.get("unknown", &names, 32).is_some());
        assert!(icons.get("none", &["nothing".into()], 32).is_none());
    }

    fn center(px: &Pixmap) -> [u8; 4] {
        let c = px.pixel(px.width() / 2, px.height() / 2).unwrap();
        [c.red(), c.green(), c.blue(), c.alpha()]
    }

    fn png(w: u32, h: u32, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = ::png::Encoder::new(&mut out, w, h);
        enc.set_color(::png::ColorType::Rgba);
        enc.set_depth(::png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        if !data.is_empty() {
            wr.write_image_data(data).unwrap();
        }
        drop(wr);
        out
    }

    #[test]
    fn ac6_decodes_safely() {
        let px = render_svg(SVG_RED.as_bytes(), 32).unwrap();
        assert_eq!((px.width(), px.height()), (32, 32));
        assert_eq!(center(&px), [255, 0, 0, 255]);

        // A referenced image is never read: here a real red PNG next to it stays undrawn.
        let tmp = Tmp::new("decode");
        let red = tmp.0.join("red.png");
        fs::write(&red, png(1, 1, &[255, 0, 0, 255])).unwrap();
        for href in [
            format!("file://{}", red.display()),
            red.display().to_string(),
            "file:///etc/passwd".into(),
        ] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><image href="{href}" width="8" height="8"/></svg>"#
            );
            let px = render_svg(svg.as_bytes(), 32).unwrap();
            assert_eq!(center(&px), [0, 0, 0, 0], "{href}");
        }

        let cap = |bytes: Vec<u8>| {
            let e = render_png(&bytes, 32).err().unwrap();
            assert!(e.contains("outside the size cap (512 px a side"), "{e}");
        };
        cap(png(513, 1, &[]));
        cap(png(100_000, 100_000, &[]));
        let ok = png(2, 2, &[0, 0, 255, 255].repeat(4));
        assert_eq!(center(&render_png(&ok, 32).unwrap()), [0, 0, 255, 255]);

        let put = |name: &str, bytes: &[u8]| {
            let p = tmp.0.join(name);
            fs::write(&p, bytes).unwrap();
            p
        };
        let big = put("big.svg", &vec![b' '; MAX_FILE as usize + 1]);
        assert_eq!(
            render_file(&big, 32).err().unwrap(),
            format!("over {MAX_FILE} bytes")
        );
        let cut = put("cut.png", &ok[..ok.len() / 2]);
        assert!(render_file(&cut, 32).is_err());
        let junk = put("junk.svg", b"\x00<svg nope");
        assert!(render_file(&junk, 32).is_err());

        let mut icons = Icons::new(Themes::new(Vec::new(), None, PREFERRED));
        let navi = icons.navi(32).expect("embedded icon");
        let lit = navi.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(lit > 50, "{lit} px drawn");
    }
}
