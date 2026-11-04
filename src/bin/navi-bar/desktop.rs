//! Installed apps from `.desktop` files (Desktop Entry Spec, the parts the menu needs). The files
//! are untrusted: sizes, counts and depth are capped and a bad file is skipped.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::exec;
use crate::files::{read_bytes, xdg_dir};

const MAX_FILE: u64 = 64 << 10;
const MAX_FILES: usize = 2048;
const MAX_DEPTH: usize = 4;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The desktop file ID, e.g. `org.kde.konsole.desktop`.
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub argv: Vec<String>,
    pub wm_class: Option<String>,
    pub category: Category,
}

/// Desktop Entry Spec main categories. Each app appears in one folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Internet,
    Development,
    Office,
    Graphics,
    Media,
    Games,
    Education,
    Science,
    Settings,
    System,
    Utilities,
    #[default]
    Other,
}

impl Category {
    pub fn name(self) -> &'static str {
        match self {
            Self::Internet => "Internet",
            Self::Development => "Development",
            Self::Office => "Office",
            Self::Graphics => "Graphics",
            Self::Media => "Sound & Video",
            Self::Games => "Games",
            Self::Education => "Education",
            Self::Science => "Science",
            Self::Settings => "Settings",
            Self::System => "System",
            Self::Utilities => "Utilities",
            Self::Other => "Other",
        }
    }

    fn parse(categories: &str) -> Self {
        // A stable priority makes multi-category apps appear once, in a useful folder.
        for (key, category) in [
            ("Network", Self::Internet),
            ("Development", Self::Development),
            ("Office", Self::Office),
            ("Graphics", Self::Graphics),
            ("AudioVideo", Self::Media),
            ("Audio", Self::Media),
            ("Video", Self::Media),
            ("Game", Self::Games),
            ("Education", Self::Education),
            ("Science", Self::Science),
            ("Settings", Self::Settings),
            ("System", Self::System),
            ("Utility", Self::Utilities),
        ] {
            if categories.split(';').any(|c| c == key) {
                return category;
            }
        }
        Self::Other
    }
}

/// Match both native Wayland app IDs and apps that advertise their StartupWMClass.
pub fn entry_for(app_id: &str, entries: &[Entry]) -> Option<usize> {
    if app_id.is_empty() {
        return None;
    }
    entries
        .iter()
        .position(|e| e.id == app_id || e.id.strip_suffix(".desktop") == Some(app_id))
        .or_else(|| {
            entries.iter().position(|e| {
                e.wm_class
                    .as_ref()
                    .is_some_and(|c| c.eq_ignore_ascii_case(app_id))
            })
        })
}

/// `$XDG_DATA_HOME`, then each `$XDG_DATA_DIRS` entry, with defaults; relative values are ignored.
pub fn data_dirs(
    data_home: Option<&OsStr>,
    home: Option<&OsStr>,
    data_dirs: Option<&OsStr>,
) -> Vec<PathBuf> {
    let dirs = data_dirs
        .and_then(OsStr::to_str)
        .filter(|d| !d.is_empty())
        .unwrap_or("/usr/local/share:/usr/share");
    xdg_dir(data_home, home, ".local/share")
        .into_iter()
        .chain(dirs.split(':').map(PathBuf::from))
        .filter(|p| p.is_absolute())
        .collect()
}

/// The apps to show from `<dir>/applications` of each data dir; the first file per ID wins.
/// Sorted by name, case-insensitively, then ID.
pub fn scan(data_dirs: &[PathBuf]) -> Vec<Entry> {
    let mut files = Vec::new();
    for d in data_dirs {
        let apps = d.join("applications");
        walk(&apps, &apps, 0, &mut files);
    }
    let mut seen = HashSet::new();
    let mut out: Vec<Entry> = files
        .into_iter()
        .filter(|(id, _)| seen.insert(id.clone()))
        .filter_map(|(id, path)| {
            let bytes = read_bytes(&path, MAX_FILE).ok()??;
            parse(id, &String::from_utf8(bytes).ok()?)
        })
        .collect();
    out.sort_by(|a, b| {
        let key = |e: &Entry| e.name.to_lowercase();
        key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// Collects (desktop file ID, path) under `dir`, in name order, without following directory
/// symlinks.
fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = read.filter_map(Result::ok).collect();
    items.sort_by_key(|e| e.file_name());
    for e in items {
        if out.len() >= MAX_FILES {
            return;
        }
        let path = e.path();
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        if is_dir && depth < MAX_DEPTH {
            walk(root, &path, depth + 1, out);
        } else if !is_dir && path.extension() == Some(OsStr::new("desktop")) {
            let id = path.strip_prefix(root).ok().and_then(Path::to_str);
            if let Some(id) = id {
                out.push((id.replace('/', "-"), path.clone()));
            }
        }
    }
}

/// The spec's string escapes; others are left as they are.
pub fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(o) => out.extend(['\\', o]),
            None => out.push('\\'),
        }
    }
    out
}

