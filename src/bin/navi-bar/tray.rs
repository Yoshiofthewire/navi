//! The system tray's pure logic: StatusNotifierItem registrations, their properties and icons,
//! the band's targets, the dbusmenu model and signal coalescing. Everything here is untrusted
//! input from other processes on the session bus; `sni.rs` is the D-Bus glue.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::context::{Kind, Line};
use crate::layout::Rect;

/// Items one connection may register, and all of them.
pub const PER_SENDER: usize = 4;
pub const TOTAL: usize = 32;
/// Titles, ids and labels are cut to this many chars.
pub const TEXT: usize = 128;
pub const DEFAULT_PATH: &str = "/StatusNotifierItem";
/// The icon in logical px, its target, and the space between targets.
pub const ICON: i32 = 20;
pub const TARGET: i32 = 28;
pub const GAP: i32 = 4;
/// What the row keeps clear of the clock's text and the battery, and the band's right padding.
pub const CLEAR: i32 = 12;
/// Re-reads of one item are at least this far apart.
pub const COALESCE: Duration = Duration::from_millis(250);
/// IconPixmap entries considered, and the largest side used.
const PIXMAPS: usize = 16;
const PIXMAP_SIDE: i32 = 256;
/// dbusmenu caps: children per level, depth below the root, nodes in all.
pub const MENU_WIDTH: usize = 64;
pub const MENU_DEPTH: usize = 4;
pub const MENU_NODES: usize = 256;
/// Touchpad px per scroll notch, and the delta a notch sends.
const SCROLL_PX: f64 = 15.0;
pub const NOTCH: i32 = 120;

/// An item: the unique name of the connection that registered it, and its object path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub sender: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub key: Key,
    /// As `RegisteredStatusNotifierItems` lists it: the name it registered with, then its path.
    pub listed: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The bus name isn't the sender's.
    Spoofed,
    SenderFull,
    Full,
    BadPath,
}

/// The registered items, in registration order.
#[derive(Default)]
pub struct Registry {
    items: Vec<Entry>,
}

/// An object path as D-Bus defines it, at most 255 bytes.
fn valid_path(p: &str) -> bool {
    p == "/"
        || (p.len() <= 255
            && p.starts_with('/')
            && p[1..].split('/').all(|c| {
                !c.is_empty() && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            }))
}

impl Registry {
    /// `arg` is an object path (the item is the sender's) or a bus name, whose owner the bus
    /// reported as `owner`; the item is at `DEFAULT_PATH` then. Only a bus name's own owner may
    /// register it. Returns the new entry, or `None` for one already registered.
    pub fn register(
        &mut self,
        sender: &str,
        arg: &str,
        owner: Option<&str>,
    ) -> Result<Option<Entry>, Refused> {
        let (name, path) = match arg.starts_with('/') {
            true => (sender, arg),
            false if owner == Some(sender) => (arg, DEFAULT_PATH),
            false => return Err(Refused::Spoofed),
        };
        if !valid_path(path) {
            return Err(Refused::BadPath);
        }
        let key = Key {
            sender: sender.into(),
            path: path.into(),
        };
        if self.items.iter().any(|e| e.key == key) {
            return Ok(None);
        }
        if self.items.iter().filter(|e| e.key.sender == sender).count() >= PER_SENDER {
            return Err(Refused::SenderFull);
        }
        if self.items.len() >= TOTAL {
            return Err(Refused::Full);
        }
        let e = Entry {
            key,
            listed: format!("{name}{path}"),
        };
        self.items.push(e.clone());
        Ok(Some(e))
    }

    /// `sender` left the bus: its items, removed.
    pub fn gone(&mut self, sender: &str) -> Vec<Entry> {
        let (gone, kept) = std::mem::take(&mut self.items)
            .into_iter()
            .partition(|e| e.key.sender == sender);
        self.items = kept;
        gone
    }

    pub fn remove(&mut self, key: &Key) -> Option<Entry> {
        let at = self.items.iter().position(|e| &e.key == key)?;
        Some(self.items.remove(at))
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.items.iter().any(|e| &e.key == key)
    }

