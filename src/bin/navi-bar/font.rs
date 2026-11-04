//! The menu's font: the first of a few distro paths that parses. No fontconfig, no shaping.

use std::path::Path;

use crate::files::read_bytes;

const MAX_FILE: u64 = 32 << 20;
/// Noto Sans, DejaVu Sans, Liberation Sans, in Arch and Debian layouts.
const PATHS: &[&str] = &[
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
];

pub fn load() -> Option<fontdue::Font> {
    PATHS.iter().find_map(|p| {
        let bytes = read_bytes(Path::new(p), MAX_FILE).ok()??;
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).ok()?;
        tracing::info!(path = p, "font");
        Some(font)
    })
}

/// The chars of `text` that fit in `max` px, with `…` in place of the rest. `advance` is a
/// glyph's width, `None` for a glyph the font lacks, which is skipped.
pub fn fit(text: &str, max: f32, advance: impl Fn(char) -> Option<f32>) -> Vec<char> {
    let chars: Vec<(char, f32)> = text
        .chars()
        .filter(|c| !c.is_control())
        .filter_map(|c| Some((c, advance(c)?)))
        .collect();
    if chars.iter().map(|c| c.1).sum::<f32>() <= max {
        return chars.into_iter().map(|c| c.0).collect();
    }
    let room = max - advance('…').unwrap_or(0.0);
    let mut used = 0.0;
    let mut out: Vec<char> = chars
        .into_iter()
        .take_while(|&(_, w)| {
            used += w;
            used <= room
        })
        .map(|c| c.0)
        .collect();
    if advance('…').is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_with_an_ellipsis_and_skips_missing_glyphs() {
        let adv = |c: char| (c != '☃').then_some(10.0);
        let s = |v: Vec<char>| v.into_iter().collect::<String>();
        assert_eq!(s(fit("Konsole", 70.0, adv)), "Konsole");
        assert_eq!(s(fit("Konsole", 69.0, adv)), "Konso…");
        assert_eq!(s(fit("Konsole", 59.0, adv)), "Kons…");
        assert_eq!(s(fit("K☃onsole", 70.0, adv)), "Konsole");
        assert_eq!(s(fit("Konsole", 5.0, adv)), "…");
    }
}
