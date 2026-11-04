//! Wallpaper per monitor: `$XDG_CONFIG_HOME/navi/wallpaper`, imported once from Plasma's
//! appletsrc on first run. Plain data and std I/O, no Wayland types.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use tracing::{info, warn};

use crate::image::{self, Image};
use crate::layout::Rect;
use crate::persist;

const HEADER: &str = "navi-wallpaper 1";
const PLASMA: &str = "plasma-org.kde.plasma.desktop-appletsrc";

/// A screen index: Plasma's screen number, which navi maps to monitor priority.
pub type Entry = (u8, PathBuf);

/// Absolute, with no control characters.
fn valid(path: &str) -> Option<PathBuf> {
    (path.starts_with('/') && !path.chars().any(char::is_control)).then(|| path.into())
}

/// Digits only: `str::parse` would also take a sign.
fn index(s: &str) -> Option<u8> {
    s.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| s.parse().ok())?
}

/// `%XX` escapes only; anything else after `%`, or bytes that aren't UTF-8, is `None`.
fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        out.push(match b {
            b'%' => {
                let hex = [bytes.next()?, bytes.next()?];
                u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?
            }
            _ => b,
        });
    }
    String::from_utf8(out).ok()
}

/// Plasma screen index → image path, from `plasma-org.kde.plasma.desktop-appletsrc`.
pub fn plasma_import(appletsrc: &str) -> Vec<Entry> {
    #[derive(Default)]
    struct Containment<'a> {
        header: bool,
        plugin: Option<&'a str>,
        wallpaper: Option<&'a str>,
        screen: Option<&'a str>,
        activity: Option<&'a str>,
        image: Option<&'a str>,
    }
    let mut all: BTreeMap<u64, Containment> = BTreeMap::new();
    // The containment and whether this is its image section.
    let mut section: Option<(u64, bool)> = None;
    for line in appletsrc.lines() {
        if line.starts_with('[') {
            section = line
                .strip_prefix("[Containments][")
                .and_then(|r| r.split_once(']'))
                .and_then(|(n, rest)| {
                    let n = n
                        .bytes()
                        .all(|b| b.is_ascii_digit())
                        .then(|| n.parse().ok())??;
                    match rest {
                        "" => Some((n, false)),
                        "[Wallpaper][org.kde.image][General]" => Some((n, true)),
                        _ => None,
                    }
                });
            if let Some((n, false)) = section {
                all.entry(n).or_default().header = true;
            }
            continue;
        }
        let (Some((n, image)), Some((key, value))) = (section, line.split_once('=')) else {
            continue;
        };
        let c = all.entry(n).or_default();
        let slot = match (image, key) {
            (true, "Image") => &mut c.image,
            (false, "plugin") => &mut c.plugin,
            (false, "wallpaperplugin") => &mut c.wallpaper,
            (false, "lastScreen") => &mut c.screen,
            (false, "activityId") => &mut c.activity,
            _ => continue,
        };
        *slot = Some(value);
    }

    // Per screen: (no activity, containment number) as the preference key, and the path.
    let mut best: BTreeMap<u8, ((bool, u64), PathBuf)> = BTreeMap::new();
    for (n, c) in all {
        if !c.header || c.wallpaper != Some("org.kde.image") || c.plugin == Some("org.kde.panel") {
            continue;
        }
        let (Some(screen), Some(image)) = (c.screen.and_then(index), c.image) else {
            continue;
        };
        let path = match image.strip_prefix("file://") {
            Some(p) => percent_decode(p),
            None => Some(image.to_string()),
        };
        let Some(path) = path.as_deref().and_then(valid) else {
            continue;
        };
        let key = (c.activity.is_none_or(str::is_empty), n);
        if best.get(&screen).is_none_or(|(k, _)| key < *k) {
            best.insert(screen, (key, path));
        }
    }
    best.into_iter().map(|(s, (_, p))| (s, p)).collect()
}

/// navi's file: the header, then `<index> <path>` per line, sorted by index.
pub fn serialize(entries: &[Entry]) -> String {
    let sorted: BTreeMap<u8, &Path> = entries.iter().map(|(i, p)| (*i, p.as_path())).collect();
    let mut text = format!("{HEADER}\n");
    for (i, p) in sorted {
        text += &format!("{i} {}\n", p.display());
    }
    text
}

