//! File helpers shared by navi and navi-bar: XDG base dirs and capped reads.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// A new private temp file beside `path`, in a private directory.
pub fn private_tmp(path: &Path) -> io::Result<(File, PathBuf)> {
    let dir = path.parent().ok_or(ErrorKind::InvalidInput)?;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = path.with_extension("tmp");
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    Ok((file, tmp))
}

/// Atomic, private replacement shared by compositor state and shell preferences.
pub fn write_private(path: &Path, text: &str) -> io::Result<()> {
    let (mut file, tmp) = private_tmp(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::rename(tmp, path)
}

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

/// Output `name`'s frost image in `dir`, if the name is `[A-Za-z0-9_-]{1,64}`, so it can't
/// leave the directory.
pub fn frost_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let ok = (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then(|| dir.join(format!("{name}.png")))
}
