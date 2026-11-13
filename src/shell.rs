//! Starts navi-bar, navi's own shell client, over a socket only it holds, and starts it again
//! when it exits: after 1 s, at most `MAX_STARTS` times in any `WINDOW_MS`.

use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::{OsStr, OsString},
    io::{self, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::PathBuf,
    process::{Command, Stdio},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use smithay::reexports::calloop::{
    LoopHandle,
    channel::{self, Event},
    timer::{TimeoutAction, Timer},
};
use tracing::{info, warn};

use crate::{
    CalloopData,
    control::{self, NO, Request, YES},
    state::{ClientState, Navi},
};

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
    xdisplay: Option<String>,
    respawn: Respawn,
    clock: Instant,
}

/// Sends navi-bar one command. Never blocks: a full or broken socket drops it. Returns whether
/// it was sent.
pub fn send_command(control: &UnixStream, command: control::Command) -> bool {
    use rustix::net::{SendFlags, send};
    let bytes = command.bytes();
    match send(control, &bytes, SendFlags::DONTWAIT | SendFlags::NOSIGNAL) {
        Ok(n) if n == bytes.len() => true,
        // Linux sends a frame this small whole or not at all. Were it ever cut, navi-bar would
        // read the next bytes as its ID: end the stream instead.
        Ok(n) => {
            warn!(
                ?command,
                n, "control command cut short, closing navi-bar's control stream"
            );
            let _ = control.shutdown(std::net::Shutdown::Both);
            false
        }
        Err(e) => {
            warn!(?command, "control command dropped: {e}");
            false
        }
    }
}

