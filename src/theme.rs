//! navi's one palette, shared by navi and navi-bar.
#![allow(dead_code)] // Each binary uses part of it.

/// Near-black: the empty desktop and ink on the accent.
pub const BG: u32 = 0x0B0D12;
/// Neon blue from the logo: focus, hover, gaps and the grab strip.
pub const ACCENT: u32 = 0x2BA3D8;
/// Light grey text and glyphs.
pub const INK: u32 = 0xD0D4DC;
pub const INK_ON_ACCENT: u32 = BG;
/// Secondary text and missing icons; 4.5:1 on a panel over any wallpaper.
pub const DIM: u32 = 0xA3ABBC;
/// The bar above tabbed and server-decorated tiles, and every shell panel's fill.
pub const BAR_BG: u32 = 0x151922;
/// An inactive tab, and the selected menu row.
pub const TAB_BG: u32 = 0x262C38;
pub const SELECTED: u32 = TAB_BG;
/// Settings' and the GTK utilities' subtle boundary.
pub const SHELL_EDGE: u32 = 0x303847;
/// A shell panel's 1 px rim and the dock's pinned mark: 3:1 on the panel over any wallpaper.
pub const PANEL_EDGE: u32 = 0x848C9E;
/// A minimized tile's strip or tab: a dimmed accent, still 3:1 against the bar and background.
pub const MINIMIZED: u32 = 0x20769E;
/// Outlines inactive tabs, which can't be 3:1 against both the bar and the active tab.
pub const OUTLINE: u32 = DIM;

/// Shell panels' corner radius in logical px.
pub const RADIUS: i32 = 10;
/// A floating panel's distance from the screen edge and from what it floats next to.
pub const INSET: i32 = 12;

/// Text (4.5:1) and marks (3:1) that may sit on a translucent panel, and text on each opaque
/// fill inside one.
#[cfg(test)]
pub const PANEL_TEXT: [u32; 2] = [INK, DIM];
#[cfg(test)]
pub const PANEL_MARKS: [u32; 2] = [ACCENT, PANEL_EDGE];
#[cfg(test)]
pub const ON_FILL: [(u32, &[u32]); 3] = [
    (SELECTED, &[INK, DIM]),
    (BAR_BG, &[INK, DIM, ACCENT]),
    (ACCENT, &[INK_ON_ACCENT]),
];

/// `hex` at `alpha`, premultiplied.
pub fn rgba(hex: u32, alpha: f32) -> [f32; 4] {
    let [_, r, g, b] = resolve(hex)
        .to_be_bytes()
        .map(|c| f32::from(c) / 255.0 * alpha);
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

// Packed, immutable per-frame palette choices; no file reads in painting paths.
static APPEARANCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn flags(settings: &serde_json::Value) -> u32 {
    let accent = match settings["accent"].as_str() {
        Some("green") => 1,
        Some("purple") => 2,
        Some("amber") => 3,
        Some("pink") => 4,
        _ => 0,
    };
    accent
        | (u32::from(settings["scheme"] == "light") << 4)
        | (u32::from(settings["high_contrast"] == true) << 5)
}

pub fn configure(settings: &serde_json::Value) {
    APPEARANCE.store(flags(settings), std::sync::atomic::Ordering::Relaxed);
}

pub fn resolve_for(settings: &serde_json::Value, color: u32) -> u32 {
    resolve_flags(color, flags(settings))
}

pub fn resolve(color: u32) -> u32 {
    resolve_flags(color, APPEARANCE.load(std::sync::atomic::Ordering::Relaxed))
}

/// Shell panels' opacity: translucent, opaque with high contrast.
pub fn panel_alpha() -> f32 {
    panel_alpha_flags(APPEARANCE.load(std::sync::atomic::Ordering::Relaxed))
}

fn panel_alpha_flags(flags: u32) -> f32 {
    if flags & 32 != 0 { 1.0 } else { 0.85 }
}

fn resolve_flags(color: u32, flags: u32) -> u32 {
    let light = flags & 16 != 0;
    let high = flags & 32 != 0;
    if color == ACCENT {
        return if light {
            [0x0077AA, 0x08754F, 0x7950AD, 0x906000, 0xAD3268]
        } else {
            [ACCENT, 0x4BCD96, 0xBD94F7, 0xEDB64E, 0xF28AB8]
        }[(flags & 7).min(4) as usize];
    }
    if light && high {
        return match color {
            BG | BAR_BG => 0xFFFFFF,
            INK => 0x000000,
            DIM | SHELL_EDGE | PANEL_EDGE => 0x333333,
            TAB_BG => 0xDDDDDD,
            _ => color,
        };
    }
    if light {
        match color {
            BG => 0xF5F6F8,
            BAR_BG => 0xFFFFFF,
            TAB_BG => 0xE3E7ED,
            INK => 0x202734,
            DIM => 0x525E70,
            SHELL_EDGE => 0xA0AABA,
            PANEL_EDGE => 0x6B7689,
            MINIMIZED => 0x256B8C,
            _ => color,
        }
    } else if high {
        match color {
            BG | BAR_BG => 0x000000,
            INK => 0xFFFFFF,
            DIM | SHELL_EDGE | PANEL_EDGE => 0xB8C0CE,
            _ => color,
        }
    } else {
        color
    }
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

    /// Every scheme, accent and contrast choice.
    fn all_flags() -> impl Iterator<Item = u32> {
        (0..5).flat_map(|accent| [0, 16, 32, 48].map(|f| f | accent))
    }

    /// `fg` at `alpha` over `bg`, blended in sRGB as tiny-skia and the GL renderer do.
    fn over(fg: u32, alpha: f32, bg: u32) -> u32 {
        let (f, b) = (fg.to_be_bytes(), bg.to_be_bytes());
        let c =
            |i: usize| (f32::from(f[i]) * alpha + f32::from(b[i]) * (1.0 - alpha)).round() as u32;
        c(1) << 16 | c(2) << 8 | c(3)
    }

    #[test]
    fn ac1_panel_tokens() {
        assert_eq!((RADIUS, INSET), (10, 12));
        for flags in all_flags() {
            let want = if flags & 32 != 0 { 1.0 } else { 0.85 };
            assert_eq!(panel_alpha_flags(flags), want, "flags {flags}");
        }
    }

    #[test]
    fn ac2_panels_are_readable_over_any_wallpaper() {
        for flags in all_flags() {
            let r = |c| resolve_flags(c, flags);
            for wallpaper in [0xFFFFFF, 0x000000] {
                let panel = over(r(BAR_BG), panel_alpha_flags(flags), wallpaper);
                let text = PANEL_TEXT.map(|fg| (fg, 4.5));
                for (fg, min) in text.into_iter().chain(PANEL_MARKS.map(|fg| (fg, 3.0))) {
                    let c = contrast(r(fg), panel);
                    assert!(c >= min, "flags {flags}: {fg:06x} on {panel:06x}: {c}");
                }
            }
            for (fill, texts) in ON_FILL {
                for &fg in texts {
                    let c = contrast(r(fg), r(fill));
                    assert!(c >= 4.5, "flags {flags}: {fg:06x} on {fill:06x}: {c}");
                }
            }
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