    pub fn listed(&self) -> Vec<String> {
        self.items.iter().map(|e| e.listed.clone()).collect()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    Passive,
    #[default]
    Active,
    NeedsAttention,
}

/// A premultiplied RGBA icon.
#[derive(Clone, Debug, PartialEq)]
pub struct Icon {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// An item's properties as read, before any check. A wrong-typed property is `None`.
#[derive(Debug, Default)]
pub struct Raw {
    pub id: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub icon_name: Option<String>,
    pub theme_path: Option<String>,
    /// The `pick`ed IconPixmap entry: its size and ARGB32 bytes.
    pub pixmap: Option<(u32, u32, Vec<u8>)>,
    pub menu: Option<String>,
    pub item_is_menu: Option<bool>,
}

/// An item's properties, checked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Props {
    pub id: String,
    pub title: String,
    pub status: Status,
    pub icon_name: Option<String>,
    pub theme_path: Option<PathBuf>,
    pub pixmap: Option<Icon>,
    pub menu: Option<String>,
    pub item_is_menu: bool,
}

/// Text from another process: no controls or bidi overrides, one line, `TEXT` chars at most.
pub fn text(s: &str) -> String {
    crate::notify::sanitize(s, TEXT).replace('\n', " ")
}

/// An icon name that can't form a path: no separators, NUL or leading dot, 128 bytes at most.
pub fn icon_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
        && !name.chars().any(char::is_control)
}

impl Props {
    pub fn new(raw: Raw) -> Props {
        let status = match raw.status.as_deref() {
            Some("Passive") => Status::Passive,
            Some("NeedsAttention") => Status::NeedsAttention,
            _ => Status::Active,
        };
        let theme_path = raw
            .theme_path
            .filter(|p| p.len() <= 4096 && !p.contains('\0'))
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && !p.components().any(|c| c.as_os_str() == ".."));
        Props {
            id: text(raw.id.as_deref().unwrap_or("")),
            title: text(raw.title.as_deref().unwrap_or("")),
            status,
            icon_name: raw.icon_name.filter(|n| icon_name_ok(n)),
            theme_path,
            pixmap: raw.pixmap.and_then(|(w, h, argb)| {
                let ok = argb.len() as u64 == 4 * u64::from(w) * u64::from(h);
                ok.then(|| Icon {
                    w,
                    h,
                    rgba: premultiply(&argb),
                })
            }),
            menu: raw.menu.filter(|p| valid_path(p) && p != "/"),
            item_is_menu: raw.item_is_menu.unwrap_or(false),
        }
    }

    pub fn shown(&self) -> bool {
        self.status != Status::Passive
    }
}

/// Of the first 16 IconPixmap entries (width, height, byte length), those 1..=256 px a side
/// with exactly 4·w·h bytes: the smallest at least `want` px, else the largest.
pub fn pick(entries: &[(i32, i32, usize)], want: u32) -> Option<usize> {
    let valid = entries
        .iter()
        .take(PIXMAPS)
        .enumerate()
        .filter(|(_, (w, h, len))| {
            (1..=PIXMAP_SIDE).contains(w)
                && (1..=PIXMAP_SIDE).contains(h)
                && *len as u64 == 4 * *w as u64 * *h as u64
        })
        .map(|(i, &(w, h, _))| (i, w.max(h) as u32));
    let (big, small): (Vec<_>, Vec<_>) = valid.partition(|&(_, side)| side >= want);
    match big.iter().min_by_key(|e| e.1) {
        Some(&(i, _)) => Some(i),
        None => small.iter().max_by_key(|e| e.1).map(|e| e.0),
    }
}

/// ARGB32 in network byte order to premultiplied RGBA.
pub fn premultiply(argb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(argb.len());
    for &[a, r, g, b] in argb.as_chunks::<4>().0 {
        let k = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
        out.extend([k(r), k(g), k(b), a]);
    }
    out
}

