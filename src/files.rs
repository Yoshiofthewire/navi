//! File helpers shared by navi and navi-bar: XDG base dirs and capped reads.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

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
