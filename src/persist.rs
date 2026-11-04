//! Keeps the layout's memory in `$XDG_STATE_HOME/navi/memory` across restarts. Private (dir 0700,
//! file 0600), replaced atomically, and a bad file is ignored, never fatal. The file helpers here
//! serve navi's other files too.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::layout::Layout;

const MAX_BYTES: u64 = 1 << 20;

/// An XDG base dir: `var` (e.g. `$XDG_STATE_HOME`), else `$HOME/<fallback>`. Relative values are
/// ignored, as the XDG spec says, so navi never writes relative to its working directory.
pub fn xdg_dir(var: Option<&OsStr>, home: Option<&OsStr>, fallback: &str) -> Option<PathBuf> {
    fn abs(v: Option<&OsStr>) -> Option<&Path> {
        v.map(Path::new).filter(|p| p.is_absolute())
    }
    match abs(var) {
        Some(s) => Some(s.to_path_buf()),
        None => Some(abs(home)?.join(fallback)),
    }
}

fn path(state: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    Some(xdg_dir(state, home, ".local/state")?.join("navi/memory"))
}

/// The file's bytes, at most `max`; `None` if there is no file.
pub fn read_bytes(path: &Path, max: u64) -> Result<Option<Vec<u8>>, String> {
    match fs::metadata(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(m) if !m.is_file() => return Err("not a regular file".into()),
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|f| f.take(max + 1).read_to_end(&mut bytes))
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err(format!("over {max} bytes"));
    }
    Ok(Some(bytes))
}

/// The file's text, at most 1 MiB; `None` if there is no file.
pub fn read(path: &Path) -> Result<Option<String>, String> {
    match read_bytes(path, MAX_BYTES)? {
        None => Ok(None),
        Some(b) => String::from_utf8(b)
            .map(Some)
            .map_err(|_| "not UTF-8".into()),
    }
}

/// Writes a private temp file and renames it over `path`, so a crash leaves the old file or the
/// new one. `create_new` never follows a symlink planted at the temp path.
pub fn write(path: &Path, text: &str) -> io::Result<()> {
    let dir = path.parent().ok_or(ErrorKind::InvalidInput)?;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = path.with_extension("tmp");
    // A killed run's leftover.
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

fn load(path: &Path, layout: &mut Layout) {
    let shown = path.display();
    let loaded = match read(path) {
        Ok(None) => return,
        Ok(Some(text)) => layout.load_memory(&text).map_err(String::from),
        Err(e) => Err(e),
    };
    match loaded {
        Ok((entries, skipped)) => {
            if skipped > 0 {
                warn!(path = %shown, skipped, "memory: bad lines skipped");
            }
            info!(path = %shown, entries, skipped, "memory loaded");
        }
        Err(e) => warn!(path = %shown, "memory file ignored: {e}"),
    }
}

/// Where memory is saved, and the text last saved there.
pub struct Store {
    path: Option<PathBuf>,
    saved: String,
}

impl Store {
    /// Finds the file and loads it into `layout`. What it loaded counts as saved.
    pub fn open(layout: &mut Layout) -> Store {
        let var = std::env::var_os;
        let path = path(var("XDG_STATE_HOME").as_deref(), var("HOME").as_deref());
        match &path {
            Some(p) => load(p, layout),
            None => warn!("no absolute XDG_STATE_HOME or HOME: memory is not saved"),
        }
        Store {
            path,
            saved: layout.memory_text(),
        }
    }

    /// Saves `layout`'s memory if it changed since the last save.
    pub fn sync(&mut self, layout: &Layout) {
        let Some(path) = &self.path else {
            return;
        };
        let text = layout.memory_text();
        if text == self.saved {
            return;
        }
        match write(path, &text) {
            Ok(()) => {
                let entries = text.lines().count() - 1;
                info!(path = %path.display(), entries, "memory saved");
                self.saved = text;
            }
            Err(e) => warn!(path = %path.display(), "memory not saved: {e}"),
        }
    }
}

#[cfg(test)]
pub mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;
    use crate::layout::{Output, Rect};

    /// A fresh empty dir, removed on drop.
    pub struct Tmp(pub PathBuf);

    impl Tmp {
        pub fn new(name: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!("navi-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir(&p).unwrap();
            Tmp(p)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn remembering() -> Layout {
        let mut l = Layout::default();
        l.set_outputs(vec![Output {
            name: "uw".into(),
            area: Rect {
                x: 0,
                y: 0,
                w: 3440,
                h: 1440,
            },
            priority: 0,
        }]);
        l.place("a", (10, 10)).unwrap();
        l.place("b", (3400, 10)).unwrap();
        l
    }

    const EMPTY: &str = "navi-memory 1\n";

    #[test]
    fn path_follows_xdg_and_rejects_relative() {
        let os = |s: &'static str| Some(OsStr::new(s));
        let home = Some(PathBuf::from("/home/u/.local/state/navi/memory"));
        assert_eq!(
            path(os("/x"), os("/home/u")),
            Some(PathBuf::from("/x/navi/memory"))
        );
        assert_eq!(path(None, os("/home/u")), home);
        assert_eq!(path(os(""), os("/home/u")), home);
        assert_eq!(path(os("state"), os("/home/u")), home);
        assert_eq!(path(None, None), None);
        assert_eq!(path(os("state"), os("home/u")), None);
        assert_eq!(path(os(""), os("")), None);
    }

    #[test]
    fn save_is_private_atomic_and_loads_back() {
        let tmp = Tmp::new("save");
        let file = tmp.0.join("navi/memory");
        let l = remembering();

        write(&file, &l.memory_text()).unwrap();
        assert_eq!(mode(&tmp.0.join("navi")), 0o700);
        assert_eq!(mode(&file), 0o600);

        // A looser file from elsewhere, and a dangling symlink left at the temp path.
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        let stale = file.with_extension("tmp");
        symlink(tmp.0.join("nowhere"), &stale).unwrap();
        write(&file, &l.memory_text()).unwrap();
        assert_eq!(mode(&file), 0o600);
        assert!(fs::symlink_metadata(&stale).is_err());
        assert!(!tmp.0.join("nowhere").exists());

        let mut back = Layout::default();
        load(&file, &mut back);
        assert_eq!(back.memory_text(), l.memory_text());
        assert_ne!(back.memory_text(), EMPTY);
    }

    #[test]
    fn bad_files_load_as_empty() {
        let tmp = Tmp::new("bad");
        let file = tmp.0.join("memory");
        let check = |what: &str| {
            let mut l = Layout::default();
            load(&file, &mut l);
            assert_eq!(l.memory_text(), EMPTY, "{what}");
        };
        check("missing");
        fs::create_dir(&file).unwrap();
        check("directory");
        fs::remove_dir(&file).unwrap();
        let big = format!("{EMPTY}{}", "a uw 0 0 6 6 1720 1440\n".repeat(50_000));
        fs::write(&file, big).unwrap();
        check("over 1 MiB");
        fs::write(&file, b"navi-memory 1\n\xff\xfe uw 0 0 6 6 1720 1440\n").unwrap();
        check("not UTF-8");
    }

    #[test]
    fn save_under_a_file_fails_cleanly() {
        let tmp = Tmp::new("parent");
        let blocker = tmp.0.join("navi");
        fs::write(&blocker, "").unwrap();
        assert!(write(&blocker.join("memory"), EMPTY).is_err());

        let mut store = Store {
            path: Some(blocker.join("memory")),
            saved: String::new(),
        };
        store.sync(&remembering());
        assert_eq!(store.saved, "");
    }
}