/// The targets of the first items that fit, in order, at the band's vertical centre: the row
/// ends `CLEAR` px left of `end` and keeps `CLEAR` px clear of the clock's text, which ends at
/// `clock_end`. Later items have no target.
pub fn targets(end: i32, clock_end: i32, n: usize) -> Vec<Rect> {
    let room = end - CLEAR - (clock_end + CLEAR);
    let fit = if room < TARGET {
        0
    } else {
        ((room + GAP) / (TARGET + GAP)) as usize
    };
    let k = n.min(fit) as i32;
    let start = end - CLEAR - k * TARGET - (k - 1).max(0) * GAP;
    (0..k)
        .map(|i| Rect {
            x: start + i * (TARGET + GAP),
            y: (crate::session::BAND_H - TARGET) / 2,
            w: TARGET,
            h: TARGET,
        })
        .collect()
}

/// The index of the target at `(x, y)`; the space between targets is none.
pub fn hit(targets: &[Rect], x: i32, y: i32) -> Option<usize> {
    targets.iter().position(|r| r.contains(x, y))
}

/// A dbusmenu item after the caps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub separator: bool,
    /// `✓` or `•` when its toggle is on.
    pub mark: Option<char>,
    pub children: Vec<Node>,
}

/// A property value in a layout; anything of another type is `Other`.
pub enum Prop<'a> {
    Str(&'a str),
    Bool(bool),
    Int(i32),
    Other,
}

/// A layout as the bus delivered it.
pub trait Tree {
    fn id(&self) -> Option<i32>;
    fn prop(&self, key: &str) -> Option<Prop<'_>>;
    fn children(&self) -> Box<dyn Iterator<Item = &Self> + '_>;
}

/// `label` without `_` mnemonics: `_x` is `x`, `__` is `_`.
pub fn mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        match c {
            '_' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

/// The layout under `root`, capped: `MENU_WIDTH` children a level, `MENU_DEPTH` levels,
/// `MENU_NODES` in all. A node without an id is dropped; a wrong-typed property is ignored.
pub fn parse<T: Tree + ?Sized>(root: &T) -> Node {
    fn node<T: Tree + ?Sized>(t: &T, id: i32, depth: usize, budget: &mut usize) -> Node {
        let s = |k| match t.prop(k) {
            Some(Prop::Str(s)) => Some(s),
            _ => None,
        };
        let b = |k, default| match t.prop(k) {
            Some(Prop::Bool(b)) => b,
            _ => default,
        };
        let on = matches!(t.prop("toggle-state"), Some(Prop::Int(1)));
        let mark = match s("toggle-type") {
            Some("checkmark") if on => Some('✓'),
            Some("radio") if on => Some('•'),
            _ => None,
        };
        let mut children = Vec::new();
        if depth < MENU_DEPTH {
            for c in t.children().take(MENU_WIDTH) {
                if *budget == 0 {
                    break;
                }
                let Some(id) = c.id() else { continue };
                *budget -= 1;
                children.push(node(c, id, depth + 1, budget));
            }
        }
        Node {
            id,
            label: text(&mnemonic(s("label").unwrap_or(""))),
            enabled: b("enabled", true),
            visible: b("visible", true),
            separator: s("type") == Some("separator"),
            mark,
            children,
        }
    }
    let mut budget = MENU_NODES;
    node(root, root.id().unwrap_or(0), 0, &mut budget)
}

/// An open tray menu: the layout and the submenus drilled into, by id.
#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub root: Node,
    pub path: Vec<i32>,
}

impl Menu {
    fn level(&self) -> (&Node, usize) {
        let mut at = &self.root;
        let mut depth = 0;
        for id in &self.path {
            match at
                .children
                .iter()
                .find(|c| c.id == *id && !c.children.is_empty())
            {
                Some(c) => {
                    at = c;
                    depth += 1;
                }
                None => break,
            }
        }
        (at, depth)
    }

    /// A new layout; drilled-in submenus that are gone are left.
    pub fn update(&mut self, root: Node) {
        self.root = root;
        let depth = self.level().1;
        self.path.truncate(depth);
    }

