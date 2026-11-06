//! Compositor-owned session lock. Only our privately connected swaylock can unlock.

use std::{
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::{Command, Stdio},
    sync::{Arc, mpsc::Sender},
    time::Duration,
};

use smithay::{
    input::pointer::MotionEvent,
    output::Output,
    reexports::{
        calloop::{
            channel,
            timer::{TimeoutAction, Timer},
        },
        wayland_protocols::ext::session_lock::v1::server::{
            ext_session_lock_manager_v1::ExtSessionLockManagerV1,
            ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
            ext_session_lock_v1::{self, ExtSessionLockV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, Resource, delegate_dispatch,
            delegate_global_dispatch,
            protocol::{wl_output::WlOutput, wl_surface::WlSurface},
        },
    },
    utils::SERIAL_COUNTER,
    wayland::{
        selection::data_device::set_data_device_focus,
        session_lock::{
            ExtLockSurfaceUserData, LockSurface, SessionLockHandler, SessionLockManagerGlobalData,
            SessionLockManagerState, SessionLockState, SessionLocker,
        },
    },
};
use tracing::{info, warn};

use crate::{
    focus::Focus,
    shell::Respawn,
    state::{ClientState, Navi},
};

mod background;
pub mod logind;

pub const SWAYLOCK: &str = "/usr/bin/swaylock";

pub fn executable() -> std::path::PathBuf {
    // Like input replay, isolated package testing is absent from release builds.
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("NAVI_TEST_SWAYLOCK") {
        let path = std::path::PathBuf::from(path);
        if path.is_absolute() {
            return path;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let helper = exe.with_file_name("navi-swaylock");
        if helper.is_file() && std::path::Path::new(SWAYLOCK).is_file() {
            return helper;
        }
    }
    SWAYLOCK.into()
}

fn command(path: &std::path::Path, backgrounds: &[background::Background]) -> Command {
    let mut cmd = Command::new(path);
    cmd.args([
        "--config",
        "/dev/null",
        "--color",
        "0b0d12",
        "--scaling",
        "fill",
        "--indicator-idle-visible",
        "--indicator-radius",
        "52",
        "--indicator-thickness",
        "6",
        "--indicator-caps-lock",
        "--font",
        "Noto Sans",
        "--font-size",
        "16",
        "--inside-caps-lock-color",
        "151922",
        "--ring-caps-lock-color",
        "f0c674",
        "--text-caps-lock-color",
        "f0c674",
        "--line-caps-lock-color",
        "00000000",
        "--inside-color",
        "151922",
        "--inside-clear-color",
        "151922",
        "--inside-ver-color",
        "151922",
        "--inside-wrong-color",
        "151922",
        "--ring-color",
        "2ba3d8",
        "--ring-clear-color",
        "2ba3d8",
        "--ring-ver-color",
        "2ba3d8",
        "--ring-wrong-color",
        "f07886",
        "--key-hl-color",
        "2ba3d8",
        "--bs-hl-color",
        "d0d4dc",
        "--line-color",
        "00000000",
        "--line-clear-color",
        "00000000",
        "--line-ver-color",
        "00000000",
        "--line-wrong-color",
        "00000000",
        "--text-clear-color",
        "d0d4dc",
        "--text-color",
        "d0d4dc",
        "--text-ver-color",
        "d0d4dc",
        "--text-wrong-color",
        "f07886",
        "--ignore-empty-password",
    ])
    .env("WAYLAND_SOCKET", "0")
    .env_remove("DISPLAY")
    .stdout(Stdio::null());
    if let Some(first) = backgrounds.first() {
        // A newly connected output gets a protected, blurred default too.
        cmd.arg("--image").arg(&first.path);
    }
    for bg in backgrounds.iter().skip(1) {
        cmd.arg("--image")
            .arg(format!("{}:{}", bg.name, bg.path.display()));
    }
    cmd
}

#[derive(Default)]
pub struct Lock {
    /// Remains true on disconnect, authentication failure and client crash.
    pub active: bool,
    pub epoch: u64,
    pub surfaces: Vec<(Output, LockSurface)>,
    owner: Option<ExtSessionLockV1>,
    confirmation: Option<SessionLocker>,
    covered: Vec<Output>,
    replies: Vec<Sender<bool>>,
    spawning: bool,
    timeout: Option<smithay::reexports::calloop::RegistrationToken>,
    starts: Respawn,
    pub(super) hint: Option<Sender<bool>>,
}

impl Navi {
    pub fn lock_outputs_covered(&self) -> bool {
        self.lock.active && self.space.outputs().all(|o| self.lock.covered.contains(o))
    }
    pub fn protect_session(&mut self) {
        if !self.lock.active {
            self.lock.active = true;
            self.isolate_lock_input();
            self.lock_outputs_changed();
        }
    }

    pub fn request_lock(&mut self, reply: Option<Sender<bool>>) {
        if let Some(reply) = reply {
            self.lock.replies.push(reply);
        }
        // Already locked with a live UI: recheck current output coverage.
        if self.lock.owner.as_ref().is_some_and(Resource::is_alive) {
            self.confirm_lock();
            return;
        }
        if self.lock.spawning {
            return;
        }
        if let Err(e) = self.spawn_locker() {
            warn!("swaylock not started: {e}");
            self.lock_reply(false);
        }
    }

    fn spawn_locker(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if !self.lock.starts.allow(self.now_ms()) {
            return Err("swaylock restarted too often; session stays protected".into());
        }
        if !executable().is_file() {
            return Err("Install the swaylock package first".into());
        }
        let outputs = self
            .space
            .outputs()
            .filter_map(|o| {
                let geo = self.space.output_geometry(o)?;
                Some((
                    o.name(),
                    self.priority_of(&crate::state::identity(o)),
                    geo.size.w,
                    geo.size.h,
                ))
            })
            .collect();
        let (tx, rx) = channel::channel();
        self.loop_handle.insert_source(rx, |event, _, data| {
            if let channel::Event::Msg(backgrounds) = event
                && let Err(e) = data.state.spawn_locker_ready(backgrounds)
            {
                data.state.lock.spawning = false;
                data.state.lock_reply(false);
                warn!("swaylock not started; protection remains active: {e}");
            }
        })?;
        self.lock.spawning = true;
        // Protect immediately while decoding/blur/PNG work happens off the event loop.
        self.protect_session();
        std::thread::spawn(move || {
            let _ = tx.send(background::prepare(outputs));
        });
        Ok(())
    }

    fn spawn_locker_ready(
        &mut self,
        backgrounds: Vec<background::Background>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (ours, theirs) = UnixStream::pair()?;
        let mut cmd = command(&executable(), &backgrounds);
        let mut child = cmd.stdin(Stdio::from(OwnedFd::from(theirs))).spawn()?;
        let client = match self.display_handle.insert_client(
            ours,
            Arc::new(ClientState {
                locker: true,
                ..Default::default()
            }),
        ) {
            Ok(c) => c,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        };
        self.lock.spawning = true;
        let timeout_client = client.clone();
        // A failed startup must not leave a menu action waiting forever.
        self.lock.timeout = Some(self.loop_handle.insert_source(Timer::from_duration(Duration::from_secs(10)),
            move |_, _, data| {
                data.state.lock.timeout = None;
                let current = data.state.lock.owner.as_ref().and_then(Resource::client);
                if data.state.lock.spawning && (current.is_none() || current.as_ref() == Some(&timeout_client)) {
                    data.state.lock_reply(false);
                    if current.is_none() {
                        data.state.display_handle.backend_handle().kill_client(timeout_client.id(), smithay::reexports::wayland_server::backend::DisconnectReason::ConnectionClosed);
                    }
                }
                TimeoutAction::Drop
            })?);
        let (tx, rx) = channel::channel();
        self.loop_handle.insert_source(rx, move |event, _, data| {
            if let channel::Event::Msg(()) = event {
                // Drain a valid final unlock sent just before the process exited.
                let _ = data.display.dispatch_clients(&mut data.state);
                // Disconnection is not proof of successful authentication.
                data.state.display_handle.backend_handle().kill_client(
                    client.id(),
                    smithay::reexports::wayland_server::backend::DisconnectReason::ConnectionClosed,
                );
                data.state.lock.spawning = false;
                if let Some(token) = data.state.lock.timeout.take() {
                    data.state.loop_handle.remove(token);
                }
                data.state.lock_reply(false);
                if data.state.lock.active {
                    data.state.lock.owner = None;
                    data.state.lock.confirmation = None;
                    data.state.lock.surfaces.clear();
                    data.state.isolate_lock_input();
                    data.state.request_redraw();
                    let _ = data.state.loop_handle.insert_source(
                        Timer::from_duration(Duration::from_secs(1)),
                        |_, _, data| {
                            if data.state.lock.active {
                                data.state.request_lock(None);
                            }
                            TimeoutAction::Drop
                        },
                    );
                }
            }
        })?;
        std::thread::spawn(move || {
            let status = child.wait();
            drop(backgrounds);
            info!(?status, "swaylock exited");
            let _ = tx.send(());
        });
        Ok(())
    }

    fn lock_reply(&mut self, ok: bool) {
        for reply in self.lock.replies.drain(..) {
            let _ = reply.send(ok);
        }
    }

    pub fn isolate_lock_input(&mut self) {
        self.release_sticky();
        self.cancel_drag();
        if let Some(kb) = self.seat.get_keyboard() {
            kb.unset_grab(self);
            kb.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
        if let Some(p) = self.seat.get_pointer() {
            let serial = SERIAL_COUNTER.next_serial();
            p.unset_grab(self, serial, 0);
            p.motion(
                self,
                None,
                &MotionEvent {
                    location: p.current_location(),
                    serial,
                    time: 0,
                },
            );
            p.frame(self);
        }
        set_data_device_focus(&self.display_handle, &self.seat, None);
    }

    /// A mode/topology change invalidates queued presentation evidence.
    pub fn lock_outputs_changed(&mut self) {
        if !self.lock.active {
            return;
        }
        self.lock.epoch += 1;
        self.lock.covered.clear();
        self.lock
            .surfaces
            .retain(|(o, s)| s.alive() && self.space.outputs().any(|x| x == o));
        for (output, surface) in &self.lock.surfaces {
            if let Some(geo) = self.space.output_geometry(output) {
                surface.with_pending_state(|s| {
                    s.size = Some((geo.size.w as u32, geo.size.h as u32).into())
                });
                surface.send_configure();
            }
        }
        self.confirm_lock();
        self.request_redraw();
    }

    pub fn lock_needs_present(&self, output: &Output) -> bool {
        self.lock.active && !self.lock.covered.contains(output)
    }

    /// Called only after a protected frame is actually presented, never merely queued.
    pub fn lock_presented(&mut self, output: &Output, epoch: u64) {
        if !self.lock.active || epoch != self.lock.epoch {
            return;
        }
        if !self.lock.covered.contains(output) {
            self.lock.covered.push(output.clone());
        }
        self.confirm_lock();
    }

    fn confirm_lock(&mut self) {
        if !self.lock.active || !self.space.outputs().all(|o| self.lock.covered.contains(o)) {
            return;
        }
        if !self.lock.owner.as_ref().is_some_and(Resource::is_alive) {
            return;
        }
        if let Some(confirmation) = self.lock.confirmation.take() {
            confirmation.lock();
            info!("session protected on every output");
            if let Some(tx) = &self.lock.hint {
                let _ = tx.send(true);
            }
        }
        if self.lock.owner.as_ref().is_some_and(Resource::is_alive) {
            self.lock_reply(true);
        }
    }

    pub fn focus_lock_surface(&mut self, surface: Option<WlSurface>) {
        if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(
                self,
                surface.map(Focus::Wayland),
                SERIAL_COUNTER.next_serial(),
            );
        }
    }
}

impl SessionLockHandler for Navi {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        if self.lock.owner.as_ref().is_some_and(Resource::is_alive) {
            return;
        }
        self.lock.active = true;
        self.lock.owner = Some(confirmation.ext_session_lock().clone());
        self.lock.confirmation = Some(confirmation);
        self.lock.surfaces.clear();
        self.isolate_lock_input();
        self.lock_outputs_changed();
    }

    fn unlock(&mut self) {
        // Only the confirmed owner reaches this via the guarded dispatcher below.
        self.lock.active = false;
        self.lock.owner = None;
        self.lock.confirmation = None;
        self.lock.surfaces.clear();
        self.lock_reply(false);
        self.focus_lock_surface(None);
        self.request_redraw();
        if let Some(tx) = &self.lock.hint {
            let _ = tx.send(false);
        }
        info!("session unlocked by swaylock");
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        let Some(output) = Output::from_resource(&output) else {
            return;
        };
        let Some(geo) = self.space.output_geometry(&output) else {
            return;
        };
        if self.lock.surfaces.iter().any(|(o, _)| o == &output) {
            return;
        }
        surface
            .with_pending_state(|s| s.size = Some((geo.size.w as u32, geo.size.h as u32).into()));
        self.lock.surfaces.push((output, surface));
        self.request_redraw();
    }
}

// Smithay 0.7 calls unlock() even after posting InvalidUnlock. Reject nonowners
// and pre-confirmation requests before delegating, including failed second locks.
impl Dispatch<ExtSessionLockV1, SessionLockState> for Navi {
    fn request(
        state: &mut Self,
        client: &Client,
        lock: &ExtSessionLockV1,
        request: ext_session_lock_v1::Request,
        data: &SessionLockState,
        dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if state.lock.owner.as_ref() != Some(lock) {
            if !matches!(request, ext_session_lock_v1::Request::Destroy) {
                lock.post_error(
                    ext_session_lock_v1::Error::InvalidUnlock,
                    "Not the session lock owner",
                );
            }
            return;
        }
        if matches!(request, ext_session_lock_v1::Request::UnlockAndDestroy)
            && state.lock.confirmation.is_some()
        {
            lock.post_error(
                ext_session_lock_v1::Error::InvalidUnlock,
                "Lock not confirmed",
            );
            return;
        }
        <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::request(
            state, client, lock, request, data, dh, init,
        );
    }
}

delegate_global_dispatch!(Navi: [ExtSessionLockManagerV1: SessionLockManagerGlobalData] => SessionLockManagerState);
delegate_dispatch!(Navi: [ExtSessionLockManagerV1: ()] => SessionLockManagerState);
delegate_dispatch!(Navi: [ExtSessionLockSurfaceV1: ExtLockSurfaceUserData] => SessionLockManagerState);

#[cfg(test)]
pub(crate) mod tests;
