//! Frosted shell panels: each output's wallpaper, blurred on a worker once per change and saved
//! as a PNG that navi-bar paints under its panels. Never live, never window contents.

use std::{
    ffi::OsStr,
    fs::{self, DirBuilder, OpenOptions},
    io::{self, ErrorKind},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use smithay::reexports::calloop::{LoopHandle, channel};
use tracing::{info, warn};

use crate::{CalloopData, files, image::Image, locking::background, state::Navi, wallpaper};

/// What a batch blurs: each output's (name, priority, logical w, h).
pub type Outputs = Vec<(String, u32, i32, i32)>;

/// navi's private frost directory, removed when navi exits.
pub struct Dir(PathBuf);

impl Dir {
    /// `$XDG_RUNTIME_DIR/navi-frost-<pid>-<nanos>`. `None`, and frost off, without an absolute
    /// runtime dir or when the path already exists.
    pub fn create(runtime: Option<&OsStr>) -> Option<Dir> {
        let runtime = Path::new(runtime?);
        if !runtime.is_absolute() {
            warn!("frost off: XDG_RUNTIME_DIR is not absolute");
            return None;
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        sweep(runtime, |pid| {
            Path::new("/proc").join(pid.to_string()).exists()
        });
        let path = runtime.join(format!("navi-frost-{}-{nanos}", std::process::id()));
        Dir::make(path)
            .inspect_err(|e| warn!("frost off: {e}"))
            .ok()
    }

    /// A new 0700 directory; an existing path is an error.
    fn make(path: PathBuf) -> io::Result<Dir> {
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Dir(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Removes frost dirs that a navi which died uncleanly left in `runtime`: our own real
/// directories (never symlinks) named for a pid that `alive` says is gone.
fn sweep(runtime: &Path, alive: impl Fn(u32) -> bool) {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = fs::read_dir(runtime) else {
        return;
    };
    let uid = rustix::process::getuid().as_raw();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid = name
            .to_str()
            .and_then(|n| n.strip_prefix("navi-frost-"))
            .and_then(|rest| rest.split_once('-'))
            .and_then(|(pid, _)| pid.parse::<u32>().ok());
        let Some(pid) = pid.filter(|&p| !alive(p)) else {
            continue;
        };
        // `file_type` and `symlink_metadata` don't follow a symlink.
        let ours = entry
            .path()
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_dir() && m.uid() == uid);
        if ours && fs::remove_dir_all(entry.path()).is_ok() {
            info!(pid, "stale frost dir removed");
        }
    }
}

/// Writes each output's frost into `dir`, each file replaced atomically, decoding one wallpaper
/// at a time with `image(i)`. A solid or failed wallpaper, or a failed write, removes the
/// output's file, so navi-bar falls back to solid; so does an output no longer in `outputs`.
pub fn write(
    dir: &Path,
    outputs: &Outputs,
    mut image: impl FnMut(usize) -> Option<Rc<Image>>,
) -> usize {
    let mut written = 0;
    let mut keep = Vec::new();
    for (i, (name, _, w, h)) in outputs.iter().enumerate() {
        let Some(path) = files::frost_file(dir, name) else {
            warn!(
                output = name,
                "no frost: output name unusable as a file name"
            );
            continue;
        };
        match image(i).map(|image| save(&path, &image, *w, *h)) {
            Some(Ok(())) => {
                written += 1;
                keep.push(path);
                continue;
            }
            Some(Err(e)) => warn!(output = name, "frost not written: {e}"),
            None => {}
        }
        remove(&path);
    }
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "png") && !keep.contains(&path) {
            remove(&path);
        }
    }
    written
}

fn remove(path: &Path) {
    match fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => {
            warn!(path = %path.display(), "stale frost not removed: {e}")
        }
        _ => {}
    }
}