    /// Opens submenu `id`; false if it's no submenu here.
    pub fn enter(&mut self, id: i32) -> bool {
        let (at, _) = self.level();
        let sub = at
            .children
            .iter()
            .any(|c| c.id == id && c.visible && c.enabled && !c.children.is_empty());
        if sub {
            self.path.push(id);
        }
        sub
    }

    /// Back out of a submenu; false at the top.
    pub fn back(&mut self) -> bool {
        self.path.pop().is_some()
    }

    /// The lines of the level shown, under `title` at the top or the submenu's label.
    pub fn lines(&self, title: &str) -> Vec<Line> {
        let (at, depth) = self.level();
        let header = if depth == 0 { title } else { &at.label };
        let mut out = vec![Line::Header(header.into())];
        if depth > 0 {
            out.push(Line::Item(Kind::Back, "Back".into()));
        }
        for c in at.children.iter().filter(|c| c.visible) {
            if c.separator {
                // No leading, trailing or doubled separators.
                if !matches!(out.last(), Some(Line::Sep | Line::Header(_))) {
                    out.push(Line::Sep);
                }
                continue;
            }
            let kind = Kind::Tray {
                id: c.id,
                mark: c.mark,
                sub: !c.children.is_empty(),
            };
            out.push(match c.enabled {
                true => Line::Item(kind, c.label.clone()),
                false => Line::Off(kind, c.label.clone()),
            });
        }
        if out.last() == Some(&Line::Sep) {
            out.pop();
        }
        out
    }
}

/// Which items to re-read, at most one per `COALESCE` each: a key marked dirty is due once
/// that long has passed since its last read.
pub struct Coalesce<K> {
    last: HashMap<K, Instant>,
    dirty: HashSet<K>,
}

impl<K> Default for Coalesce<K> {
    fn default() -> Self {
        Coalesce {
            last: HashMap::new(),
            dirty: HashSet::new(),
        }
    }
}

impl<K: Clone + Eq + std::hash::Hash> Coalesce<K> {
    pub fn mark(&mut self, key: K) {
        self.dirty.insert(key);
    }

    pub fn forget(&mut self, key: &K) {
        self.dirty.remove(key);
        self.last.remove(key);
    }

    /// A due key, now counted as read; else how long until the next is due (`None`: no
    /// dirty key).
    pub fn take(&mut self, now: Instant) -> Result<K, Option<Duration>> {
        let due = |k: &K| {
            self.last.get(k).map_or(Duration::ZERO, |&t| {
                COALESCE.saturating_sub(now.saturating_duration_since(t))
            })
        };
        let (k, wait) = self
            .dirty
            .iter()
            .map(|k| (k, due(k)))
            .min_by_key(|(_, d)| *d)
            .map(|(k, d)| (k.clone(), d))
            .ok_or(None)?;
        if !wait.is_zero() {
            return Err(Some(wait));
        }
        self.dirty.remove(&k);
        self.last.insert(k.clone(), now);
        Ok(k)
    }
}

/// Scroll on a target, in notches: a wheel's own, or one per `SCROLL_PX` of a touchpad.
#[derive(Default)]
pub struct Scroll {
    px: [f64; 2],
}

impl Scroll {
    /// Axis `i` (0 vertical, 1 horizontal) moved `px`, or `discrete` wheel notches. Returns the
    /// `Scroll` deltas to send, positive up or left, as Plasma sends them.
    pub fn feed(&mut self, i: usize, px: f64, discrete: i32) -> Vec<i32> {
        let notches = if discrete != 0 {
            self.px[i] = 0.0;
            discrete
        } else if px.is_finite() {
            self.px[i] += px;
            let n = (self.px[i] / SCROLL_PX).trunc();
            self.px[i] -= n * SCROLL_PX;
            n as i32
        } else {
            self.px[i] = 0.0;
            0
        };
        let one = if notches > 0 { -NOTCH } else { NOTCH };
        // A notch each, as a wheel sends them; a burst is capped.
        vec![one; notches.unsigned_abs().min(16) as usize]
    }

