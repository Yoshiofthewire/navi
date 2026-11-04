//! `NAVI_SCREENSHOT=<path>`: writes navi's rendered frame to a PNG, as evidence for tests.
//! Off unless set in navi's own environment.

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use tracing::{info, warn};

pub struct Screenshot {
    path: PathBuf,
    last: Option<Instant>,
    /// The last failure, so a lasting one is logged once, not every second.
    error: Option<String>,
}

impl Screenshot {
    pub fn from_env() -> Option<Self> {
        let path = PathBuf::from(std::env::var_os("NAVI_SCREENSHOT")?);
        info!(path = %path.display(), "NAVI_SCREENSHOT set: writing a frame there every second");
        Some(Self {
            path,
            last: None,
            error: None,
        })
    }

    /// The first frame, then at most once per second, whether or not the attempt succeeds.
    pub fn due(&mut self) -> bool {
        let due = self
            .last
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        if due {
            self.last = Some(Instant::now());
        }
        due
    }

    /// Writes `rgba`, top row first. Failures are logged, never fatal.
    pub fn save(&mut self, w: u32, h: u32, rgba: &[u8]) {
        match encode(w, h, rgba).and_then(|png| write(&self.path, &png)) {
            Ok(()) => self.error = None,
            Err(e) => self.fail(e.to_string()),
        }
    }

    /// Logs a failure unless it is the same as the last one.
    pub fn fail(&mut self, e: String) {
        if self.error.as_ref() != Some(&e) {
            warn!(path = %self.path.display(), "screenshot failed: {e}");
            self.error = Some(e);
        }
    }
}

fn encode(w: u32, h: u32, rgba: &[u8]) -> io::Result<Vec<u8>> {
    let mut png = Vec::new();
    let mut enc = png::Encoder::new(&mut png, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut writer = enc.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(png)
}

/// Through a new 0600 sibling temp file and a rename: readers never see a partial image, and a
/// planted temp file or symlink makes the write fail instead of redirecting it.
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut name = path
        .file_name()
        .ok_or(io::ErrorKind::InvalidInput)?
        .to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(name);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let done = f.write_all(bytes).and_then(|()| fs::rename(&tmp, path));
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn encode_round_trips() {
        let px: Vec<u8> = (0..3 * 2 * 4).map(|i| (i * 10) as u8).collect();
        let png = encode(3, 2, &px).unwrap();
        let mut dec = png::Decoder::new(io::Cursor::new(png)).read_info().unwrap();
        let mut out = vec![0; dec.output_buffer_size().unwrap()];
        let info = dec.next_frame(&mut out).unwrap();
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(&out[..info.buffer_size()], &px[..]);
    }

    #[test]
    fn writes_0600_and_refuses_a_planted_temp() {
        let dir = std::env::temp_dir().join(format!("navi-shot-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("shot.png");
        write(&path, b"one").unwrap();
        write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let victim = dir.join("victim");
        fs::write(&victim, b"keep").unwrap();
        let tmp = dir.join(format!("shot.png.{}.tmp", std::process::id()));
        symlink(&victim, &tmp).unwrap();
        assert!(write(&path, b"three").is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"keep");
        assert_eq!(fs::read(&path).unwrap(), b"two");
        fs::remove_dir_all(&dir).unwrap();
    }
}