/// The entries and the number of bad lines skipped. A later line for an index wins.
pub fn parse(text: &str) -> Result<(Vec<Entry>, usize), &'static str> {
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Err("not a navi-wallpaper 1 file");
    }
    let (mut entries, mut skipped) = (BTreeMap::new(), 0);
    for line in lines {
        let entry = line
            .split_once(' ')
            .and_then(|(i, p)| Some((index(i)?, valid(p)?)));
        match entry {
            Some((i, p)) => {
                entries.insert(i, p);
            }
            None => skipped += 1,
        }
    }
    Ok((entries.into_iter().collect(), skipped))
}

/// Each output's wallpaper, given the outputs' priorities. Index 0 is the main monitor
/// (priority 0), then the rest by priority, as Plasma's screen 0 is the primary.
pub fn for_outputs<'a>(entries: &'a [Entry], priorities: &[u32]) -> Vec<Option<&'a Path>> {
    let mut by_priority: Vec<usize> = (0..priorities.len()).collect();
    by_priority.sort_by_key(|&i| priorities[i]);
    let mut out = vec![None; priorities.len()];
    for (rank, i) in by_priority.into_iter().enumerate() {
        out[i] = entries
            .iter()
            .find(|(n, _)| usize::from(*n) == rank)
            .map(|(_, p)| p.as_path());
    }
    out
}

/// The centred crop of an image with the output's aspect ratio ("scaled and cropped").
pub fn cover(img_w: i32, img_h: i32, out_w: i32, out_h: i32) -> Rect {
    let (iw, ih) = (img_w.max(1), img_h.max(1));
    let whole = Rect {
        x: 0,
        y: 0,
        w: iw,
        h: ih,
    };
    if out_w <= 0 || out_h <= 0 {
        return whole;
    }
    let (iw64, ih64, ow, oh) = (iw as i64, ih as i64, out_w as i64, out_h as i64);
    // Rounded quotient, kept within 1..=max.
    let fit = |num: i64, den: i64, max: i32| ((num + den / 2) / den).clamp(1, max as i64) as i32;
    if iw64 * oh > ow * ih64 {
        let w = fit(ih64 * ow, oh, iw);
        Rect {
            x: (iw - w) / 2,
            w,
            ..whole
        }
    } else {
        let h = fit(iw64 * oh, ow, ih);
        Rect {
            y: (ih - h) / 2,
            h,
            ..whole
        }
    }
}

/// The grid's area: what layer surfaces leave of the output, unless that is under half the
/// output either way or not inside it, so a client's exclusive zone can't squeeze the grid away.
pub fn usable_area(output: Rect, non_exclusive: Rect) -> Rect {
    let o = output;
    let n = non_exclusive;
    let inside = n.x >= o.x && n.y >= o.y && n.x + n.w <= o.x + o.w && n.y + n.h <= o.y + o.h;
    match inside && n.w * 2 >= o.w && n.h * 2 >= o.h {
        true => n,
        false => o,
    }
}

/// navi's entries, imported from Plasma's file on first run: when navi's file doesn't exist.
fn load_or_import(navi: &Path, plasma: &Path) -> Vec<Entry> {
    match persist::read(navi) {
        Ok(Some(text)) => match parse(&text) {
            Ok((entries, skipped)) => {
                if skipped > 0 {
                    warn!(path = %navi.display(), skipped, "wallpaper: bad lines skipped");
                }
                entries
            }
            Err(e) => {
                warn!(path = %navi.display(), "wallpaper file ignored: {e}");
                Vec::new()
            }
        },
        Ok(None) => import(navi, plasma),
        Err(e) => {
            warn!(path = %navi.display(), "wallpaper file ignored: {e}");
            Vec::new()
        }
    }
}

/// Copies Plasma's wallpapers into navi's file. A read error leaves navi's file unwritten, so the
/// next start retries.
fn import(navi: &Path, plasma: &Path) -> Vec<Entry> {
    let entries = match persist::read(plasma) {
        Ok(text) => plasma_import(text.as_deref().unwrap_or("")),
        Err(e) => {
            warn!(path = %plasma.display(), "wallpaper import failed: {e}");
            return Vec::new();
        }
    };
    match persist::write(navi, &serialize(&entries)) {
        Ok(()) => info!(path = %navi.display(), count = entries.len(), "wallpaper imported"),
        Err(e) => warn!(path = %navi.display(), "wallpaper import not saved: {e}"),
    }
    entries
}

fn open_image(path: &Path) -> Result<Image, String> {
    if path.is_dir() {
        return Err("unsupported: a directory".into());
    }
    match persist::read_bytes(path, image::MAX_FILE)? {
        Some(bytes) => image::decode(&bytes),
        None => Err("not found".into()),
    }
}