    pub fn clear(&mut self) {
        self.px = [0.0; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(
        r: &mut Registry,
        sender: &str,
        arg: &str,
        owner: Option<&str>,
    ) -> Result<bool, Refused> {
        r.register(sender, arg, owner).map(|e| e.is_some())
    }

    #[test]
    fn ac3_name_and_path_forms_and_duplicates() {
        let mut r = Registry::default();
        assert_eq!(reg(&mut r, ":1.5", "org.kde.foo", Some(":1.5")), Ok(true));
        assert_eq!(reg(&mut r, ":1.6", "/org/ayatana/item", None), Ok(true));
        assert_eq!(
            r.listed(),
            ["org.kde.foo/StatusNotifierItem", ":1.6/org/ayatana/item"]
        );
        assert_eq!(reg(&mut r, ":1.5", "org.kde.foo", Some(":1.5")), Ok(false));
        assert_eq!(reg(&mut r, ":1.6", "/org/ayatana/item", None), Ok(false));
        assert_eq!(r.listed().len(), 2);
        for bad in ["/", "/a//b", "/a/", "/a-b", "/é"] {
            let ok = bad == "/";
            assert_eq!(reg(&mut r, ":1.7", bad, None).is_ok(), ok, "{bad}");
        }
    }

    #[test]
    fn ac4_spoofing_and_caps() {
        let mut r = Registry::default();
        assert_eq!(
            reg(&mut r, ":1.5", "org.kde.foo", Some(":1.9")),
            Err(Refused::Spoofed)
        );
        assert_eq!(
            reg(&mut r, ":1.5", "org.kde.foo", None),
            Err(Refused::Spoofed)
        );
        assert_eq!(
            reg(&mut r, ":1.5", ":1.9", Some(":1.9")),
            Err(Refused::Spoofed)
        );
        assert!(r.listed().is_empty());
        for i in 0..PER_SENDER {
            assert_eq!(reg(&mut r, ":1.5", &format!("/i{i}"), None), Ok(true));
        }
        assert_eq!(reg(&mut r, ":1.5", "/i9", None), Err(Refused::SenderFull));
        for s in 1..TOTAL / PER_SENDER {
            for i in 0..PER_SENDER {
                assert_eq!(
                    reg(&mut r, &format!(":2.{s}"), &format!("/i{i}"), None),
                    Ok(true)
                );
            }
        }
        assert_eq!(r.listed().len(), TOTAL);
        assert_eq!(reg(&mut r, ":3.0", "/x", None), Err(Refused::Full));
    }

    #[test]
    fn ac5_owner_leaving_removes_only_its_items() {
        let mut r = Registry::default();
        reg(&mut r, ":1.5", "/a", None).unwrap();
        reg(&mut r, ":1.6", "/a", None).unwrap();
        reg(&mut r, ":1.5", "/b", None).unwrap();
        let gone = r.gone(":1.5");
        assert_eq!(gone.len(), 2);
        assert_eq!(r.listed(), [":1.6/a"]);
        let key = Key {
            sender: ":1.6".into(),
            path: "/a".into(),
        };
        assert!(r.contains(&key));
        assert!(r.remove(&key).is_some());
        assert!(r.listed().is_empty());
    }

    #[test]
    fn ac6_icon_names_never_form_paths() {
        assert!(icon_name_ok("nm-signal-75"));
        assert!(icon_name_ok(&"a".repeat(128)));
        for bad in [
            "",
            "a/b",
            "/etc/passwd",
            "..",
            ".hidden",
            "a\\b",
            "a\0b",
            "a\nb",
        ] {
            assert!(!icon_name_ok(bad), "{bad:?}");
        }
        assert!(!icon_name_ok(&"a".repeat(129)));
        let props = |name: &str, theme: &str| {
            Props::new(Raw {
                icon_name: Some(name.into()),
                theme_path: Some(theme.into()),
                ..Default::default()
            })
        };
        let p = props("x", "/usr/share/foo");
        assert_eq!(p.icon_name.as_deref(), Some("x"));
        assert_eq!(p.theme_path, Some(PathBuf::from("/usr/share/foo")));
        for theme in ["relative/dir", "/a/../b", ""] {
            assert_eq!(props("x", theme).theme_path, None, "{theme}");
        }
        assert_eq!(props("../x", "/a").icon_name, None);
    }

    #[test]
    fn ac7_hostile_pixmaps_and_conversion() {
        let len = |w: i32, h: i32| (4 * w * h) as usize;
        // Negative, mismatched, empty and huge entries are never picked.
        let bad = [
            (-1, 4, 16),
            (4, -1, 16),
            (4, 4, 15),
            (0, 0, 0),
            (100_000, 100_000, 0),
            (257, 1, len(257, 1)),
            (i32::MAX, i32::MAX, usize::MAX),
        ];
        assert_eq!(pick(&bad, 20), None);
        let good = [
            (16, 16, len(16, 16)),
            (22, 22, len(22, 22)),
            (48, 48, len(48, 48)),
        ];
        assert_eq!(pick(&good, 20), Some(1), "smallest at least 20");
        assert_eq!(pick(&good, 40), Some(2));
        assert_eq!(pick(&good, 64), Some(2), "else the largest");
        let mut many = vec![(1, 1, 3); 16];
        many.push((24, 24, len(24, 24)));
        assert_eq!(pick(&many, 20), None, "only 16 entries are considered");
        assert_eq!(
            premultiply(&[
                0x80, 0xFF, 0x00, 0x00, 0xFF, 0x10, 0x20, 0x30, 0, 0xFF, 0xFF, 0xFF
            ]),
            [128, 0, 0, 128, 0x10, 0x20, 0x30, 0xFF, 0, 0, 0, 0]
        );
        // A length mismatch that slipped past `pick` is still no icon.
        let p = Props::new(Raw {
            pixmap: Some((2, 2, vec![0; 15])),
            ..Default::default()
        });
        assert_eq!(p.pixmap, None);
        let p = Props::new(Raw {
            pixmap: Some((1, 1, vec![0x80, 0xFF, 0, 0])),
            ..Default::default()
        });
        assert_eq!(p.pixmap.unwrap().rgba, [128, 0, 0, 128]);
    }

    #[test]
    fn ac8_text_is_clean_and_capped() {
        let p = Props::new(Raw {
            title: Some(format!("a\u{202E}b\x07c\nd{}", "x".repeat(300))),
            id: Some("\u{2066}id".into()),
            ..Default::default()
        });
        assert!(p.title.starts_with("abc d"));
        assert_eq!(p.title.chars().count(), TEXT);
        assert!(p.title.ends_with('…'));
        assert_eq!(p.id, "id");
    }

    #[test]
    fn ac9_status() {
        let status = |s: Option<&str>| {
            Props::new(Raw {
                status: s.map(Into::into),
                ..Default::default()
            })
        };
        assert!(!status(Some("Passive")).shown());
        assert!(status(Some("Active")).shown());
        assert!(status(None).shown());
        let n = status(Some("NeedsAttention"));
        assert!(n.shown());
        assert_eq!(n.status, Status::NeedsAttention);
    }

    #[test]
    fn ac10_signals_coalesce_to_one_read_per_250_ms() {
        let mut c = Coalesce::<String>::default();
        let t0 = Instant::now();
        assert_eq!(c.take(t0), Err(None));
        let mut reads = 0;
        // 10 000 signals across 1 s, reads taken as they come due.
        for i in 0..10_000u64 {
            let now = t0 + Duration::from_micros(i * 100);
            c.mark("a".to_string());
            if c.take(now).is_ok() {
                reads += 1;
            }
        }
        let end = t0 + Duration::from_secs(1);
        reads += usize::from(c.take(end).is_ok());
        reads += usize::from(c.take(end + COALESCE).is_ok());
        assert!((4..=6).contains(&reads), "{reads}");
        // Another key isn't held back by the first.
        c.mark("a".to_string());
        c.mark("b".to_string());
        let now = end + 2 * COALESCE;
        assert!(c.take(now).is_ok());
        assert!(c.take(now).is_ok());
        assert_eq!(c.take(now), Err(None));
        c.mark("a".to_string());
        assert_eq!(c.take(now), Err(Some(COALESCE)));
        c.forget(&"a".to_string());
        assert_eq!(c.take(now), Err(None));
    }

    #[test]
    fn ac11_band_layout_and_hits() {
        for (w, clock_w) in [(1125 - 24, 60), (3440 - 24, 140)] {
            let clock_end = (w - clock_w) / 2 + clock_w;
            for end in [w - CLEAR, w - 12 - 40 - 27] {
                let t = targets(end, clock_end, 3);
                assert_eq!(t.len(), 3);
                assert_eq!(t[2].x + t[2].w, end - CLEAR, "row ends CLEAR left of {end}");
                for (a, b) in t.iter().zip(&t[1..]) {
                    assert_eq!((a.w, b.x - (a.x + a.w)), (TARGET, GAP));
                }
                assert_eq!((t[0].y, t[0].h), (2, 28), "centred in the 32 px band");
                assert_eq!(hit(&t, t[1].x, t[1].y), Some(1));
                assert_eq!(hit(&t, t[1].x + t[1].w + 1, 16), None, "a gap is nothing");
                assert_eq!(hit(&t, t[0].x - 1, 16), None);
                // Overflow hides the newest: as many as fit, the first ones.
                let all = targets(end, clock_end, 100);
                let last = all.last().unwrap();
                assert!(all[0].x >= clock_end + CLEAR);
                assert!(all[0].x - (TARGET + GAP) < clock_end + CLEAR);
                assert_eq!(last.x + last.w, end - CLEAR);
            }
        }
        assert!(targets(100, 90, 3).is_empty());
    }

    /// A test layout.
    struct T {
        id: Option<i32>,
        props: Vec<(&'static str, Prop<'static>)>,
        kids: Vec<T>,
    }

    impl Tree for T {
        fn id(&self) -> Option<i32> {
            self.id
        }
        fn prop(&self, key: &str) -> Option<Prop<'_>> {
            self.props
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| match v {
                    Prop::Str(s) => Prop::Str(s),
                    Prop::Bool(b) => Prop::Bool(*b),
                    Prop::Int(i) => Prop::Int(*i),
                    Prop::Other => Prop::Other,
                })
        }
        fn children(&self) -> Box<dyn Iterator<Item = &Self> + '_> {
            Box::new(self.kids.iter())
        }
    }

    fn t(id: i32, props: Vec<(&'static str, Prop<'static>)>, kids: Vec<T>) -> T {
        T {
            id: Some(id),
            props,
            kids,
        }
    }

    fn labels(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| match l {
                Line::Header(h) => format!("[{h}]"),
                Line::Item(Kind::Tray { mark, sub, .. }, s) => {
                    format!(
                        "{s}{}{}",
                        mark.map_or(String::new(), |m| format!(" {m}")),
                        if *sub { " ›" } else { "" }
                    )
                }
                Line::Item(_, s) => s.clone(),
                Line::Off(_, s) => format!("({s})"),
                Line::Sep => "-".into(),
            })
            .collect()
    }

    #[test]
    fn ac14_menu_lines_marks_submenus_and_back() {
        use Prop::*;
        let root = t(
            0,
            vec![],
            vec![
                t(1, vec![("label", Str("_Open"))], vec![]),
                t(2, vec![("type", Str("separator"))], vec![]),
                t(
                    3,
                    vec![("label", Str("Hidden")), ("visible", Bool(false))],
                    vec![],
                ),
                t(
                    4,
                    vec![("label", Str("Off")), ("enabled", Bool(false))],
                    vec![],
                ),
                t(
                    5,
                    vec![
                        ("label", Str("Wi__Fi")),
                        ("toggle-type", Str("checkmark")),
                        ("toggle-state", Int(1)),
                    ],
                    vec![],
                ),
                t(
                    6,
                    vec![
                        ("label", Str("Radio")),
                        ("toggle-type", Str("radio")),
                        ("toggle-state", Int(1)),
                    ],
                    vec![],
                ),
                t(
                    7,
                    vec![
                        ("label", Str("Unchecked")),
                        ("toggle-type", Str("checkmark")),
                        ("toggle-state", Int(0)),
                    ],
                    vec![],
                ),
                t(
                    8,
                    vec![("label", Str("More"))],
                    vec![t(9, vec![("label", Str("Inner"))], vec![])],
                ),
            ],
        );
        let mut m = Menu {
            root: parse(&root),
            path: Vec::new(),
        };
        assert_eq!(
            labels(&m.lines("App")),
            [
                "[App]",
                "Open",
                "-",
                "(Off)",
                "Wi_Fi ✓",
                "Radio •",
                "Unchecked",
                "More ›"
            ]
        );
        assert!(!m.enter(1), "no submenu");
        assert!(m.enter(8));
        assert_eq!(labels(&m.lines("App")), ["[More]", "Back", "Inner"]);
        // A new layout without the submenu leaves it.
        let mut gone = parse(&root);
        gone.children.pop();
        m.update(gone);
        assert_eq!(m.path, Vec::<i32>::new());
        assert!(m.enter(8) || m.root.children.iter().all(|c| c.id != 8));
        m.update(parse(&root));
        assert!(m.enter(8));
        assert!(m.back());
        assert!(!m.back());
        assert_eq!(mnemonic("__a_b_"), "_ab");
    }

    #[test]
    fn ac15_hostile_layouts_are_capped() {
        use Prop::*;
        let wide = t(
            0,
            vec![],
            (0..10_000)
                .map(|i| t(i, vec![("label", Str("x"))], vec![]))
                .collect(),
        );
        assert_eq!(parse(&wide).children.len(), MENU_WIDTH);
        let mut deep = t(100, vec![], vec![]);
        for i in (0..100).rev() {
            deep = t(i, vec![], vec![deep]);
        }
        fn depth(n: &Node) -> usize {
            n.children.first().map_or(0, |c| 1 + depth(c))
        }
        assert_eq!(depth(&parse(&deep)), MENU_DEPTH);
        // 64 × 64 × ...: the whole tree stops at MENU_NODES.
        fn bushy(d: usize) -> T {
            t(
                1,
                vec![],
                if d == 0 {
                    vec![]
                } else {
                    (0..64).map(|_| bushy(d - 1)).collect()
                },
            )
        }
        fn count(n: &Node) -> usize {
            n.children.iter().map(|c| 1 + count(c)).sum()
        }
        assert_eq!(count(&parse(&bushy(2))), MENU_NODES);
        // Wrong types are ignored per property; a child without an id is dropped.
        let odd = t(
            0,
            vec![],
            vec![
                t(
                    1,
                    vec![
                        ("label", Int(3)),
                        ("enabled", Str("no")),
                        ("visible", Other),
                    ],
                    vec![],
                ),
                T {
                    id: None,
                    props: vec![],
                    kids: vec![],
                },
            ],
        );
        let n = parse(&odd);
        assert_eq!(n.children.len(), 1);
        assert_eq!(
            n.children[0],
            Node {
                id: 1,
                enabled: true,
                visible: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn ac13_scroll_notches_and_touchpad_accumulator() {
        let mut s = Scroll::default();
        assert_eq!(s.feed(0, 15.0, 1), [-NOTCH], "wheel down");
        assert_eq!(s.feed(0, -15.0, -1), [NOTCH], "wheel up");
        assert_eq!(s.feed(1, 15.0, 1), [-NOTCH], "horizontal");
        assert!(s.feed(0, 10.0, 0).is_empty());
        assert_eq!(s.feed(0, 10.0, 0), [-NOTCH], "15 px accumulated");
        assert!(s.feed(0, 5.0, 0).is_empty(), "5 px left over, now 10");
        s.clear();
        assert!(
            s.feed(0, 10.0, 0).is_empty(),
            "the remainder cleared on leave"
        );
        assert!(s.feed(0, f64::NAN, 0).is_empty());
        assert_eq!(s.feed(0, 1e9, 0).len(), 16);
    }
}
