#![forbid(unsafe_code)]

mod activation;
mod cursor;
mod files;
mod image;
mod input;
mod layout;
mod monitors;
mod persist;
mod render;
#[cfg(debug_assertions)]
mod replay;
mod screenshot;
mod shell;
mod state;
mod theme;
mod toplevels;
mod udev;
mod wallpaper;
mod winit;

use std::{
    error::Error,
    ffi::OsStr,
    process::ExitCode,
    sync::{Arc, Mutex},
};

use smithay::reexports::{
    calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
    wayland_server::Display,
};
use smithay::wayland::socket::ListeningSocketSource;
use tracing::{error, info, warn};
use tracing_subscriber::fmt::writer::MakeWriterExt;

use state::{ClientState, Navi};

pub struct CalloopData {
    pub state: Navi,
    pub display: Display<Navi>,
    /// Real hardware only.
    pub udev: Option<udev::Udev>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Backend {
    /// A window in another compositor.
    Nested,
    /// The whole machine: DRM, libinput and a seat session.
    Drm,
}

/// Nested when either display variable is set and non-empty. There is no override.
fn backend(wayland_display: Option<&OsStr>, display: Option<&OsStr>) -> Backend {
    let set = |v: Option<&OsStr>| v.is_some_and(|v| !v.is_empty());
    match set(wayland_display) || set(display) {
        true => Backend::Nested,
        false => Backend::Drm,
    }
}

fn main() -> ExitCode {
    let var = std::env::var_os;
    let backend = backend(var("WAYLAND_DISPLAY").as_deref(), var("DISPLAY").as_deref());
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    };
    if backend == Backend::Nested {
        tracing_subscriber::fmt().with_env_filter(filter()).init();
        return match run(backend) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                error!("navi: {e}");
                ExitCode::FAILURE
            }
        };
    }
    // Real hardware: stderr goes wherever the display manager puts it, so the log is kept too.
    let (file, failed) = match persist::state_file("navi.log").map(|p| (persist::create_log(&p), p))
    {
        Some((Ok(f), _)) => (Some(f), None),
        Some((Err(e), p)) => (None, Some(format!("{}: {e}", p.display()))),
        None => (None, Some("no absolute XDG_STATE_HOME or HOME".into())),
    };
    let sub = tracing_subscriber::fmt()
        .with_env_filter(filter())
        .with_ansi(false);
    match file {
        Some(f) => sub.with_writer(std::io::stderr.and(Mutex::new(f))).init(),
        None => sub.with_writer(std::io::stderr).init(),
    }
    if let Some(e) = failed {
        warn!("no log file: {e}");
    }
    match run(backend) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("navi: cannot start on real hardware: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(backend: Backend) -> Result<(), Box<dyn Error>> {
    let mut event_loop: EventLoop<CalloopData> = EventLoop::try_new()?;
    let mut display: Display<Navi> = Display::new()?;
    let display_fd = display.backend().poll_fd().try_clone_to_owned()?;
    let state = Navi::new(
        display.handle(),
        event_loop.handle(),
        event_loop.get_signal(),
    )?;

    let socket = ListeningSocketSource::new_auto()?;
    let socket_name = socket.socket_name().to_os_string();
    info!(
        "listening on WAYLAND_DISPLAY={}",
        socket_name.to_string_lossy()
    );
    let handle = event_loop.handle();
    handle.insert_source(socket, |stream, _, data| {
        if let Err(e) = data
            .state
            .display_handle
            .insert_client(stream, Arc::new(ClientState::default()))
        {
            warn!("rejected client: {e}");
        }
    })?;
    handle.insert_source(
        Generic::new(display_fd, Interest::READ, Mode::Level),
        |_, _, data| {
            data.display.dispatch_clients(&mut data.state)?;
            Ok(PostAction::Continue)
        },
    )?;

    let mut data = CalloopData {
        state,
        display,
        udev: None,
    };
    let theme = match backend {
        Backend::Nested => {
            let cursor = cursor::from_env(1.0);
            let theme = cursor
                .as_ref()
                .map(|c| (c.theme.clone(), cursor::SIZE.round() as u32));
            winit::init(&mut event_loop, &mut data, cursor)?;
            theme
        }
        Backend::Drm => udev::init(&mut event_loop, &mut data)?,
    };
    // After the first output exists, so the bar lands on it.
    shell::start_bar(
        &handle,
        &mut data.state.display_handle,
        &socket_name,
        theme,
        backend == Backend::Drm,
    );
    #[cfg(debug_assertions)]
    replay::start(&handle);
    event_loop.run(None, &mut data, |_| {})?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac1_backend_selection() {
        let s = |v: &'static str| Some(OsStr::new(v));
        assert_eq!(backend(s("wayland-1"), None), Backend::Nested);
        assert_eq!(backend(None, s(":0")), Backend::Nested);
        assert_eq!(backend(s("wayland-1"), s(":0")), Backend::Nested);
        assert_eq!(backend(None, None), Backend::Drm);
        assert_eq!(backend(s(""), s("")), Backend::Drm);
        assert_eq!(backend(s(""), None), Backend::Drm);
    }

    /// AC14: `desktop-file-validate` rejects `DesktopNames`, a session-file key (Plasma's own
    /// session file fails it the same way), so the keys a display manager reads are checked here.
    #[test]
    fn ac14_session_file() {
        let text = include_str!("../dist/navi.desktop");
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("[Desktop Entry]"));
        let keys: Vec<(&str, &str)> = lines.filter_map(|l| l.split_once('=')).collect();
        for want in [
            ("Name", "navi"),
            ("Exec", "/usr/local/bin/navi"),
            ("TryExec", "/usr/local/bin/navi"),
            ("Type", "Application"),
            ("DesktopNames", "navi"),
        ] {
            assert!(keys.contains(&want), "{want:?}");
        }
        assert_eq!(keys.len(), 6);
    }
}
