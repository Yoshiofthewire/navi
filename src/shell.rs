//! Starts navi-bar, navi's own shell client, over a socket only it holds.

use std::{
    ffi::OsStr,
    io,
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
};

use smithay::reexports::wayland_server::DisplayHandle;
use tracing::{info, warn};

use crate::state::ClientState;

/// Spawns the `navi-bar` next to navi's own executable, with no shell and no `PATH` search. Its
/// connection is the only privileged one. Apps it launches get `wayland_display`, the public
/// socket. Without it, navi runs without a bar.
pub fn start_bar(dh: &mut DisplayHandle, wayland_display: &OsStr) {
    let path = match std::env::current_exe() {
        Ok(exe) => exe.with_file_name("navi-bar"),
        Err(e) => {
            warn!("navi-bar not started, navi's own path is unknown: {e}");
            return;
        }
    };
    if let Err(e) = spawn(dh, &path, wayland_display) {
        warn!(path = %path.display(), "navi-bar not started, running without a bar: {e}");
    }
}

fn spawn(dh: &mut DisplayHandle, path: &Path, wayland_display: &OsStr) -> io::Result<()> {
    let (ours, theirs) = UnixStream::pair()?;
    let mut child = Command::new(path)
        // Its stdin is the socket; navi-bar moves it off fd 0 as it connects.
        .stdin(Stdio::from(OwnedFd::from(theirs)))
        .env("WAYLAND_SOCKET", "0")
        .env("WAYLAND_DISPLAY", wayland_display)
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
    std::thread::spawn(move || match child.wait() {
        Ok(status) => info!(pid, %status, "navi-bar exited"),
        Err(e) => warn!(pid, "navi-bar not reaped: {e}"),
    });
    Ok(())
}
