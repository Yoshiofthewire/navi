//! Starts navi-bar, navi's own shell client, over a socket only it holds, and starts it again
//! when it exits: after 1 s, at most `MAX_STARTS` times in any `WINDOW_MS`.

use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::{OsStr, OsString},
    io,
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::PathBuf,
    process::{Command, Stdio},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use smithay::reexports::{
    calloop::{
        LoopHandle,
        channel::{self, Event},
        timer::{TimeoutAction, Timer},
    },
    wayland_server::DisplayHandle,
};
use tracing::{info, warn};

use crate::{CalloopData, state::ClientState};

const MAX_STARTS: usize = 5;
const WINDOW_MS: u64 = 60_000;
const RESTART_DELAY: Duration = Duration::from_secs(1);

/// At most `MAX_STARTS` starts in any `WINDOW_MS`.
#[derive(Debug, Default)]
pub struct Respawn {
    starts: VecDeque<u64>,
}

impl Respawn {
    /// Whether a start at `now_ms` is allowed; if so, it's counted.
    pub fn allow(&mut self, now_ms: u64) -> bool {
        while self
            .starts
            .front()
            .is_some_and(|&t| now_ms.saturating_sub(t) >= WINDOW_MS)
        {
            self.starts.pop_front();
        }
        let ok = self.starts.len() < MAX_STARTS;
        if ok {
            self.starts.push_back(now_ms);
        }
        ok
    }
}

/// How navi-bar is started.
pub struct Bar {
    path: PathBuf,
    /// The public socket, for the apps it launches.
    wayland_display: OsString,
    /// `XCURSOR_THEME` and `XCURSOR_SIZE` for apps.
    cursor: Option<(String, u32)>,
    /// Real hardware: the session's variables, and navi-bar exports them to D-Bus.
    session: bool,
    respawn: Respawn,
    clock: Instant,
}

/// Spawns `navi-bar` next to navi's own executable, with no shell and no `PATH` search, and
/// restarts it when it exits. Without it, navi runs without a bar.
pub fn start_bar(
    handle: &LoopHandle<'static, CalloopData>,
    dh: &mut DisplayHandle,
    wayland_display: &OsStr,
    cursor: Option<(String, u32)>,
    session: bool,
) {
    let path = match std::env::current_exe() {
        Ok(exe) => exe.with_file_name("navi-bar"),
        Err(e) => {
            warn!("navi-bar not started, navi's own path is unknown: {e}");
            return;
        }
    };
    let (tx, rx) = channel::channel::<()>();
    let bar = Rc::new(RefCell::new(Bar {
        path,
        wayland_display: wayland_display.to_os_string(),
        cursor,
        session,
        respawn: Respawn::default(),
        clock: Instant::now(),
    }));
    bar.borrow_mut().start(dh, &tx);
    let h = handle.clone();
    let restart = handle.insert_source(rx, move |event, _, _| {
        let Event::Msg(()) = event else {
            return;
        };
        let (bar, tx) = (bar.clone(), tx.clone());
        let timer = Timer::from_duration(RESTART_DELAY);
        let queued = h.insert_source(timer, move |_, _, data| {
            bar.borrow_mut().start(&mut data.state.display_handle, &tx);
            TimeoutAction::Drop
        });
        if let Err(e) = queued {
            warn!("navi-bar not restarted: {e}");
        }
    });
    if let Err(e) = restart {
        warn!("navi-bar won't be restarted: {e}");
    }
}

impl Bar {
    /// Starts navi-bar if the respawn limit allows; `exited` hears when it exits.
    fn start(&mut self, dh: &mut DisplayHandle, exited: &channel::Sender<()>) {
        if !self.respawn.allow(self.clock.elapsed().as_millis() as u64) {
            warn!("navi-bar keeps crashing, running without a bar");
            return;
        }
        if let Err(e) = self.spawn(dh, exited.clone()) {
            warn!(path = %self.path.display(), "navi-bar not started, running without a bar: {e}");
        }
    }

    fn spawn(&self, dh: &mut DisplayHandle, exited: channel::Sender<()>) -> io::Result<()> {
        let (ours, theirs) = UnixStream::pair()?;
        let mut cmd = Command::new(&self.path);
        // Without a theme navi drew nothing either, so apps keep their own default.
        if let Some((theme, size)) = &self.cursor {
            cmd.env("XCURSOR_THEME", theme)
                .env("XCURSOR_SIZE", size.to_string());
        }
        session_env(&mut cmd, self.session);
        let mut child = cmd
            // Its stdin is the socket; navi-bar moves it off fd 0 as it connects.
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            .env("WAYLAND_SOCKET", "0")
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env_remove("DISPLAY")
            .spawn()?;
        let pid = child.id();
        let bar = ClientState {
            privileged: true,
            ..Default::default()
        };
        // On failure navi-bar sees its socket close and exits.
        match dh.insert_client(ours, Arc::new(bar)) {
            Ok(_) => info!(pid, "navi-bar started"),
            Err(e) => warn!(pid, "navi-bar's connection refused: {e}"),
        }
        std::thread::spawn(move || {
            match child.wait() {
                Ok(status) => info!(pid, %status, "navi-bar exited"),
                Err(e) => warn!(pid, "navi-bar not reaped: {e}"),
            }
            // Fails only once navi is shutting down.
            let _ = exited.send(());
        });
        Ok(())
    }
}

/// On real hardware navi-bar, and the apps it starts, get the session's variables and export
/// them to D-Bus. Nested they never do, so the host's user services keep pointing at the host.
fn session_env(cmd: &mut Command, session: bool) {
    if !session {
        cmd.env_remove("NAVI_EXPORT_ENV");
        return;
    }
    if std::env::var_os("XDG_CURRENT_DESKTOP").is_none_or(|v| v.is_empty()) {
        cmd.env("XDG_CURRENT_DESKTOP", "navi");
    }
    cmd.env("XDG_SESSION_TYPE", "wayland")
        .env("NAVI_EXPORT_ENV", "1");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac10_respawn_allows_five_a_minute() {
        let mut r = Respawn::default();
        for t in [0, 1_000, 2_000, 3_000, 4_000] {
            assert!(r.allow(t), "{t}");
        }
        assert!(!r.allow(5_000));
        assert!(!r.allow(59_999));
        // The first start is now 60 s old.
        assert!(r.allow(60_000));
        assert!(!r.allow(60_500));
        assert!(r.allow(61_000));
    }

    #[test]
    fn nested_bar_never_exports() {
        let mut cmd = Command::new("true");
        session_env(&mut cmd, false);
        let envs: Vec<_> = cmd.get_envs().collect();
        assert_eq!(envs, [(OsStr::new("NAVI_EXPORT_ENV"), None)]);

        let mut cmd = Command::new("true");
        session_env(&mut cmd, true);
        let set: Vec<_> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?, v?.to_str()?)))
            .filter(|(k, _)| *k != "XDG_CURRENT_DESKTOP")
            .collect();
        assert_eq!(
            set,
            [("NAVI_EXPORT_ENV", "1"), ("XDG_SESSION_TYPE", "wayland")]
        );
    }
}
