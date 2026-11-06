//! Monitors on real hardware: EDID identity, the arrangement file and its first-run import from
//! KWin, the main monitor and priority, the pointer clamp, and the taskbar's hover band. Pure, no
//! Smithay types.
//!
//! `$XDG_CONFIG_HOME/navi/monitors`:
//!
//! ```text
//! navi-monitors 1
//! <identity> <x> <y> <scale>
//! main <identity>
//! ```
//!
//! x and y are logical px, the scale is 0.5–4.0, and the `main` line is optional.

use std::collections::{HashMap, HashSet};

use crate::layout::{Rect, safe};

const HEADER: &str = "navi-monitors 1";
const MAX_POS: i32 = 65535;
/// How long the pointer rests on another monitor's bottom edge before the taskbar moves there.
pub const HOVER_MS: u64 = 300;
/// The bottom edge band, in logical px.
const BAND: f64 = 2.0;

/// KWin's `edidIdentifier`, spaces as `_`: `<PNP> <product> <serial> <week> <year> <model
/// year>`, from the base block. `None` unless it has the header and a valid checksum.
pub fn identity(edid: &[u8]) -> Option<String> {
    let b = edid.get(..128)?;
    if b[..8] != [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0] {
        return None;
    }
    if b.iter().fold(0u8, |s, &x| s.wrapping_add(x)) != 0 {
        return None;
    }
    let product = u16::from_le_bytes([b[10], b[11]]);
    let serial = u32::from_le_bytes([b[12], b[13], b[14], b[15]]);
    let year = u32::from(b[17]) + 1990;
    let (week, year, model_year) = match b[16] {
        0xFF => (0, 0, year),
        w => (u32::from(w), year, 0),
    };
    Some(format!(
        "{}_{product}_{serial}_{week}_{year}_{model_year}",
        pnp(b)
    ))
}

/// The manufacturer's three letters, 5 bits each from bytes 8–9, 1 = `A`.
fn pnp(b: &[u8]) -> String {
    let v = u16::from_be_bytes([b[8], b[9]]);
    [10, 5, 0]
        .iter()
        .map(|s| char::from(b'@' + ((v >> s) & 0x1F) as u8))
        .collect()
}

/// Make and model for `wl_output`: the PNP id, and the monitor name descriptor (printable ASCII
/// only) or the product code.
pub fn make_model(edid: &[u8]) -> Option<(String, String)> {
    identity(edid)?;
    let product = u16::from_le_bytes([edid[10], edid[11]]);
    let name = (0..4)
        .map(|i| &edid[54 + 18 * i..72 + 18 * i])
        .find(|d| d[..4] == [0, 0, 0, 0xFC])
        .map(|d| {
            let text = d[5..].split(|&c| c == b'\n').next().unwrap_or(&[]);
            let text: String = text
                .iter()
                .filter(|c| c.is_ascii_graphic() || **c == b' ')
                .map(|&c| char::from(c))
                .collect();
            text.trim().to_string()
        })
        .filter(|s| !s.is_empty());
    Some((pnp(edid), name.unwrap_or_else(|| format!("{product:04X}"))))
}

/// The built-in panel.
pub fn is_laptop(connector: &str) -> bool {
    ["eDP", "LVDS", "DSI"]
        .iter()
        .any(|p| connector.starts_with(p))
}

/// Each connector's identity: its EDID's, else the connector name. Repeats, and identities in
/// `held` by monitors already connected, get `_2`, `_3`… in connector-name order. The flags say
/// which fell back to the connector name.
pub fn identities(conns: &[(&str, Option<&[u8]>)], held: &[String]) -> Vec<(String, bool)> {
    let mut order: Vec<usize> = (0..conns.len()).collect();
    order.sort_by_key(|&i| conns[i].0);
    let mut out = vec![(String::new(), false); conns.len()];
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut taken: HashSet<String> = held.iter().cloned().collect();
    for i in order {
        let (name, edid) = conns[i];
        let (base, fallback) = match edid.and_then(identity) {
            Some(id) => (id, false),
            None => (name.to_string(), true),
        };
        let n = seen.entry(base.clone()).or_insert(0);
        let mut id = base.clone();
        while !taken.insert(id.clone()) {
            *n += 1;
            id = format!("{base}_{}", *n + 1);
        }
        out[i] = (id, fallback);
    }
    out
}

/// A connected monitor.
#[derive(Clone, Debug, PartialEq)]
pub struct Mon {
    pub connector: String,
    pub id: String,
    /// Its mode, in physical px.
    pub size: (i32, i32),
    /// Where it is, in logical px.
    pub rect: Rect,
}

impl Mon {
    fn laptop(&self) -> bool {
        is_laptop(&self.connector)
    }

    fn area(&self) -> i64 {
        i64::from(self.rect.w) * i64::from(self.rect.h)
    }
}

/// The logical size of a `size` mode at `scale`.
pub fn logical((w, h): (i32, i32), scale: f64) -> (i32, i32) {
    let l = |v: i32| ((f64::from(v) / scale).round() as i32).max(1);
    (l(w), l(h))
}

