//! Per-Navi-instance GTK utilities; no global GTK/Xfce preferences are rewritten.
use std::{io, path::PathBuf};

use serde_json::Value;

use crate::{files, theme};

pub fn path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let display = std::env::var("WAYLAND_DISPLAY").ok()?;
    if !std::path::Path::new(&runtime).is_absolute()
        || display.is_empty()
        || display.len() > 128
        || !display
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
    {
        return None;
    }
    Some(PathBuf::from(runtime).join(format!("navi-{display}.utility/appearance.ini")))
}

pub fn css(settings: &Value) -> String {
    let mut css = include_str!("../assets/utility.css").to_string();
    css.push_str(include_str!("../assets/window-chrome.css"));
    // Placeholders keep a resolved color from being recolored as another source role.
    let colors = [
        theme::BG,
        theme::INK,
        theme::BAR_BG,
        theme::TAB_BG,
        theme::SHELL_EDGE,
        theme::DIM,
        theme::ACCENT,
    ];
    for (i, color) in colors.iter().enumerate() {
        css = css.replace(&format!("#{color:06x}"), &format!("NAVI_COLOR_{i}"));
    }
    for (i, color) in colors.iter().enumerate() {
        // Utilities follow Navi Settings' DIM (src/settings/extra_ui.py), not the shell's.
        let resolved = if *color == theme::DIM {
            match (
                settings["scheme"] == "light",
                settings["high_contrast"] == true,
            ) {
                (true, true) => 0x333333,
                (true, false) => 0x617084,
                (false, true) => 0xc0c7d2,
                (false, false) => *color,
            }
        } else {
            theme::resolve_for(settings, *color)
        };
        css = css.replace(&format!("NAVI_COLOR_{i}"), &format!("#{resolved:06X}"));
    }
    css.replace("14px", &format!("{}px", settings["font_size"]))
}

fn escaped(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace(' ', "\\s")
}

pub fn write(settings: &Value) -> io::Result<PathBuf> {
    let path = path().ok_or(io::ErrorKind::InvalidInput)?;
    let text = format!(
        "[Navi]\ncss={}\ndark={}\nicons={}\ncursor={}\nsize={}\n",
        escaped(&css(settings)),
        settings["scheme"] != "light",
        escaped(settings["icon_theme"].as_str().unwrap_or_default()),
        escaped(settings["cursor_theme"].as_str().unwrap_or_default()),
        settings["cursor_size"]
    );
    files::write_private(&path, &text)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn utilities_use_the_shared_palette_and_bounded_text_size() {
        let p = crate::desktop_config::parse(&serde_json::json!({"scheme":"light","accent":"purple","high_contrast":true,"font_size":24})).unwrap();
        let css = super::css(&p);
        assert!(css.contains("#FFFFFF"));
        assert!(css.contains("#7950AD"));
        assert!(css.contains("24px"));
        assert!(!css.contains("#0b0d12"));
        assert_eq!(super::escaped("a b\\c\n"), "a\\sb\\\\c\\n");
    }
}
