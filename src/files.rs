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

/// The file's bytes, at most `max`; `None` if there is no file. Judges the file it opened, not
/// the path, and never blocks opening a FIFO swapped in by another process.
pub fn read_bytes(path: &Path, max: u64) -> Result<Option<Vec<u8>>, String> {
    // Never wait for a FIFO's writer, and never take a tty as the controlling terminal.
    let flags = (rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::NOCTTY).bits() as i32;
    let file = match OpenOptions::new().read(true).custom_flags(flags).open(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(f) => f,
    };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err(format!("over {max} bytes"));
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering::Relaxed, mpsc};
    use std::time::Duration;

    /// A fresh empty dir, removed on drop.
    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn read_bytes_caps_and_refuses_what_is_not_a_regular_file() {
        let tmp = Tmp(std::env::temp_dir().join(format!("navi-files-{}-caps", std::process::id())));
        let _ = fs::remove_dir_all(&tmp.0);
        fs::create_dir(&tmp.0).unwrap();
        let dir = &tmp.0;
        fs::write(dir.join("f"), b"hello").unwrap();
        assert_eq!(read_bytes(&dir.join("f"), 5), Ok(Some(b"hello".to_vec())));
        assert!(read_bytes(&dir.join("f"), 4).is_err(), "over the cap");
        assert_eq!(read_bytes(&dir.join("missing"), 5), Ok(None));
        symlink("missing", dir.join("dangling")).unwrap();
        assert_eq!(read_bytes(&dir.join("dangling"), 5), Ok(None));
        symlink("f", dir.join("link")).unwrap();
        assert_eq!(
            read_bytes(&dir.join("link"), 5),
            Ok(Some(b"hello".to_vec()))
        );
        assert!(read_bytes(dir, 5).is_err(), "a directory");
        // navi-files reads another process's environ: procfs files are regular, size 0.
        let environ = read_bytes(Path::new("/proc/self/environ"), 1 << 20)
            .unwrap()
            .unwrap();
        assert!(!environ.is_empty());
    }

    #[test]
    fn read_bytes_refuses_devices() {
        for dev in [
            "/dev/zero",
            "/dev/null",
            "/dev/urandom",
            "/dev/full",
            "/dev/tty",
        ] {
            assert!(read_bytes(Path::new(dev), 64).is_err(), "{dev}");
        }
    }

    /// A tty swapped in for a path must never become the reader's controlling terminal
    /// (`O_NOCTTY`). Runs itself again as a session leader with no ctty, aimed at a pty slave.
    #[test]
    fn read_bytes_never_takes_a_controlling_terminal() {
        const SLAVE: &str = "NAVI_TEST_NOCTTY_SLAVE";
        const MARKER: &str = "noctty child checked";
        let no_ctty = || File::open("/dev/tty").is_err();
        if let Some(slave) = std::env::var_os(SLAVE) {
            rustix::process::setsid().expect("setsid");
            assert!(no_ctty(), "child has a ctty before read_bytes");
            assert!(
                read_bytes(Path::new(&slave), 64).is_err(),
                "a tty is refused"
            );
            assert!(
                no_ctty(),
                "read_bytes made the tty our controlling terminal"
            );
            println!("{MARKER}");
            return;
        }
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        let master =
            openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).expect("openpt");
        grantpt(&master).expect("grantpt");
        unlockpt(&master).expect("unlockpt");
        let slave = ptsname(&master, Vec::new()).expect("ptsname");
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "files::tests::read_bytes_never_takes_a_controlling_terminal",
            ])
            .args(["--nocapture", "--test-threads=1"])
            .env(SLAVE, slave.to_str().unwrap())
            .output()
            .expect("spawn the test binary");
        drop(master);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let report = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "child failed:\n{report}");
        assert!(
            stdout.contains(MARKER),
            "child skipped the check:\n{report}"
        );
    }

    /// A FIFO swapped in for the file between any check and the open must not block the
    /// caller (navi-bar's main thread reads icons, fonts and `.desktop` files this way).
    #[test]
    fn read_bytes_never_blocks_on_a_swapped_in_fifo() {
        let tmp = Tmp(std::env::temp_dir().join(format!("navi-files-{}-fifo", std::process::id())));
        let _ = fs::remove_dir_all(&tmp.0);
        fs::create_dir(&tmp.0).unwrap();
        let dir = tmp.0.clone();
        fs::write(dir.join("reg"), b"bytes").unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(dir.join("fifo"))
            .status()
            .unwrap();
        assert!(made.success());
        assert!(
            read_bytes(&dir.join("fifo"), 64).is_err(),
            "a FIFO is refused"
        );
        symlink("reg", dir.join("x")).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (stopped, d) = (stop.clone(), dir.clone());
        // Atomically flips x between the regular file and the FIFO.
        let swapper = std::thread::spawn(move || {
            for i in 0u64.. {
                if stopped.load(Relaxed) {
                    break;
                }
                let _ = fs::remove_file(d.join("n"));
                symlink(if i % 2 == 0 { "fifo" } else { "reg" }, d.join("n")).unwrap();
                fs::rename(d.join("n"), d.join("x")).unwrap();
            }
        });
        let (done, finished) = mpsc::channel();
        let d = dir.clone();
        std::thread::spawn(move || {
            for _ in 0..20_000 {
                let _ = read_bytes(&d.join("x"), 64);
            }
            let _ = done.send(());
        });
        let result = finished.recv_timeout(Duration::from_secs(30));
        if result.is_err() {
            // Unblock the stuck open so the test process can end.
            let _ = OpenOptions::new().write(true).open(dir.join("fifo"));
        }
        stop.store(true, Relaxed);
        swapper.join().unwrap();
        assert!(result.is_ok(), "read_bytes blocked opening a FIFO");
    }
}
