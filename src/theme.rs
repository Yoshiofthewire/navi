//! navi's one palette, shared by navi and navi-bar.
#![allow(dead_code)] // Each binary uses part of it.

/// Near-black: the empty desktop, the taskbar, menu and cards, and ink on the accent.
pub const BG: u32 = 0x0B0D12;
/// Neon blue from the logo: focus, hover, gaps and the grab strip.
pub const ACCENT: u32 = 0x2BA3D8;
/// Light grey text and glyphs.
pub const INK: u32 = 0xD0D4DC;
pub const INK_ON_ACCENT: u32 = BG;
/// Secondary text and missing icons.
pub const DIM: u32 = 0x7A8396;
/// The bar above tabbed and server-decorated tiles.
pub const BAR_BG: u32 = 0x151922;
/// An inactive tab, and the selected menu row.
pub const TAB_BG: u32 = 0x262C38;
pub const SELECTED: u32 = TAB_BG;
/// The subtle boundary of the dock, launcher and top status bar.
pub const SHELL_EDGE: u32 = 0x303847;
/// A minimized tile's strip or tab: a dimmed accent, still 3:1 against the bar and background.
pub const MINIMIZED: u32 = 0x20769E;
/// Outlines inactive tabs, which can't be 3:1 against both the bar and the active tab.
pub const OUTLINE: u32 = DIM;

/// `hex` at `alpha`, premultiplied.
pub fn rgba(hex: u32, alpha: f32) -> [f32; 4] {
    let [_, r, g, b] = hex.to_be_bytes().map(|c| f32::from(c) / 255.0 * alpha);
    [r, g, b, alpha]
}

/// The WCAG 2.x contrast ratio of two colours.
#[cfg(test)]
pub fn contrast(a: u32, b: u32) -> f32 {
    fn luminance(hex: u32) -> f32 {
        let [_, r, g, b] = hex.to_be_bytes().map(|c| {
            let c = f32::from(c) / 255.0;
            match c <= 0.04045 {
                true => c / 12.92,
                false => ((c + 0.055) / 1.055).powf(2.4),
            }
        });
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac1_text_is_4_5_to_1_and_ui_is_3_to_1() {
        assert!((contrast(0xFFFFFF, 0x000000) - 21.0).abs() < 0.01);
        for (fg, bg) in [
            (INK, BG),
            (INK, BAR_BG),
            (INK, TAB_BG),
            (DIM, BG),
            (DIM, BAR_BG),
            (INK_ON_ACCENT, ACCENT),
        ] {
            assert!(contrast(fg, bg) >= 4.5, "{fg:06x} on {bg:06x}");
        }
        // WCAG 1.4.11: what sits on the bar and the empty background, and inactive tab outlines.
        for (fg, bg) in [
            (ACCENT, BG),
            (ACCENT, BAR_BG),
            (ACCENT, TAB_BG),
            (MINIMIZED, BG),
            (MINIMIZED, BAR_BG),
            (OUTLINE, BAR_BG),
            (OUTLINE, TAB_BG),
        ] {
            assert!(contrast(fg, bg) >= 3.0, "{fg:06x} on {bg:06x}");
        }
    }

    #[test]
    fn rgba_premultiplies() {
        assert_eq!(rgba(0xFF0000, 0.5), [0.5, 0.0, 0.0, 0.5]);
        assert_eq!(
            rgba(ACCENT, 1.0),
            [
                0x2B as f32 / 255.0,
                0xA3 as f32 / 255.0,
                0xD8 as f32 / 255.0,
                1.0
            ]
        );
    }
}