/// The wallpaper of each output, given as (name, priority); `None` means the solid colour. Each
/// distinct path is decoded once.
pub fn open(outputs: &[(&str, u32)]) -> Vec<Option<Rc<Image>>> {
    let var = std::env::var_os;
    let Some(dir) = persist::xdg_dir(
        var("XDG_CONFIG_HOME").as_deref(),
        var("HOME").as_deref(),
        ".config",
    ) else {
        warn!("no absolute XDG_CONFIG_HOME or HOME: no wallpaper");
        return vec![None; outputs.len()];
    };
    let entries = load_or_import(&dir.join("navi/wallpaper"), &dir.join(PLASMA));
    let mut decoded: HashMap<&Path, Option<Rc<Image>>> = HashMap::new();
    let priorities: Vec<u32> = outputs.iter().map(|o| o.1).collect();
    let paths = for_outputs(&entries, &priorities);
    outputs
        .iter()
        .zip(paths)
        .map(|(&(name, _), path)| {
            let img = path.and_then(|p| {
                decoded
                    .entry(p)
                    .or_insert_with(|| match open_image(p) {
                        Ok(img) => Some(Rc::new(img)),
                        Err(e) => {
                            warn!(path = %p.display(), "wallpaper not shown: {e}");
                            None
                        }
                    })
                    .clone()
            });
            match (&img, path) {
                (Some(i), Some(p)) => {
                    info!(output = name, path = %p.display(), w = i.w, h = i.h, "wallpaper")
                }
                _ => info!(output = name, path = "solid", "wallpaper"),
            }
            img
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::persist::tests::{Tmp, mode};

    /// The structure of Yoshi's appletsrc: a panel on screen 0, a stale containment 4 with an
    /// image but no header, and desktops on screens 0, 1 and 2.
    const APPLETSRC: &str = "\
[ActionPlugins][0]
RightButton;NoModifier=org.kde.contextmenu

[Containments][23]
activityId=
formfactor=2
lastScreen=0
location=4
plugin=org.kde.panel
wallpaperplugin=org.kde.image

[Containments][23][Applets][28]
activityId=
lastScreen=0
plugin=org.kde.plasma.systemtray
wallpaperplugin=org.kde.image

[Containments][23][Wallpaper][org.kde.image][General]
Image=/usr/share/wallpapers/panel.png

[Containments][4][Wallpaper][org.kde.image][General]
Image=/usr/share/wallpapers/cachyos-wallpapers/north.png
SlidePaths=/usr/share/wallpapers/

[Containments][43]
activityId=8806f5a1-39bd-4254-8cd1-dbcaa8991c95
formfactor=0
lastScreen=0
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][43][General]
Image=/not/this.png

[Containments][43][Wallpaper][org.kde.image][General]
Blur=true
FillMode=1
Image=file:///home/yoshi/Pictures/Lain_Tint_1610.jpg

[Containments][50]
activityId=8806f5a1-39bd-4254-8cd1-dbcaa8991c95
lastScreen=1
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][50][Wallpaper][org.kde.image][General]
Image=file:///usr/share/wallpapers/cachyos-wallpapers/Cachy_Topography.jpg

[Containments][51]
activityId=8806f5a1-39bd-4254-8cd1-dbcaa8991c95
lastScreen=2
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][51][Wallpaper][org.kde.image][General]
Image=/usr/share/wallpapers/cachyos-wallpapers/PurpleFeathers.png