/// Length of the edge `a` and `b` share; 0 for corners, gaps and overlaps.
fn shared_edge(a: Rect, b: Rect) -> i32 {
    let overlap = |a0: i32, a1: i32, b0: i32, b1: i32| (a1.min(b1) - a0.max(b0)).max(0);
    let x = overlap(a.x, a.x + a.w, b.x, b.x + b.w);
    let y = overlap(a.y, a.y + a.h, b.y, b.y + b.h);
    let vertical = a.x + a.w == b.x || b.x + b.w == a.x;
    let horizontal = a.y + a.h == b.y || b.y + b.h == a.y;
    match (vertical, horizontal) {
        (true, false) => y,
        (false, true) => x,
        _ => 0,
    }
}

/// How many other monitors `i` borders.
pub fn borders(mons: &[Mon], i: usize) -> usize {
    (0..mons.len())
        .filter(|&j| j != i && shared_edge(mons[i].rect, mons[j].rect) > 0)
        .count()
}

/// The main monitor: `pinned` if connected, else the one bordering the most others, then the
/// largest, then by connector name. The laptop panel only when nothing else is connected.
pub fn main(mons: &[Mon], pinned: Option<&str>) -> usize {
    let others = mons.iter().any(|m| !m.laptop());
    let allowed = |m: &Mon| !(others && m.laptop());
    if let Some(i) = mons
        .iter()
        .position(|m| Some(m.id.as_str()) == pinned && allowed(m))
    {
        return i;
    }
    (0..mons.len())
        .filter(|&i| allowed(&mons[i]))
        .min_by(|&a, &b| {
            (borders(mons, b), mons[b].area())
                .cmp(&(borders(mons, a), mons[a].area()))
                .then(mons[a].connector.cmp(&mons[b].connector))
        })
        .unwrap_or(0)
}

/// Monitor indices by priority: `main`, then the rest by logical area, largest first (ties by
/// connector name), and the laptop panel last when anything else is connected.
pub fn priority(mons: &[Mon], main: usize) -> Vec<usize> {
    let mut rest: Vec<usize> = (0..mons.len()).filter(|&i| i != main).collect();
    rest.sort_by(|&a, &b| {
        let (ma, mb) = (&mons[a], &mons[b]);
        (ma.laptop(), mb.area(), &ma.connector).cmp(&(mb.laptop(), ma.area(), &mb.connector))
    });
    let mut out = vec![main];
    out.extend(rest);
    out.retain(|&i| i < mons.len());
    out
}

/// One monitor's line in the arrangement file.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
}

/// The arrangement file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arrangement {
    pub entries: Vec<Entry>,
    pub main: Option<String>,
    pub modes: std::collections::BTreeMap<String, (i32, i32, i32)>,
    pub rotations: std::collections::BTreeMap<String, u16>,
    pub disabled: std::collections::BTreeSet<String>,
    pub mirrors: std::collections::BTreeMap<String, String>,
    pub vrr: std::collections::BTreeSet<String>,
}