/// The entry, if it's an app to show: `[Desktop Entry]` keys only, the first of each key wins.
fn parse(id: String, text: &str) -> Option<Entry> {
    let mut in_entry = false;
    let mut keys: Vec<(&str, &str)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry && let Some((k, v)) = line.split_once('=') {
            keys.push((k.trim(), v.trim()));
        }
    }
    let get = |key: &str| {
        keys.iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| unescape(v))
    };
    let yes = |key: &str| get(key).as_deref() == Some("true");
    let has_navi = |key: &str| get(key).is_some_and(|v| v.split(';').any(|d| d == "navi"));
    let shown = get("Type").as_deref() == Some("Application")
        && !yes("NoDisplay")
        && !yes("Hidden")
        && !yes("Terminal")
        && (get("OnlyShowIn").is_none() || has_navi("OnlyShowIn"))
        && !has_navi("NotShowIn");
    if !shown {
        return None;
    }
    Some(Entry {
        id,
        name: get("Name").filter(|n| !n.is_empty())?,
        icon: get("Icon").filter(|i| !i.is_empty()),
        argv: exec::split(&get("Exec")?).ok()?,
        wm_class: get("StartupWMClass").filter(|c| !c.is_empty()),
        category: Category::parse(&get("Categories").unwrap_or_default()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Tmp;

    fn app(name: &str, extra: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec=run\n{extra}")
    }

    #[test]
    fn ac2_scans_like_the_spec() {
        let tmp = Tmp::new("desktop");
        let (home, sys) = (tmp.0.join("home"), tmp.0.join("sys"));
        let put = |dir: &Path, rel: &str, body: &[u8]| {
            let p = dir.join("applications").join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        };
        put(&sys, "kde/a.desktop", app("Alpha", "Icon=alpha").as_bytes());
        put(&sys, "shadowed.desktop", app("System copy", "").as_bytes());
        put(&home, "shadowed.desktop", app("User copy", "").as_bytes());
        for (file, extra) in [
            ("nodisplay", "NoDisplay=true"),
            ("hidden", "Hidden=true"),
            ("terminal", "Terminal=true"),
            ("onlykde", "OnlyShowIn=KDE;"),
            ("notnavi", "NotShowIn=GNOME;navi;"),
        ] {
            put(
                &sys,
                &format!("{file}.desktop"),
                app(file, extra).as_bytes(),
            );
        }
        put(
            &sys,
            "link.desktop",
            b"[Desktop Entry]\nType=Link\nName=Link\nExec=x\n",
        );
        put(
            &sys,
            "noexec.desktop",
            b"[Desktop Entry]\nType=Application\nName=NoExec\n",
        );
        put(
            &sys,
            "badexec.desktop",
            app("x", "").replace("run", "a;b").as_bytes(),
        );
        put(
            &sys,
            "onlynavi.desktop",
            app("beta", "OnlyShowIn=KDE;navi;").as_bytes(),
        );
        put(&sys, "escaped.desktop", app(r"Gamma\sRay", "").as_bytes());
        let big = app("Big", &"#".repeat(65 << 10));
        put(&sys, "big.desktop", big.as_bytes());
        put(
            &sys,
            "latin1.desktop",
            b"[Desktop Entry]\nType=Application\nName=\xe9\nExec=x\n",
        );
        put(&sys, "garbage.desktop", &[0, 159, 146, 150, 255, 0, 1]);
        put(
            &sys,
            "groups.desktop",
            b"[Other]\nType=Application\nName=Wrong\nExec=wrong\n[Desktop Entry]\n\
              Type=Application\nName=delta\nExec=right %U\n[Desktop Action x]\nName=Nope\n",
        );
        put(
            &sys,
            "deep/1/2/3/4/too-deep.desktop",
            app("Deep", "").as_bytes(),
        );
        put(
            &sys,
            "deep/1/2/3/ok.desktop",
            app("Deep enough", "").as_bytes(),
        );

        let dirs = data_dirs(
            Some(home.as_os_str()),
            None,
            Some(OsStr::new(&format!("relative:{}", sys.display()))),
        );
        assert_eq!(dirs, [home.clone(), sys.clone()]);
        let got = scan(&dirs);
        let names: Vec<(&str, &str)> = got
            .iter()
            .map(|e| (e.name.as_str(), e.id.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("Alpha", "kde-a.desktop"),
                ("beta", "onlynavi.desktop"),
                ("Deep enough", "deep-1-2-3-ok.desktop"),
                ("delta", "groups.desktop"),
                ("Gamma Ray", "escaped.desktop"),
                ("User copy", "shadowed.desktop"),
            ]
        );
        assert_eq!(got[0].icon.as_deref(), Some("alpha"));
        assert_eq!(got[3].argv, ["right"]);
    }

    #[test]
    fn data_dirs_defaults() {
        let os = |s: &'static str| Some(OsStr::new(s));
        assert_eq!(
            data_dirs(None, os("/home/u"), None),
            [
                PathBuf::from("/home/u/.local/share"),
                "/usr/local/share".into(),
                "/usr/share".into()
            ]
        );
        assert_eq!(
            data_dirs(os("rel"), None, os("")),
            [PathBuf::from("/usr/local/share"), "/usr/share".into()]
        );
    }

    #[test]
    fn desktop_categories_assign_one_stable_folder_and_match_app_ids() {
        for (keys, category) in [
            ("Network;WebBrowser;", Category::Internet),
            ("Qt;KDE;Development;Utility;", Category::Development),
            ("Video;Player;", Category::Media),
            ("Game;", Category::Games),
            ("Utility;TextEditor;", Category::Utilities),
            ("NotNetwork;", Category::Other),
            ("", Category::Other),
        ] {
            let text = app(
                "Example",
                &format!("Categories={keys}\nStartupWMClass=Example"),
            );
            let entry = parse("org.example.App.desktop".into(), &text).unwrap();
            assert_eq!(entry.category, category);
            for id in ["org.example.App", "org.example.App.desktop", "example"] {
                assert_eq!(entry_for(id, std::slice::from_ref(&entry)), Some(0));
            }
            assert_eq!(entry_for("", &[entry]), None);
        }
    }
}