[ScreenMapping]
screenMapping=
";

    fn yoshi() -> Vec<Entry> {
        vec![
            (0, "/home/yoshi/Pictures/Lain_Tint_1610.jpg".into()),
            (
                1,
                "/usr/share/wallpapers/cachyos-wallpapers/Cachy_Topography.jpg".into(),
            ),
            (
                2,
                "/usr/share/wallpapers/cachyos-wallpapers/PurpleFeathers.png".into(),
            ),
        ]
    }

    /// One desktop containment on screen 0 with `screen` and `image`.
    fn one(screen: &str, image: &str) -> Vec<Entry> {
        plasma_import(&format!(
            "[Containments][7]\nlastScreen={screen}\nplugin=org.kde.plasma.folder\n\
             wallpaperplugin=org.kde.image\n\n\
             [Containments][7][Wallpaper][org.kde.image][General]\nImage={image}\n"
        ))
    }

    #[test]
    fn imports_yoshis_appletsrc() {
        assert_eq!(plasma_import(APPLETSRC), yoshi());
        assert_eq!(plasma_import(&APPLETSRC.replace('\n', "\r\n")), yoshi());
    }

    #[test]
    fn import_validates_every_value() {
        assert_eq!(one("0", "file:///a%20b.png"), [(0, "/a b.png".into())]);
        assert_eq!(one("255", "/c.png"), [(255, "/c.png".into())]);
        assert_eq!(one("0", "file:///%C3%A9.png"), [(0, "/é.png".into())]);
        for (screen, image) in [
            ("0", "file:///a%zz.png"),
            ("0", "file:///a%2"),
            ("0", "file:///%ff.png"),
            ("0", "file://host/a.png"),
            ("0", "a.png"),
            ("0", ""),
            ("0", "/a\u{7}.png"),
            ("0", "file:///a%0A.png"),
            ("-1", "/a.png"),
            ("256", "/a.png"),
            ("x", "/a.png"),
            ("+1", "/a.png"),
            ("", "/a.png"),
        ] {
            assert_eq!(one(screen, image), [], "{screen} {image}");
        }
        // Key and section names match exactly.
        assert_eq!(
            plasma_import(&APPLETSRC.replace("lastScreen=1", "lastscreen=1")).len(),
            2
        );
        assert_eq!(
            plasma_import(&APPLETSRC.replace("[Containments][50]\n", "[containments][50]\n")).len(),
            2
        );
    }

    #[test]
    fn import_prefers_an_activity_then_the_lowest_containment() {
        let c = |n: u32, activity: &str, image: &str| {
            format!(
                "[Containments][{n}]\nactivityId={activity}\nlastScreen=0\n\
                 wallpaperplugin=org.kde.image\n\
                 [Containments][{n}][Wallpaper][org.kde.image][General]\nImage={image}\n"
            )
        };
        let text = c(3, "", "/none.png") + &c(9, "a", "/nine.png") + &c(5, "b", "/five.png");
        assert_eq!(plasma_import(&text), [(0, "/five.png".into())]);
        let text = c(3, "", "/three.png") + &c(2, "", "/two.png");
        assert_eq!(plasma_import(&text), [(0, "/two.png".into())]);
    }

    #[test]
    fn import_survives_junk() {
        assert_eq!(plasma_import(""), []);
        let junk: String = (0..4096u32)
            .map(|i| char::from_u32(i * 7919 % 0x2fff).unwrap_or('['))
            .collect();
        assert_eq!(plasma_import(&junk), []);
        let bytes: Vec<u8> = (0..8192u32).map(|i| (i * 31 % 251) as u8).collect();
        assert_eq!(plasma_import(&String::from_utf8_lossy(&bytes)), []);
        assert_eq!(
            plasma_import("[Containments][99999999999999999999999]\nlastScreen=0"),
            []
        );
    }

    #[test]
    fn config_round_trips() {
        let mut entries = yoshi();
        entries.push((7, "/home/u/My Pictures/a b.png".into()));
        let text = serialize(&entries);
        assert!(text.starts_with("navi-wallpaper 1\n"));
        assert_eq!(text.lines().last(), Some("7 /home/u/My Pictures/a b.png"));
        assert_eq!(parse(&text), Ok((entries.clone(), 0)));
        entries.reverse();
        assert_eq!(serialize(&entries), text);
        assert_eq!(serialize(&[]), "navi-wallpaper 1\n");
    }

    #[test]
    fn config_skips_bad_lines() {
        let text = "navi-wallpaper 1\n0 /a.png\n1 rel.png\n256 /b.png\n2\n2 \n3 /c\u{1}.png\n\
                    x /d.png\n-1 /e.png\n\n0 /later.png\n4 /f.png\n";
        let want = vec![(0, "/later.png".into()), (4, "/f.png".into())];
        assert_eq!(parse(text), Ok((want, 8)));
        assert!(parse("navi-wallpaper 2\n0 /a.png\n").is_err());
        assert!(parse("").is_err());
        assert!(parse("0 /a.png\n").is_err());
    }

    #[test]
    fn maps_by_priority() {
        // Laptop, 4K, ultrawide.
        let rig = [2, 1, 0];
        let entries: Vec<Entry> = vec![(0, "/zero.png".into()), (2, "/two.png".into())];
        let got = for_outputs(&entries, &rig);
        assert_eq!(
            got,
            [
                Some(Path::new("/two.png")),
                None,
                Some(Path::new("/zero.png"))
            ]
        );
        assert_eq!(for_outputs(&[], &rig), [None, None, None]);
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn cover_crops_to_the_output_aspect() {
        assert_eq!(cover(2880, 1800, 1800, 1125), rect(0, 0, 2880, 1800));
        assert_eq!(cover(3440, 1440, 1920, 1080), rect(440, 0, 2560, 1440));
        let r = cover(2000, 1125, 3440, 1440);
        assert_eq!((r.x, r.w), (0, 2000));
        assert!((836..=838).contains(&r.h), "{r:?}");
        assert_eq!(r.y, (1125 - r.h) / 2);
        for (iw, ih, ow, oh) in [
            (1, 1, 3440, 1440),
            (3440, 1440, 1, 1),
            (8192, 1, 1, 8192),
            (1, 8192, 8192, 1),
            (100, 100, 0, 0),
            (100, 100, 0, 50),
            (100, 100, -5, 50),
            (0, 0, 10, 10),
        ] {
            let r = cover(iw, ih, ow, oh);
            assert!(r.w >= 1 && r.h >= 1 && r.x >= 0 && r.y >= 0, "{r:?}");
            assert!(r.x + r.w <= iw.max(1) && r.y + r.h <= ih.max(1), "{r:?}");
        }
    }

    #[test]
    fn usable_area_keeps_half_the_output() {
        let o = rect(0, 0, 1920, 1080);
        assert_eq!(
            usable_area(o, rect(0, 0, 1920, 1040)),
            rect(0, 0, 1920, 1040)
        );
        assert_eq!(usable_area(o, rect(0, 0, 1920, 500)), o);
        assert_eq!(usable_area(o, rect(0, 0, 900, 1080)), o);
        assert_eq!(usable_area(o, rect(0, 0, 960, 540)), rect(0, 0, 960, 540));
        assert_eq!(usable_area(o, rect(2000, 0, 1920, 1040)), o);
        assert_eq!(usable_area(o, rect(0, 100, 1920, 1040)), o);
        assert_eq!(usable_area(o, rect(0, 0, 0, 0)), o);
        let off = rect(3440, 0, 3840, 2160);
        assert_eq!(
            usable_area(off, rect(3440, 40, 3840, 2120)),
            rect(3440, 40, 3840, 2120)
        );
    }

    #[test]
    fn first_run_imports_once() {
        let tmp = Tmp::new("wallpaper");
        let (navi, plasma) = (tmp.0.join("navi/wallpaper"), tmp.0.join(PLASMA));
        fs::write(&plasma, APPLETSRC).unwrap();

        assert_eq!(load_or_import(&navi, &plasma), yoshi());
        assert_eq!(mode(&tmp.0.join("navi")), 0o700);
        assert_eq!(mode(&navi), 0o600);
        let written = fs::read(&navi).unwrap();
        assert_eq!(written, serialize(&yoshi()).as_bytes());

        // Never re-imported, even if Plasma changes.
        fs::write(&plasma, APPLETSRC.replace("Lain", "Other")).unwrap();
        assert_eq!(load_or_import(&navi, &plasma), yoshi());
        assert_eq!(fs::read(&navi).unwrap(), written);

        // Nothing to import: a header-only file, which is then kept.
        fs::remove_file(&navi).unwrap();
        fs::remove_file(&plasma).unwrap();
        assert_eq!(load_or_import(&navi, &plasma), []);
        assert_eq!(fs::read_to_string(&navi).unwrap(), "navi-wallpaper 1\n");
        fs::write(&plasma, APPLETSRC).unwrap();
        assert_eq!(load_or_import(&navi, &plasma), []);

        // A read error writes nothing, so the next start retries.
        fs::remove_file(&navi).unwrap();
        fs::remove_file(&plasma).unwrap();
        fs::create_dir(&plasma).unwrap();
        assert_eq!(load_or_import(&navi, &plasma), []);
        assert!(!navi.exists());
    }

    #[test]
    fn unreadable_images_are_errors() {
        let tmp = Tmp::new("wallpaper-img");
        assert_eq!(
            open_image(&tmp.0.join("missing.png")).err().unwrap(),
            "not found"
        );
        assert_eq!(
            open_image(&tmp.0).err().unwrap(),
            "unsupported: a directory"
        );
        let garbage = tmp.0.join("garbage.png");
        fs::write(&garbage, [7; 16]).unwrap();
        assert_eq!(open_image(&garbage).err().unwrap(), "unsupported format");
    }
}