impl Arrangement {
    /// The whole file, or why it is ignored.
    pub fn parse(text: &str) -> Result<Arrangement, String> {
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Err("not a navi-monitors 1 file".into());
        }
        let mut a = Arrangement::default();
        let mut ids = HashSet::new();
        for (n, line) in lines.enumerate() {
            let bad = || format!("line {}", n + 2);
            match line.split(' ').collect::<Vec<_>>()[..] {
                ["mode", id, width, height, refresh] if safe(id) && !a.modes.contains_key(id) => {
                    let dimension =
                        |s: &str| s.parse::<i32>().ok().filter(|v| (1..=16384).contains(v));
                    let (Some(w), Some(h), Some(r)) = (
                        dimension(width),
                        dimension(height),
                        refresh
                            .parse::<i32>()
                            .ok()
                            .filter(|v| (1000..=1000000).contains(v)),
                    ) else {
                        return Err(bad());
                    };
                    a.modes.insert(id.into(), (w, h, r));
                }
                ["rotation", id, angle] if safe(id) && !a.rotations.contains_key(id) => {
                    let angle = angle
                        .parse::<u16>()
                        .ok()
                        .filter(|n| [0, 90, 180, 270].contains(n))
                        .ok_or_else(bad)?;
                    a.rotations.insert(id.into(), angle);
                }
                ["disabled", id] if safe(id) && a.disabled.insert(id.into()) => {}
                ["vrr", id] if safe(id) && a.vrr.insert(id.into()) => {}
                ["mirror", id, source]
                    if safe(id) && safe(source) && id != source && !a.mirrors.contains_key(id) =>
                {
                    a.mirrors.insert(id.into(), source.into());
                }
                ["main", id] if safe(id) && a.main.is_none() => a.main = Some(id.into()),
                [id, x, y, scale] if safe(id) && id != "main" && ids.insert(id) => {
                    let pos = |s: &str| {
                        s.parse()
                            .ok()
                            .filter(|v: &i32| (-MAX_POS..=MAX_POS).contains(v))
                    };
                    let scale = scale
                        .parse::<f64>()
                        .ok()
                        .filter(|s| (0.5..=4.0).contains(s));
                    let (Some(x), Some(y), Some(scale)) = (pos(x), pos(y), scale) else {
                        return Err(bad());
                    };
                    a.entries.push(Entry {
                        id: id.into(),
                        x,
                        y,
                        scale,
                    });
                }
                _ => return Err(bad()),
            }
        }
        Ok(a)
    }

    pub fn text(&self) -> String {
        let mut t = format!("{HEADER}\n");
        for e in &self.entries {
            t += &format!("{} {} {} {}\n", e.id, e.x, e.y, e.scale);
        }
        if let Some(m) = &self.main {
            t += &format!("main {m}\n");
        }
        for (id, (w, h, r)) in &self.modes {
            t += &format!("mode {id} {w} {h} {r}\n");
        }
        for (id, angle) in &self.rotations {
            t += &format!("rotation {id} {angle}\n");
        }
        for id in &self.disabled {
            t += &format!("disabled {id}\n");
        }
        for (id, source) in &self.mirrors {
            t += &format!("mirror {id} {source}\n");
        }
        for id in &self.vrr {
            t += &format!("vrr {id}\n");
        }
        t
    }

    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Each monitor's logical rect and scale, as `mons` (`rect` unused) are ordered. Monitors not
    /// in the file, and any whose rect overlaps one placed before it, go right of everything,
    /// top-aligned with the main monitor; new ones at scale 1. Returns whether the file changed.
    pub fn place(&mut self, mons: &[Mon]) -> (Vec<(Rect, f64)>, bool) {
        let mut placed: Vec<Option<(Rect, f64)>> = vec![None; mons.len()];
        for e in &self.entries {
            let Some(i) = mons.iter().position(|m| m.id == e.id) else {
                continue;
            };
            let (w, h) = logical(
                oriented(
                    mons[i].size,
                    self.rotations.get(&e.id).copied().unwrap_or(0),
                ),
                e.scale,
            );
            let r = Rect {
                x: e.x,
                y: e.y,
                w,
                h,
            };
            if self.disabled.contains(&e.id)
                || self.mirrors.contains_key(&e.id)
                || !placed.iter().enumerate().any(|(j, p)| {
                    !self.disabled.contains(&mons[j].id)
                        && !self.mirrors.contains_key(&mons[j].id)
                        && p.as_ref().is_some_and(|(p, _)| overlaps(*p, r))
                })
            {
                placed[i] = Some((r, e.scale));
            }
        }
        let mut rest: Vec<usize> = (0..mons.len()).filter(|&i| placed[i].is_none()).collect();
        rest.sort_by(|&a, &b| mons[a].connector.cmp(&mons[b].connector));
        let changed = !rest.is_empty();
        for i in rest {
            let done: Vec<Mon> = (0..mons.len())
                .filter_map(|j| {
                    let (rect, _) = placed[j]?;
                    Some(Mon {
                        rect,
                        ..mons[j].clone()
                    })
                })
                .collect();
            let x = done.iter().map(|m| m.rect.x + m.rect.w).max().unwrap_or(0);
            let y = match done.is_empty() {
                true => 0,
                false => done[main(&done, self.main.as_deref())].rect.y,
            };
            let scale = self.entry(&mons[i].id).map_or(1.0, |e| e.scale);
            let (w, h) = logical(
                oriented(
                    mons[i].size,
                    self.rotations.get(&mons[i].id).copied().unwrap_or(0),
                ),
                scale,
            );
            placed[i] = Some((Rect { x, y, w, h }, scale));
            let fresh = Entry {
                id: mons[i].id.clone(),
                x,
                y,
                scale,
            };
            match self.entries.iter_mut().find(|e| e.id == fresh.id) {
                Some(e) => *e = fresh,
                None => self.entries.push(fresh),
            }
        }
        (placed.into_iter().flatten().collect(), changed)
    }
}

