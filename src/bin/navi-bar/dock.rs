//! Persistent desktop IDs and one dock slot per application, pinned apps first.

use crate::{
    desktop::{self, Entry},
    files::{read_bytes, write_private, xdg_dir},
};
use std::path::PathBuf;

const MAX_PINS: usize = 32;
const HEADER: &str = "navi-pins 1";

pub struct Pins {
    pub ids: Vec<String>,
    path: Option<PathBuf>,
    error: Option<String>,
}

fn parse(text: &str) -> Result<Vec<String>, String> {
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Err("unrecognized pin file".into());
    }
    let mut ids = Vec::new();
    for id in lines {
        if id.is_empty()
            || id.len() > 512
            || !id.ends_with(".desktop")
            || id.chars().any(|c| c.is_control() || c == '/' || c == '\\')
        {
            return Err("invalid desktop ID in pin file".into());
        }
        if !ids.iter().any(|saved| saved == id) {
            ids.push(id.to_string());
        }
        if ids.len() > MAX_PINS {
            return Err("too many pinned apps".into());
        }
    }
    Ok(ids)
}

impl Pins {
    pub fn load() -> Self {
        let var = std::env::var_os;
        let path = xdg_dir(
            var("XDG_CONFIG_HOME").as_deref(),
            var("HOME").as_deref(),
            ".config",
        )
        .map(|p| p.join("navi/pins"));
        Self::open(path)
    }

    fn open(path: Option<PathBuf>) -> Self {
        let loaded = path.as_ref().map_or(Ok(Vec::new()), |p| {
            read_bytes(p, 32 << 10)?.map_or(Ok(Vec::new()), |b| {
                parse(std::str::from_utf8(&b).map_err(|_| "pin file is not UTF-8")?)
            })
        });
        let (ids, error) = match loaded {
            Ok(ids) => (ids, None),
            Err(e) => {
                tracing::warn!("pins not loaded: {e}");
                (Vec::new(), Some(e))
            }
        };
        Self { ids, path, error }
    }

    /// Save before changing the display: a failed save never pretends the app is pinned.
    pub fn toggle(&mut self, id: &str) -> Result<bool, String> {
        if let Some(e) = &self.error {
            return Err(format!("Pin file needs repair: {e}"));
        }
        let path = self.path.as_ref().ok_or("No config directory for pins")?;
        let mut ids = self.ids.clone();
        let pinned = if let Some(i) = ids.iter().position(|saved| saved == id) {
            ids.remove(i);
            false
        } else {
            if ids.len() >= MAX_PINS {
                return Err("The dock holds up to 32 pinned apps".into());
            }
            ids.push(id.into());
            true
        };
        let text = format!("{HEADER}\n{}\n", ids.join("\n"));
        // No blank line when removing the last pin.
        let text = if ids.is_empty() {
            format!("{HEADER}\n")
        } else {
            text
        };
        parse(&text)?;
        write_private(path, &text).map_err(|e| format!("Could not save pins: {e}"))?;
        self.ids = ids;
        Ok(pinned)
    }
}

pub struct Running<'a> {
    pub app_id: &'a str,
    pub focused: bool,
    pub minimized: bool,
}

#[derive(Debug, PartialEq)]
pub struct App {
    pub entry: Option<usize>,
    pub app_id: String,
    pub windows: Vec<usize>,
    pub focused: bool,
    pub minimized: bool,
    pub pinned: bool,
}

pub fn apps(pins: &[String], entries: &[Entry], windows: &[Running<'_>]) -> Vec<App> {
    let mut apps: Vec<App> = pins
        .iter()
        .filter_map(|id| {
            let entry = entries.iter().position(|e| &e.id == id)?;
            Some(App {
                entry: Some(entry),
                app_id: id.clone(),
                windows: Vec::new(),
                focused: false,
                minimized: false,
                pinned: true,
            })
        })
        .collect();
    for (i, win) in windows.iter().enumerate() {
        let entry = desktop::entry_for(win.app_id, entries);
        let slot = apps.iter().position(|app| match entry {
            Some(e) => app.entry == Some(e),
            None => !win.app_id.is_empty() && app.app_id == win.app_id,
        });
        let at = slot.unwrap_or_else(|| {
            apps.push(App {
                entry,
                app_id: win.app_id.into(),
                windows: Vec::new(),
                focused: false,
                minimized: true,
                pinned: false,
            });
            apps.len() - 1
        });
        let app = &mut apps[at];
        if app.windows.is_empty() {
            app.minimized = true;
        }
        app.windows.push(i);
        app.focused |= win.focused;
        app.minimized &= win.minimized;
    }
    apps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{desktop::Category, testutil::Tmp};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn pins_survive_restarts_unpin_and_refuse_bad_files() {
        let tmp = Tmp::new("pins");
        let path = tmp.0.join("navi/pins");
        let mut pins = Pins::open(Some(path.clone()));
        assert_eq!(pins.toggle("org.kde.konsole.desktop"), Ok(true));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut pins = Pins::open(Some(path.clone()));
        assert!(pins.ids.iter().any(|id| id == "org.kde.konsole.desktop"));
        assert_eq!(pins.toggle("org.kde.konsole.desktop"), Ok(false));
        assert!(Pins::open(Some(path.clone())).ids.is_empty());
        std::fs::write(&path, "broken\n").unwrap();
        let mut pins = Pins::open(Some(path.clone()));
        assert!(pins.toggle("app.desktop").is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "broken\n");
        for bad in [
            "navi-pins 2",
            "navi-pins 1\n../x.desktop",
            "navi-pins 1\nx",
            "navi-pins 1\nx\t.desktop",
        ] {
            assert!(parse(bad).is_err());
        }
        assert_eq!(
            parse("navi-pins 1\na.desktop\na.desktop\n").unwrap(),
            ["a.desktop"]
        );
    }

    #[test]
    fn pinned_apps_group_running_windows_without_duplicates() {
        let entries = [Entry {
            id: "terminal.desktop".into(),
            name: "Terminal".into(),
            icon: None,
            argv: vec!["terminal".into()],
            wm_class: Some("Term".into()),
            category: Category::System,
        }];
        let windows = [
            Running {
                app_id: "term",
                focused: false,
                minimized: true,
            },
            Running {
                app_id: "terminal",
                focused: true,
                minimized: false,
            },
            Running {
                app_id: "unknown",
                focused: false,
                minimized: false,
            },
        ];
        let apps = apps(
            &["terminal.desktop".into(), "removed.desktop".into()],
            &entries,
            &windows,
        );
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].windows, [0, 1]);
        assert!(apps[0].pinned && apps[0].focused && !apps[0].minimized);
        assert_eq!(apps[1].windows, [2]);
        assert!(!apps[1].pinned);
        assert!(
            super::apps(&["terminal.desktop".into()], &entries, &[])[0]
                .windows
                .is_empty()
        );
    }
}