/// The blurred, cover-cropped thumbnail, undarkened: navi-bar's tint darkens it.
fn save(path: &Path, image: &Image, w: i32, h: i32) -> io::Result<()> {
    let (tw, th, pixels) = background::render(Some(image), w, h, 1.0);
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
    let encoded = (|| {
        let mut encoder = png::Encoder::new(file, tw as u32, th as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&pixels)?;
        writer.finish()
    })();
    match encoded.map_err(io::Error::other) {
        Ok(()) => fs::rename(&tmp, path),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// At most one batch at a time. Changes while one runs collapse into one follow-up batch, run
/// with the latest inputs.
#[derive(Debug)]
pub struct Jobs<T> {
    running: bool,
    next: Option<T>,
}

impl<T> Default for Jobs<T> {
    fn default() -> Self {
        Jobs {
            running: false,
            next: None,
        }
    }
}

impl<T> Jobs<T> {
    /// A change: the batch to start now, unless one runs.
    pub fn request(&mut self, input: T) -> Option<T> {
        if self.running {
            self.next = Some(input);
            return None;
        }
        self.running = true;
        Some(input)
    }

    /// The running batch finished: the follow-up to start, if any.
    pub fn finished(&mut self) -> Option<T> {
        let next = self.next.take();
        self.running = next.is_some();
        next
    }
}

#[derive(Default)]
pub struct Frost {
    dir: Option<Dir>,
    done: Option<channel::Sender<()>>,
    jobs: Jobs<Outputs>,
    /// The wallpaper epoch and outputs of the last requested batch.
    last: Option<(u64, Outputs)>,
}

impl Frost {
    fn spawn(&mut self, outputs: Outputs) {
        let (Some(dir), Some(done)) = (&self.dir, &self.done) else {
            return;
        };
        let (dir, done) = (dir.0.clone(), done.clone());
        let spawned = std::thread::Builder::new()
            .name("navi-frost".into())
            .spawn(move || {
                let start = Instant::now();
                let names: Vec<_> = outputs.iter().map(|(n, p, ..)| (n.as_str(), *p)).collect();
                let paths = wallpaper::paths(&names);
                // One decoded wallpaper at a time; outputs sharing a file reuse it.
                let mut last: Option<(&Path, Option<Rc<Image>>)> = None;
                let written = write(&dir, &outputs, |i| {
                    let path = paths[i].as_deref();
                    match &last {
                        Some((p, img)) if path == Some(*p) => img.clone(),
                        _ => {
                            last = None;
                            let img = wallpaper::decode(names[i].0, path).map(Rc::new);
                            last = path.map(|p| (p, img.clone()));
                            img
                        }
                    }
                });
                let thread = std::thread::current().name().map(str::to_string);
                let ms = start.elapsed().as_millis() as u64;
                info!(thread, outputs = outputs.len(), written, ms, "frost batch");
                let _ = done.send(());
            });
        if let Err(e) = spawned {
            warn!("frost worker not started: {e}");
            self.jobs = Jobs::default();
        }
    }
}

/// Creates the frost directory and hears finished batches. Returns the directory for navi-bar.
pub fn start(handle: &LoopHandle<'static, CalloopData>, state: &mut Navi) -> Option<PathBuf> {
    let dir = Dir::create(std::env::var_os("XDG_RUNTIME_DIR").as_deref())?;
    let (done, finished) = channel::channel();
    let heard = handle.insert_source(finished, |event, _, data| {
        if let channel::Event::Msg(()) = event {
            if let Some(c) = &data.state.bar_control {
                crate::shell::send_command(c, crate::control::Command::Frost);
            }
            let frost = &mut data.state.frost;
            if let Some(next) = frost.jobs.finished() {
                frost.spawn(next);
            }
        }
    });
    if let Err(e) = heard {
        warn!("frost off: {e}");
        return None;
    }
    let path = dir.path().to_path_buf();
    info!(dir = %path.display(), "frost");
    state.frost.dir = Some(dir);
    state.frost.done = Some(done);
    Some(path)
}

impl Navi {
    /// Each output's (name, priority, logical size), as the wallpaper is chosen for it.
    pub fn wallpaper_outputs(&self) -> Outputs {
        self.space
            .outputs()
            .filter_map(|o| {
                let geo = self.space.output_geometry(o)?;
                let priority = self.priority_of(&crate::state::identity(o));
                Some((o.name(), priority, geo.size.w, geo.size.h))
            })
            .collect()
    }

    /// Starts a frost batch when the wallpaper or the outputs changed since the last one. Called
    /// before each frame; cheap when nothing changed.
    pub fn frost_check(&mut self) {
        if self.frost.dir.is_none() {
            return;
        }
        let key = (self.wallpaper_epoch, self.wallpaper_outputs());
        if self.frost.last.as_ref() == Some(&key) {
            return;
        }
        let outputs = key.1.clone();
        self.frost.last = Some(key);
        if let Some(job) = self.frost.jobs.request(outputs) {
            self.frost.spawn(job);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::tests::{Tmp, mode};

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Image {
        Image {
            w,
            h,
            rgba: [rgb[0], rgb[1], rgb[2], 255].repeat((w * h) as usize),
        }
    }

    fn read(path: &Path) -> Image {
        crate::image::decode(&fs::read(path).unwrap(), 1024).unwrap()
    }

    #[test]
    fn ac1_frost_is_bounded_opaque_blurred_and_not_darkened() {
        // An 8192×4320 wallpaper: 8K's height at the side cap.
        let row: Vec<u8> = (0..8192)
            .flat_map(|x| match x < 4096 {
                true => [200, 40, 40, 255],
                false => [40, 40, 200, 255],
            })
            .collect();
        let image = Image {
            w: 8192,
            h: 4320,
            rgba: row.repeat(4320),
        };
        for (w, h) in [(3440, 1440), (3840, 2160), (1125, 703)] {
            let (tw, th, pixels) = background::render(Some(&image), w, h, 1.0);
            assert!(tw.max(th) <= 960, "{w}x{h}: {tw}x{th}");
            // The output's aspect ratio within one pixel.
            let want_h = tw as f64 * h as f64 / w as f64;
            assert!((th as f64 - want_h).abs() <= 1.0, "{w}x{h}: {tw}x{th}");
            assert!(pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            // Blurred: the split's middle is a blend, the ends are the wallpaper's colours.
            let px = |x: usize| &pixels[(th / 2 * tw + x) * 4..][..3];
            assert_eq!(px(0), [200, 40, 40]);
            assert_eq!(px(tw - 1), [40, 40, 200]);
            let mid = px(tw / 2);
            assert!(
                mid[0] > 60 && mid[0] < 180 && mid[2] > 60 && mid[2] < 180,
                "{mid:?}"
            );
        }
        let (_, _, pixels) =
            background::render(Some(&solid(64, 48, [90, 140, 210])), 640, 480, 1.0);
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [90, 140, 210, 255])
        );
    }

    #[test]
    fn ac2_files_are_private_atomic_and_named_safely() {
        let tmp = Tmp::new("frost-ac2");
        let dir = Dir::make(tmp.0.join("frost")).unwrap();
        assert_eq!(mode(dir.path()), 0o700);
        assert!(
            Dir::make(dir.path().to_path_buf()).is_err(),
            "an existing path is refused"
        );
        let img = Some(Rc::new(solid(32, 16, [10, 200, 30])));
        let names = ["DP-1", "../x", "a/b", "", &"a".repeat(65), &"b".repeat(64)];
        let outputs: Outputs = names.iter().map(|n| (n.to_string(), 0, 320, 160)).collect();
        let images = vec![img; names.len()];
        let listing = |p: &Path| {
            let mut l: Vec<_> = fs::read_dir(p)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            l.sort();
            l
        };
        assert_eq!(write(dir.path(), &outputs, |i| images[i].clone()), 2);
        let first = fs::read(dir.path().join("DP-1.png")).unwrap();
        assert_eq!(write(dir.path(), &outputs, |i| images[i].clone()), 2);
        assert_eq!(fs::read(dir.path().join("DP-1.png")).unwrap(), first);
        let want = vec!["DP-1.png".to_string(), format!("{}.png", "b".repeat(64))];
        assert_eq!(
            listing(dir.path()),
            want,
            "no temp files, nothing for bad names"
        );
        assert_eq!(listing(&tmp.0), ["frost"], "nothing outside the dir");
        for f in &want {
            assert_eq!(mode(&dir.path().join(f)), 0o600);
        }
        // DP-1 becomes solid: its stale file goes.
        let mut images = images;
        images[0] = None;
        write(dir.path(), &outputs, |i| images[i].clone());
        assert_eq!(listing(dir.path()), want[1..]);
        let path = dir.path().to_path_buf();
        drop(dir);
        assert!(!path.exists(), "removed on exit");
    }

    #[test]
    fn ac2_no_absolute_runtime_dir_means_no_frost() {
        assert!(Dir::create(None).is_none());
        assert!(Dir::create(Some(OsStr::new("relative-runtime"))).is_none());
        assert!(!Path::new("relative-runtime").exists());
        let tmp = Tmp::new("frost-runtime");
        let dir = Dir::create(Some(tmp.0.as_os_str())).unwrap();
        let name = dir.path().file_name().unwrap().to_str().unwrap();
        let prefix = format!("navi-frost-{}-", std::process::id());
        assert!(name.starts_with(&prefix), "{name}");
        assert_eq!(dir.path().parent(), Some(tmp.0.as_path()));
    }

    #[test]
    fn ac4_changes_during_a_batch_make_one_follow_up_with_the_last_inputs() {
        let mut jobs = Jobs::default();
        assert_eq!(jobs.request(1), Some(1));
        assert_eq!(jobs.request(2), None);
        assert_eq!(jobs.request(3), None);
        assert_eq!(jobs.request(4), None);
        assert_eq!(jobs.finished(), Some(4));
        assert_eq!(jobs.finished(), None);
        // Idle again: the next change starts at once.
        assert_eq!(jobs.request(5), Some(5));
        assert_eq!(jobs.finished(), None);
        assert_eq!(jobs.finished(), None);
    }

    #[test]
    fn ac10_each_output_gets_its_own_wallpaper_at_its_own_size() {
        let tmp = Tmp::new("frost-ac10");
        // Logical sizes of 3440×1440 at 1, 3840×2160 at 1.5 and 1800×1125 at 1.6.
        let outputs: Outputs = vec![
            ("DP-1".into(), 0, 3440, 1440),
            ("DP-2".into(), 1, 2560, 1440),
            ("eDP-1".into(), 2, 1125, 703),
        ];
        let colours = [[200, 30, 30], [30, 200, 30], [30, 30, 200]];
        let images: Vec<_> = colours
            .iter()
            .map(|&c| Some(Rc::new(solid(160, 90, c))))
            .collect();
        assert_eq!(write(&tmp.0, &outputs, |i| images[i].clone()), 3);
        for ((name, _, w, h), c) in outputs.iter().zip(colours) {
            let img = read(&tmp.0.join(format!("{name}.png")));
            assert!(img.w.max(img.h) <= 960);
            let want_h = img.w as f64 * *h as f64 / *w as f64;
            assert!(
                (img.h as f64 - want_h).abs() <= 1.0,
                "{name}: {}x{}",
                img.w,
                img.h
            );
            assert!(
                img.rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[..3] == c && p[3] == 255),
                "{name}"
            );
        }
    }

    #[test]
    fn ux4_a_removed_output_loses_its_frost() {
        let tmp = Tmp::new("frost-gone");
        let img = Some(Rc::new(solid(16, 9, [1, 2, 3])));
        let both: Outputs = vec![("A".into(), 0, 160, 90), ("B".into(), 1, 160, 90)];
        assert_eq!(write(&tmp.0, &both, |_| img.clone()), 2);
        let only_a: Outputs = vec![("A".into(), 0, 160, 90)];
        assert_eq!(write(&tmp.0, &only_a, |_| img.clone()), 1);
        assert!(tmp.0.join("A.png").exists());
        assert!(!tmp.0.join("B.png").exists(), "B's output is gone");
    }

    #[test]
    fn sec3_only_our_dead_navis_frost_dirs_are_swept() {
        let tmp = Tmp::new("frost-sweep");
        let rt = &tmp.0;
        let elsewhere = Tmp::new("frost-sweep-target");
        fs::write(elsewhere.0.join("keep"), "x").unwrap();
        let me = std::process::id();
        for d in [
            "navi-frost-111-1",
            &format!("navi-frost-{me}-1"),
            "navi-frost-x-1",
            "other",
        ] {
            fs::create_dir(rt.join(d)).unwrap();
            fs::write(rt.join(d).join("DP-1.png"), "x").unwrap();
        }
        fs::write(rt.join("navi-frost-112-1"), "a file").unwrap();
        std::os::unix::fs::symlink(&elsewhere.0, rt.join("navi-frost-113-1")).unwrap();
        // 111..=113 are dead; we are alive.
        sweep(rt, |pid| pid == me);
        let mut left: Vec<_> = fs::read_dir(rt)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        let mut want = vec![
            format!("navi-frost-{me}-1"),
            "navi-frost-112-1".into(),
            "navi-frost-113-1".into(),
            "navi-frost-x-1".into(),
            "other".into(),
        ];
        want.sort();
        assert_eq!(left, want);
        assert!(
            elsewhere.0.join("keep").exists(),
            "a symlink is never followed"
        );
    }
}