fn oriented((w, h): (i32, i32), rotation: u16) -> (i32, i32) {
    if rotation == 90 || rotation == 270 {
        (h, w)
    } else {
        (w, h)
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// One row at y 0, scale 1: the laptop panel leftmost, then the rest by connector name.
pub fn auto(mons: &[Mon]) -> Arrangement {
    let mut order: Vec<&Mon> = mons.iter().collect();
    order.sort_by(|a, b| (!a.laptop(), &a.connector).cmp(&(!b.laptop(), &b.connector)));
    let mut x = 0;
    let entries = order
        .into_iter()
        .map(|m| {
            let e = Entry {
                id: m.id.clone(),
                x,
                y: 0,
                scale: 1.0,
            };
            x += m.size.0;
            e
        })
        .collect();
    Arrangement {
        entries,
        main: None,
        modes: Default::default(),
        ..Default::default()
    }
}

/// The monitors file from KWin's `kwinoutputconfig.json`: the setup whose enabled outputs are
/// exactly `connected`, lid open preferred. `None` for a damaged file or no match.
pub fn import(json: &str, connected: &[Mon]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let section = |name: &str| {
        v.as_array()?
            .iter()
            .find(|s| s["name"] == name)?
            .get("data")?
            .as_array()
    };
    let outputs = section("outputs")?;
    let want: HashSet<&str> = connected.iter().map(|m| m.id.as_str()).collect();
    let setup = |s: &serde_json::Value| -> Option<Arrangement> {
        let mut entries = Vec::new();
        for o in s["outputs"].as_array()? {
            if o["enabled"] != true {
                continue;
            }
            let out = outputs.get(usize::try_from(o["outputIndex"].as_u64()?).ok()?)?;
            let id = out["edidIdentifier"].as_str()?.replace(' ', "_");
            let scale = out["scale"].as_f64().filter(|s| (0.5..=4.0).contains(s))?;
            let pos = |k: &str| {
                let p = i32::try_from(o["position"][k].as_i64()?).ok()?;
                (p.abs() <= MAX_POS).then_some(p)
            };
            entries.push(Entry {
                x: pos("x")?,
                y: pos("y")?,
                id,
                scale,
            });
        }
        let ids: HashSet<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        (ids == want && ids.len() == entries.len()).then_some(Arrangement {
            entries,
            main: None,
            modes: Default::default(),
            ..Default::default()
        })
    };
    let setups = section("setups")?;
    let lid_open = |s: &&serde_json::Value| s["lidClosed"] != true;
    let best = setups
        .iter()
        .filter(lid_open)
        .chain(setups.iter().filter(|s| !lid_open(s)))
        .find_map(setup)?;
    Some(best.text())
}

/// The nearest point to `p` inside the union of `rects` (by priority, which wins ties). A point
/// inside one is unchanged.
pub fn clamp(p: (f64, f64), rects: &[Rect]) -> (f64, f64) {
    let inside = |r: &Rect| {
        p.0 >= f64::from(r.x)
            && p.0 < f64::from(r.x + r.w)
            && p.1 >= f64::from(r.y)
            && p.1 < f64::from(r.y + r.h)
    };
    if rects.is_empty() || rects.iter().any(inside) {
        return p;
    }
    let near = |r: &Rect| {
        let c = |v: f64, lo: i32, len: i32| v.clamp(f64::from(lo), f64::from(lo + len.max(1) - 1));
        (c(p.0, r.x, r.w), c(p.1, r.y, r.h))
    };
    let d = |q: (f64, f64)| (q.0 - p.0).powi(2) + (q.1 - p.1).powi(2);
    rects
        .iter()
        .map(near)
        .fold(None, |best: Option<(f64, f64)>, q| match best {
            Some(b) if d(b) <= d(q) => Some(b),
            _ => Some(q),
        })
        .unwrap_or(p)
}

/// A global logical rect in an output's own physical px, given its origin and scale. Edges are
/// rounded, so rects that touch still touch.
pub fn to_local(r: Rect, origin: (i32, i32), scale: f64) -> Rect {
    let px = |v: i32, o: i32| (f64::from(v - o) * scale).round() as i32;
    let (x, y) = (px(r.x, origin.0), px(r.y, origin.1));
    Rect {
        x,
        y,
        w: px(r.x + r.w, origin.0) - x,
        h: px(r.y + r.h, origin.1) - y,
    }
}

/// What the hover band asks the backend to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Hover {
    Idle,
    /// Feed the pointer again at this time, so a still pointer triggers too.
    Arm(u64),
    MoveTo(String),
}

/// The taskbar follows a rest of `HOVER_MS` on another output's bottom edge.
#[derive(Debug, Default)]
pub struct BarHover {
    /// The output whose band the pointer is in, since when, and whether it fired.
    on: Option<(String, u64, bool)>,
}

impl BarHover {
    pub fn feed(
        &mut self,
        p: (f64, f64),
        outputs: &[(String, Rect)],
        bar: &str,
        now_ms: u64,
    ) -> Hover {
        let band = outputs.iter().find(|(name, r)| {
            let bottom = f64::from(r.y + r.h);
            name != bar
                && p.0 >= f64::from(r.x)
                && p.0 < f64::from(r.x + r.w)
                && p.1 >= bottom - BAND
                && p.1 < bottom
        });
        let Some((name, _)) = band else {
            self.on = None;
            return Hover::Idle;
        };
        let (since, fired) = match &mut self.on {
            Some((o, since, fired)) if o == name => (*since, fired),
            on => {
                *on = Some((name.clone(), now_ms, false));
                return Hover::Arm(now_ms + HOVER_MS);
            }
        };
        match (*fired, now_ms >= since + HOVER_MS) {
            (true, _) => Hover::Idle,
            (false, true) => {
                *fired = true;
                Hover::MoveTo(name.clone())
            }
            (false, false) => Hover::Arm(since + HOVER_MS),
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A valid base block with these fields and a name descriptor.
    pub fn edid(pnp: &str, product: u16, serial: u32, week: u8, year_byte: u8) -> Vec<u8> {
        let mut b = vec![0u8; 128];
        b[..8].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        let l: Vec<u16> = pnp.bytes().map(|c| u16::from(c - b'@')).collect();
        b[8..10].copy_from_slice(&((l[0] << 10) | (l[1] << 5) | l[2]).to_be_bytes());
        b[10..12].copy_from_slice(&product.to_le_bytes());
        b[12..16].copy_from_slice(&serial.to_le_bytes());
        b[16] = week;
        b[17] = year_byte;
        b[54..59].copy_from_slice(&[0, 0, 0, 0xFC, 0]);
        b[59..72].copy_from_slice(b"Test Mon\n    ");
        let sum = b[..127].iter().fold(0u8, |s, &x| s.wrapping_add(x));
        b[127] = 0u8.wrapping_sub(sum);
        b
    }

    pub fn rig_edids() -> [Vec<u8>; 3] {
        [
            edid("SDC", 16797, 0, 0, 32),
            edid("ACR", 1359, u32::MAX, 0xFF, 26),
            edid("SAM", 29247, 0, 42, 31),
        ]
    }

    pub const LAPTOP: &str = "SDC_16797_0_0_2022_0";
    pub const UW: &str = "ACR_1359_4294967295_0_0_2016";
    pub const UHD: &str = "SAM_29247_0_42_2021_0";

    fn mon(connector: &str, id: &str, size: (i32, i32), x: i32, y: i32, scale: f64) -> Mon {
        let (w, h) = logical(size, scale);
        Mon {
            connector: connector.into(),
            id: id.into(),
            size,
            rect: Rect { x, y, w, h },
        }
    }

    /// Yoshi's rig, from his `kwinoutputconfig.json`.
    pub fn rig() -> Vec<Mon> {
        vec![
            mon("eDP-1", LAPTOP, (2880, 1800), 0, 2306, 1.6),
            mon("DP-11", UW, (3440, 1440), 1800, 2160, 1.0),
            mon("DP-13", UHD, (3840, 2160), 1640, 0, 1.0),
        ]
    }

    #[test]
    fn advanced_arrangement_round_trips_and_rejects_bad_data() {
        let text = "navi-monitors 1\na 0 0 1\nb 800 0 1\nmain a\nrotation a 90\ndisabled b\nmirror b a\nvrr a\n";
        let a = Arrangement::parse(text).unwrap();
        assert_eq!(Arrangement::parse(&a.text()).unwrap(), a);
        for line in [
            "rotation a 45",
            "rotation a NaN",
            "mirror a a",
            "disabled a b",
            "vrr a\nvrr a",
        ] {
            assert!(Arrangement::parse(&format!("navi-monitors 1\n{line}\n")).is_err());
        }
    }
    #[test]
    fn rotation_and_mirrors_keep_the_saved_layout_on_restart() {
        let mons = vec![
            mon("DP-1", "a", (1920, 1080), 0, 0, 1.0),
            mon("DP-2", "b", (1920, 1080), 1080, 0, 1.0),
        ];
        let mut a =
            Arrangement::parse("navi-monitors 1\na 0 0 1\nb 1080 0 1\nrotation a 90\n").unwrap();
        let (placed, changed) = a.place(&mons);
        assert!(!changed);
        assert_eq!(
            (placed[0].0.w, placed[0].0.h, placed[1].0.x),
            (1080, 1920, 1080)
        );
        let mut a = Arrangement::parse("navi-monitors 1\na 0 0 1\nb 0 0 1\nmirror b a\n").unwrap();
        let (placed, changed) = a.place(&mons);
        assert!(!changed);
        assert_eq!(placed[0].0, placed[1].0);
    }
    #[test]
    fn ac2_rig_identities() {
        let ids: Vec<Option<String>> = rig_edids().iter().map(|e| identity(e)).collect();
        assert_eq!(
            ids,
            [Some(LAPTOP.into()), Some(UW.into()), Some(UHD.into())]
        );
        let (make, model) = make_model(&rig_edids()[1]).unwrap();
        assert_eq!((make.as_str(), model.as_str()), ("ACR", "Test Mon"));
    }

    #[test]
    fn ac2_bad_edids_have_no_identity() {
        let good = edid("SAM", 1, 2, 3, 4);
        let mut sum = good.clone();
        sum[127] ^= 1;
        let mut header = good.clone();
        header[0] = 1;
        header[127] = header[127].wrapping_sub(1);
        assert!(identity(&good).is_some());
        for bad in [&sum[..], &header[..], &good[..127], &[]] {
            assert_eq!(identity(bad), None);
            assert_eq!(make_model(bad), None);
        }
    }

    #[test]
    fn ac2_duplicates_and_fallbacks() {
        let e = edid("SAM", 1, 2, 3, 4);
        let id = identity(&e).unwrap();
        // Given out of order: suffixes follow connector names.
        let got = identities(
            &[
                ("DP-2", Some(&e)),
                ("DP-1", Some(&e)),
                ("HDMI-A-1", None),
                ("DP-3", Some(&e[..100])),
            ],
            &[],
        );
        assert_eq!(
            got,
            [
                (format!("{id}_2"), false),
                (id.clone(), false),
                ("HDMI-A-1".into(), true),
                ("DP-3".into(), true),
            ]
        );
        // A fallback that collides with a real identity still gets a suffix.
        let got = identities(&[("DP-1", None), ("DP-2", None)], &[]);
        assert_eq!(got[0].0, "DP-1");
        assert_eq!(got[1].0, "DP-2");
        // Two identical monitors; the first unplugged, a third plugged in elsewhere: the new one
        // can't take `_2`, which the one still connected holds.
        let got = identities(&[("DP-3", Some(&e))], &[format!("{id}_2")]);
        assert_eq!(got[0].0, id);
        let got = identities(&[("DP-3", Some(&e))], &[id.clone(), format!("{id}_2")]);
        assert_eq!(got[0].0, format!("{id}_3"));
    }

    #[test]
    fn ac2_fuzz_never_panics_and_ids_are_safe() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut valid = 0;
        for n in 0..10_000 {
            let len = if n % 2 == 0 { 128 } else { 256 };
            let mut b: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if n % 3 == 0 {
                // Valid header and checksum, random fields.
                b[..8].copy_from_slice(&[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
                let sum = b[..127].iter().fold(0u8, |s, &x| s.wrapping_add(x));
                b[127] = 0u8.wrapping_sub(sum);
            }
            if let Some(id) = identity(&b) {
                valid += 1;
                assert!(safe(&id), "{id:?}");
                let (make, model) = make_model(&b).unwrap();
                assert!(safe(&make) && !model.chars().any(|c| c.is_control()));
            }
        }
        assert!(valid > 3000, "{valid}");
        for (id, _) in identities(&[("DP-1", Some(&rig_edids()[0])), ("DP-2", None)], &[]) {
            assert!(safe(&id));
        }
    }

    /// AC3: this machine's monitors against KWin's identities. Read-only; skipped without them.
    #[test]
    #[ignore = "reads this machine's monitors"]
    fn host_edid() {
        let Some(home) = std::env::var_os("HOME") else {
            return println!("no HOME: skipped");
        };
        let kwin = std::path::Path::new(&home).join(".config/kwinoutputconfig.json");
        let Ok(json) = std::fs::read_to_string(&kwin) else {
            return println!("no {}: skipped", kwin.display());
        };
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let outputs = v
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "outputs")
            .unwrap()["data"]
            .as_array()
            .unwrap();
        let Ok(dirs) = std::fs::read_dir("/sys/class/drm") else {
            return println!("no /sys/class/drm: skipped");
        };
        let mut checked = 0;
        for d in dirs.flatten() {
            let name = d.file_name().to_string_lossy().into_owned();
            let Some((_, connector)) = name.split_once('-') else {
                continue;
            };
            let status = std::fs::read_to_string(d.path().join("status")).unwrap_or_default();
            let edid = std::fs::read(d.path().join("edid")).unwrap_or_default();
            if status.trim() != "connected" || edid.is_empty() {
                continue;
            }
            let id = identity(&edid).expect("a valid EDID");
            let theirs = outputs
                .iter()
                .find(|o| o["connectorName"] == connector)
                .and_then(|o| o["edidIdentifier"].as_str())
                .map(|s| s.replace(' ', "_"));
            println!("{connector}: navi {id}, KWin {theirs:?}");
            assert_eq!(Some(id), theirs, "{connector}");
            checked += 1;
        }
        println!("{checked} monitors match");
    }

    #[test]
    fn laptop_connectors() {
        for c in ["eDP-1", "LVDS-1", "DSI-1"] {
            assert!(is_laptop(c));
        }
        for c in ["DP-1", "HDMI-A-1", "Virtual-1"] {
            assert!(!is_laptop(c));
        }
    }

    #[test]
    fn ac4_rig_main_and_priority() {
        let r = rig();
        assert_eq!([borders(&r, 1), borders(&r, 2), borders(&r, 0)], [2, 1, 1]);
        assert_eq!(main(&r, None), 1);
        assert_eq!(priority(&r, 1), [1, 2, 0]);
        // Pinned to the 4K.
        assert_eq!(main(&r, Some(UHD)), 2);
        assert_eq!(priority(&r, 2), [2, 1, 0]);
        // Pinned to a monitor that isn't here, or to the laptop.
        assert_eq!(main(&r, Some("GONE_1_2_3_4_5")), 1);
        assert_eq!(main(&r, Some(LAPTOP)), 1);
    }

    #[test]
    fn ac4_row_with_laptop_in_the_middle() {
        let r = vec![
            mon("DP-1", "big", (2560, 1440), 0, 0, 1.0),
            mon("eDP-1", "lap", (1920, 1200), 2560, 0, 1.0),
            mon("DP-2", "small", (1920, 1080), 4480, 0, 1.0),
        ];
        assert_eq!(borders(&r, 1), 2);
        assert_eq!(main(&r, None), 0);
        assert_eq!(priority(&r, 0), [0, 2, 1]);
    }

    #[test]
    fn ac4_laptop_alone_and_with_the_ultrawide() {
        let r = rig();
        assert_eq!(main(&r[..1], None), 0);
        assert_eq!(priority(&r[..1], 0), [0]);
        let two = &r[..2];
        assert_eq!(main(two, None), 1);
        assert_eq!(priority(two, 1), [1, 0]);
    }

    #[test]
    fn ac4_corners_dont_border() {
        let r = vec![
            mon("DP-1", "a", (100, 100), 0, 0, 1.0),
            mon("DP-2", "b", (100, 100), 100, 100, 1.0),
        ];
        assert_eq!((borders(&r, 0), borders(&r, 1)), (0, 0));
        // Then the larger, then the connector name.
        assert_eq!(main(&r, None), 0);
    }

    fn rig_file() -> String {
        format!("{HEADER}\n{LAPTOP} 0 2306 1.6\n{UW} 1800 2160 1\n{UHD} 1640 0 1\nmain {UW}\n")
    }

    #[test]
    fn ac5_round_trip_is_byte_identical() {
        let text = rig_file();
        let a = Arrangement::parse(&text).unwrap();
        assert_eq!(a.text(), text);
        let mut placed = a.clone();
        let (rects, changed) = placed.place(&rig());
        assert!(!changed);
        assert_eq!(placed, a);
        let want: Vec<(Rect, f64)> = rig()
            .iter()
            .zip([1.6, 1.0, 1.0])
            .map(|(m, s)| (m.rect, s))
            .collect();
        assert_eq!(rects, want);
    }

    #[test]
    fn ac5_any_bad_line_ignores_the_file() {
        let good = format!("{UW} 0 0 1");
        for bad in [
            "x 70000 0 1".to_string(),
            "x 0 -70000 1".into(),
            "x 0 0 0.4".into(),
            "x 0 0 4.5".into(),
            "x 0 0 NaN".into(),
            "x 0 0 inf".into(),
            "x 0 0".into(),
            "x 0 0 1 1".into(),
            "my monitor 0 0 1".into(),
            format!("{UW} 10 0 1"),
            "main".into(),
            "main a b".into(),
            "".into(),
        ] {
            let text = format!("{HEADER}\n{good}\n{bad}\n");
            assert!(Arrangement::parse(&text).is_err(), "{bad:?}");
        }
        assert!(Arrangement::parse(&format!("navi-monitors 2\n{good}\n")).is_err());
        assert!(Arrangement::parse(&format!("main {UW}\nmain {UHD}\n")).is_err());
        assert!(Arrangement::parse(&format!("{HEADER}\nmain {UW}\nmain {UHD}\n")).is_err());
        assert!(Arrangement::parse(&format!("{HEADER}\n{good}\n")).is_ok());
    }

    #[test]
    fn ac5_unknown_and_overlapping_go_right_of_everything() {
        let mut a = Arrangement::parse(&format!(
            "{HEADER}\n{LAPTOP} 0 2306 1.6\n{UW} 1800 2160 1\n"
        ))
        .unwrap();
        let (rects, changed) = a.place(&rig());
        assert!(changed);
        // Right of the ultrawide's 5240, at the main (ultrawide) monitor's top.
        assert_eq!(
            rects[2],
            (
                Rect {
                    x: 5240,
                    y: 2160,
                    w: 3840,
                    h: 2160
                },
                1.0
            )
        );
        assert_eq!(
            a.entries[2],
            Entry {
                id: UHD.into(),
                x: 5240,
                y: 2160,
                scale: 1.0
            }
        );

        // The 4K's saved spot overlaps the ultrawide, which comes first in the file.
        let mut a = Arrangement::parse(&format!(
            "{HEADER}\n{LAPTOP} 0 2306 1.6\n{UW} 1800 2160 1\n{UHD} 2000 2000 1.5\n"
        ))
        .unwrap();
        let (rects, changed) = a.place(&rig());
        assert!(changed);
        assert_eq!(
            rects[2],
            (
                Rect {
                    x: 5240,
                    y: 2160,
                    w: 2560,
                    h: 1440
                },
                1.5
            )
        );
        assert_eq!(a.entries.len(), 3);
        assert_eq!((a.entries[2].x, a.entries[2].y), (5240, 2160));
    }

    #[test]
    fn ac6_kwin_import() {
        let json = include_str!("../testdata/kwinoutputconfig.json");
        let rig = rig();
        let text = import(json, &rig).unwrap();
        assert_eq!(
            text,
            format!("{HEADER}\n{LAPTOP} 0 2306 1.6\n{UW} 1800 2160 1\n{UHD} 1640 0 1\n")
        );
        let mut a = Arrangement::parse(&text).unwrap();
        let (rects, _) = a.place(&rig);
        let want: Vec<Rect> = rig.iter().map(|m| m.rect).collect();
        assert_eq!(rects.iter().map(|r| r.0).collect::<Vec<_>>(), want);

        // Laptop and ultrawide: lid open, both enabled.
        let text = import(json, &rig[..2]).unwrap();
        assert_eq!(text, format!("{HEADER}\n{LAPTOP} 0 0 1.6\n{UW} 1800 0 1\n"));

        // Ultrawide and 4K alone match only the lid-closed setup.
        let text = import(json, &rig[1..]).unwrap();
        assert_eq!(
            text,
            format!("{HEADER}\n{UW} 1800 2160 1\n{UHD} 1640 0 1\n")
        );
    }

    #[test]
    fn ac6_lid_open_wins_over_lid_closed() {
        let json = r#"[{"name":"outputs","data":[{"edidIdentifier":"A 1 2 3 4 5","scale":1},
            {"edidIdentifier":"B 1 2 3 4 5","scale":2}]},
            {"name":"setups","data":[
              {"lidClosed":true,"outputs":[{"enabled":true,"outputIndex":0,"position":{"x":9,"y":9}},
                {"enabled":true,"outputIndex":1,"position":{"x":0,"y":0}}]},
              {"lidClosed":false,"outputs":[{"enabled":true,"outputIndex":0,"position":{"x":0,"y":0}},
                {"enabled":true,"outputIndex":1,"position":{"x":100,"y":0}}]}]}]"#;
        let mons = vec![
            mon("DP-1", "A_1_2_3_4_5", (100, 100), 0, 0, 1.0),
            mon("DP-2", "B_1_2_3_4_5", (100, 100), 0, 0, 1.0),
        ];
        assert_eq!(
            import(json, &mons).unwrap(),
            format!("{HEADER}\nA_1_2_3_4_5 0 0 1\nB_1_2_3_4_5 100 0 2\n")
        );
    }

    #[test]
    fn ac6_no_match_or_damage_falls_back_to_auto() {
        let json = include_str!("../testdata/kwinoutputconfig.json");
        let rig = rig();
        let other = vec![
            rig[0].clone(),
            mon("DP-1", "NEW_1_2_3_4_5", (1920, 1080), 0, 0, 1.0),
        ];
        assert_eq!(import(json, &other), None);
        for bad in ["", "{", "[]", "{\"a\":1}", &json[..json.len() / 2]] {
            assert_eq!(import(bad, &rig), None, "{bad:?}");
        }
        let a = auto(&rig);
        assert_eq!(
            a.text(),
            format!("{HEADER}\n{LAPTOP} 0 0 1\n{UW} 2880 0 1\n{UHD} 6320 0 1\n")
        );
    }

    #[test]
    fn ac7_pointer_clamp() {
        let rects: Vec<Rect> = rig().iter().map(|m| m.rect).collect();
        // In the gap above the laptop.
        assert_eq!(clamp((1000.0, 2200.0), &rects), (1000.0, 2306.0));
        // Right of the 4K.
        assert_eq!(clamp((6000.0, 100.0), &rects), (5479.0, 100.0));
        for p in [(2000.5, 2500.25), (0.0, 2306.0), (1640.0, 0.0)] {
            assert_eq!(clamp(p, &rects), p);
        }
        // Equally near two outputs: the one first by priority.
        let two = [
            Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 10,
            },
            Rect {
                x: 20,
                y: 0,
                w: 10,
                h: 10,
            },
        ];
        assert_eq!(clamp((14.5, 5.0), &two), (9.0, 5.0));
        assert_eq!(clamp((14.5, 5.0), &[two[1], two[0]]), (20.0, 5.0));
    }

    #[test]
    fn ac7_overlay_rects_to_output_px() {
        let r = rig();
        let at = |i: usize| (r[i].rect.x, r[i].rect.y);
        assert_eq!(
            to_local(
                Rect {
                    x: 1900,
                    y: 2200,
                    w: 100,
                    h: 50
                },
                at(1),
                1.0
            ),
            Rect {
                x: 100,
                y: 40,
                w: 100,
                h: 50
            }
        );
        assert_eq!(
            to_local(
                Rect {
                    x: 100,
                    y: 2406,
                    w: 100,
                    h: 50
                },
                at(0),
                1.6
            ),
            Rect {
                x: 160,
                y: 160,
                w: 160,
                h: 80
            }
        );
        // Touching rects still touch after rounding.
        let a = to_local(
            Rect {
                x: 3,
                y: 2309,
                w: 7,
                h: 1,
            },
            at(0),
            1.6,
        );
        let b = to_local(
            Rect {
                x: 10,
                y: 2309,
                w: 7,
                h: 1,
            },
            at(0),
            1.6,
        );
        assert_eq!(a.x + a.w, b.x);
    }

    fn outputs() -> Vec<(String, Rect)> {
        rig().into_iter().map(|m| (m.id, m.rect)).collect()
    }

    #[test]
    fn ac9_bar_follows_a_300ms_hover() {
        let o = outputs();
        let mut h = BarHover::default();
        // The 4K's bottom band is y 2158..2160.
        let p = (3000.0, 2159.0);
        assert_eq!(h.feed(p, &o, UW, 1000), Hover::Arm(1300));
        assert_eq!(h.feed(p, &o, UW, 1299), Hover::Arm(1300));
        assert_eq!(h.feed(p, &o, UW, 1300), Hover::MoveTo(UHD.into()));
        assert_eq!(h.feed(p, &o, UW, 1400), Hover::Idle);
        assert_eq!(h.feed((3000.0, 2158.0), &o, UW, 2000), Hover::Idle);
    }

    #[test]
    fn ac9_leaving_restarts_and_own_band_does_nothing() {
        let o = outputs();
        let mut h = BarHover::default();
        let p = (3000.0, 2159.0);
        assert_eq!(h.feed(p, &o, UW, 0), Hover::Arm(300));
        assert_eq!(h.feed((3000.0, 1000.0), &o, UW, 200), Hover::Idle);
        assert_eq!(h.feed(p, &o, UW, 250), Hover::Arm(550));
        assert_eq!(h.feed(p, &o, UW, 549), Hover::Arm(550));
        assert_eq!(h.feed(p, &o, UW, 550), Hover::MoveTo(UHD.into()));

        // The ultrawide's own band, with the bar on it.
        let mut h = BarHover::default();
        assert_eq!(h.feed((3000.0, 3599.0), &o, UW, 0), Hover::Idle);
        assert_eq!(h.feed((3000.0, 3599.0), &o, UW, 1000), Hover::Idle);
        // 3 px above the 4K's bottom.
        assert_eq!(h.feed((3000.0, 2157.0), &o, UW, 0), Hover::Idle);
        assert_eq!(h.feed((3000.0, 2157.0), &o, UW, 1000), Hover::Idle);
        // From the 4K's band straight into the laptop's restarts the count.
        let mut h = BarHover::default();
        assert_eq!(h.feed(p, &o, UW, 0), Hover::Arm(300));
        assert_eq!(h.feed((10.0, 3430.0), &o, UW, 200), Hover::Arm(500));
        assert_eq!(
            h.feed((10.0, 3430.0), &o, UW, 500),
            Hover::MoveTo(LAPTOP.into())
        );
    }
}
