#![forbid(unsafe_code)]

mod activation;
mod files;
mod image;
mod input;
mod layout;
mod persist;
#[cfg(debug_assertions)]
mod replay;
mod screenshot;
mod shell;
mod state;
mod toplevels;
mod wallpaper;
mod winit;

use std::sync::Arc;

use smithay::reexports::{
    calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
    wayland_server::Display,
};
use smithay::wayland::socket::ListeningSocketSource;
use tracing::info;

use state::{ClientState, Navi};

pub struct CalloopData {
    pub state: Navi,
    pub display: Display<Navi>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut event_loop: EventLoop<CalloopData> = EventLoop::try_new()?;
    let mut display: Display<Navi> = Display::new()?;
    let display_fd = display.backend().poll_fd().try_clone_to_owned()?;
    let state = Navi::new(display.handle(), event_loop.get_signal())?;

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
            tracing::warn!("rejected client: {e}");
        }
    })?;
    handle.insert_source(
        Generic::new(display_fd, Interest::READ, Mode::Level),
        |_, _, data| {
            data.display.dispatch_clients(&mut data.state)?;
            Ok(PostAction::Continue)
        },
    )?;

    let mut data = CalloopData { state, display };
    winit::init(&mut event_loop, &mut data)?;
    // After the first output exists, so the bar lands on it.
    shell::start_bar(&mut data.state.display_handle, &socket_name);
    #[cfg(debug_assertions)]
    replay::start(&event_loop.handle());
    event_loop.run(None, &mut data, |_| {})?;
    Ok(())
}