/// Spawns `navi-bar` next to navi's own executable, with no shell and no `PATH` search, and
/// restarts it when it exits. Without it, navi runs without a bar. `Navi::bar_control` follows
/// each start, for `send_command`.
pub fn start_bar(
    handle: &LoopHandle<'static, CalloopData>,
    state: &mut Navi,
    wayland_display: &OsStr,
    cursor: Option<(String, u32)>,
    xdisplay: Option<String>,
    session: bool,
) {
    let path = match std::env::current_exe() {
        Ok(exe) => exe.with_file_name("navi-bar"),
        Err(e) => {
            warn!("navi-bar not started, navi's own path is unknown: {e}");
            return;
        }
    };
    // Only a successful exit from our own shell means the user confirmed Log Out.
    let (tx, rx) = channel::channel::<bool>();
    let bar = Rc::new(RefCell::new(Bar {
        path,
        wayland_display: wayland_display.to_os_string(),
        cursor,
        session,
        xdisplay,
        respawn: Respawn::default(),
        clock: Instant::now(),
    }));
    state.bar_control = bar.borrow_mut().start(state, &tx, handle);
    let h = handle.clone();
    let restart = handle.insert_source(rx, move |event, _, data| {
        let Event::Msg(logout) = event else {
            return;
        };
        data.state.bar_control = None;
        if logout && !data.state.lock.active {
            info!("logout requested by navi-bar");
            data.state.loop_signal.stop();
            return;
        }
        let (bar, tx) = (bar.clone(), tx.clone());
        let timer = Timer::from_duration(RESTART_DELAY);
        let queued = h.insert_source(timer, move |_, _, data| {
            let h = data.state.loop_handle.clone();
            data.state.bar_control = bar.borrow_mut().start(&mut data.state, &tx, &h);
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
    /// Starts navi-bar if the respawn limit allows; `exited` hears when it exits. Returns the
    /// writer for its control stream.
    fn start(
        &mut self,
        state: &mut Navi,
        exited: &channel::Sender<bool>,
        handle: &LoopHandle<'static, CalloopData>,
    ) -> Option<UnixStream> {
        if !self.respawn.allow(self.clock.elapsed().as_millis() as u64) {
            warn!("navi-bar keeps crashing, running without a bar");
            return None;
        }
        self.spawn(state, exited.clone(), handle)
            .inspect_err(|e| {
                warn!(path = %self.path.display(), "navi-bar not started, running without a bar: {e}");
            })
            .ok()
    }

    fn spawn(
        &self,
        state: &mut Navi,
        exited: channel::Sender<bool>,
        handle: &LoopHandle<'static, CalloopData>,
    ) -> io::Result<UnixStream> {
        let (ours, theirs) = UnixStream::pair()?;
        let (control, child_control) = UnixStream::pair()?;
        let toggles = control.try_clone()?;
        let (requests, events) = channel::channel::<(Request, std::sync::mpsc::Sender<bool>)>();
        std::thread::spawn(move || {
            serve(control, |request| {
                let (tx, rx) = std::sync::mpsc::channel();
                requests.send((request, tx)).ok()?;
                Some(rx.recv_timeout(Duration::from_secs(12)).unwrap_or(false))
            })
        });
        let mut cmd = Command::new(&self.path);
        // Without a theme navi drew nothing either, so apps keep their own default.
        if let Some((theme, size)) = &self.cursor {
            cmd.env("XCURSOR_THEME", theme)
                .env("XCURSOR_SIZE", size.to_string());
        }
        session_env(&mut cmd, self.session);
        match &self.xdisplay {
            Some(display) => {
                cmd.env("DISPLAY", display).env_remove("XAUTHORITY");
            }
            None => {
                cmd.env_remove("DISPLAY");
            }
        }
        let mut child = cmd
            // Its stdin is the socket; navi-bar moves it off fd 0 as it connects.
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            .stdout(Stdio::from(OwnedFd::from(child_control)))
            .env("NAVI_CONTROL_FD", "1")
            .env("WAYLAND_SOCKET", "0")
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .spawn()?;
        let pid = child.id();
        let bar = ClientState {
            privileged: true,
            child: Some(pid),
            ..Default::default()
        };
        // On failure navi-bar sees its socket close and exits.
        let client = match state.display_handle.insert_client(ours, Arc::new(bar)) {
            Ok(c) => {
                info!(pid, "navi-bar started");
                state.protect(c.clone());
                Some(c)
            }
            Err(e) => {
                warn!(pid, "navi-bar's connection refused: {e}");
                None
            }
        };
        handle
            .insert_source(events, move |event, _, data| {
                let Event::Msg((request, reply)) = event else {
                    return;
                };
                match (request, &client) {
                    (Request::Lock, _) => data.state.request_lock(Some(reply)),
                    (Request::CanLock, _) => {
                        let _ = reply.send(crate::locking::executable().is_file());
                    }
                    (Request::ForceClose(handle), Some(bar)) => {
                        data.state.force_close(bar, handle, reply);
                    }
                    (Request::ForceClose(_), None) => {
                        let _ = reply.send(false);
                    }
                }
            })
            .map_err(|e| io::Error::other(e.to_string()))?;
        std::thread::spawn(move || {
            let logout = match child.wait() {
                Ok(status) => {
                    info!(pid, %status, "navi-bar exited");
                    status.success()
                }
                Err(e) => {
                    warn!(pid, "navi-bar not reaped: {e}");
                    false
                }
            };
            // Fails only once navi is shutting down.
            let _ = exited.send(logout);
        });
        Ok(toggles)
    }
}

/// navi's end of the control stream: answers each request with `answer`'s `YES` or `NO` until
/// EOF, a short or unknown request, or `answer` returning `None`.
fn serve(mut control: UnixStream, mut answer: impl FnMut(Request) -> Option<bool>) {
    while let Some(request) = Request::read(&mut control) {
        let Some(ok) = answer(request) else { break };
        if control.write_all(&[if ok { YES } else { NO }]).is_err() {
            break;
        }
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
    use std::io::Read;

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
    fn toggles_never_block_and_drop_when_full() {
        let (navi, bar) = UnixStream::pair().unwrap();
        // The bar never reads: every send returns at once until the buffer fills.
        let start = Instant::now();
        let toggle = || send_command(&navi, control::Command::ToggleMenu);
        let sent = (0..10_000_000).take_while(|_| toggle()).count();
        assert!(sent > 0);
        assert!(!toggle(), "still full, still dropped");
        assert!(start.elapsed() < Duration::from_secs(10));
        let mut first = [0];
        (&bar).read_exact(&mut first).unwrap();
        assert_eq!(first, [control::TOGGLE_MENU]);
        drop(bar);
        assert!(
            !send_command(&navi, control::Command::ToggleMenu),
            "broken socket"
        );
    }

    /// AC11: navi's reader decodes a force close and answers it, around lock requests; a short
    /// force close then EOF ends it.
    #[test]
    fn ac11_serve_answers_force_close_and_ends_on_a_short_one() {
        let (bar, navi) = UnixStream::pair().unwrap();
        let (seen, asked) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            serve(navi, |r| {
                seen.send(r).unwrap();
                Some(r != Request::CanLock)
            })
        });
        let control = control::reader(bar.try_clone().unwrap(), |_| {}).unwrap();
        assert_eq!(control::request(&control, Request::Lock), Ok(()));
        let force = Request::ForceClose(0xFF00_0001);
        assert_eq!(control::request(&control, force), Ok(()));
        assert!(control::request(&control, Request::CanLock).is_err());
        let got: Vec<_> = asked.try_iter().collect();
        assert_eq!(got, [Request::Lock, force, Request::CanLock]);
        (&bar).write_all(&[control::FORCE_CLOSE, 1, 0]).unwrap();
        bar.shutdown(std::net::Shutdown::Write).unwrap();
        server.join().unwrap();
        assert!(
            asked.try_recv().is_err(),
            "the short request was never answered"
        );
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
