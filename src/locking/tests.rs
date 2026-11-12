//! Real protocol connections to an isolated compositor; no host display, PAM or power calls.
use super::*;
use crate::CalloopData;
use smithay::{
    backend::input::KeyState,
    output::{Mode, PhysicalProperties, Subpixel},
    reexports::{calloop::EventLoop, wayland_server::Display},
};
use smithay_client_toolkit::reexports::{
    client::{
        self, Connection, EventQueue, Proxy, QueueHandle,
        backend::WaylandError,
        delegate_noop,
        protocol::{wl_compositor, wl_keyboard, wl_output, wl_registry, wl_seat, wl_surface},
    },
    protocols::ext::session_lock::v1::client::{
        ext_session_lock_manager_v1 as manager, ext_session_lock_surface_v1 as lock_surface,
        ext_session_lock_v1 as lock,
    },
};

#[derive(Default)]
struct View {
    manager: Option<manager::ExtSessionLockManagerV1>,
    compositor: Option<wl_compositor::WlCompositor>,
    outputs: Vec<wl_output::WlOutput>,
    seat: Option<wl_seat::WlSeat>,
    locked: usize,
    finished: usize,
    keys: usize,
    configures: Vec<(u32, u32)>,
}

impl client::Dispatch<wl_registry::WlRegistry, ()> for View {
    fn event(
        s: &mut Self,
        r: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        q: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            match interface.as_str() {
                "ext_session_lock_manager_v1" => s.manager = Some(r.bind(name, 1, q, ())),
                "wl_compositor" => s.compositor = Some(r.bind(name, version.min(4), q, ())),
                "wl_output" => s.outputs.push(r.bind(name, version.min(4), q, ())),
                "wl_seat" => s.seat = Some(r.bind(name, version.min(4), q, ())),
                _ => {}
            }
        }
    }
}
impl client::Dispatch<lock::ExtSessionLockV1, ()> for View {
    fn event(
        s: &mut Self,
        _: &lock::ExtSessionLockV1,
        e: lock::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            lock::Event::Locked => s.locked += 1,
            lock::Event::Finished => s.finished += 1,
            _ => {}
        }
    }
}
impl client::Dispatch<lock_surface::ExtSessionLockSurfaceV1, ()> for View {
    fn event(
        s: &mut Self,
        surface: &lock_surface::ExtSessionLockSurfaceV1,
        e: lock_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let lock_surface::Event::Configure {
            serial,
            width,
            height,
        } = e
        {
            s.configures.push((width, height));
            surface.ack_configure(serial);
        }
    }
}
impl client::Dispatch<wl_keyboard::WlKeyboard, ()> for View {
    fn event(
        s: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        e: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(e, wl_keyboard::Event::Key { .. }) {
            s.keys += 1;
        }
    }
}
delegate_noop!(View: ignore manager::ExtSessionLockManagerV1);
delegate_noop!(View: ignore wl_compositor::WlCompositor);
delegate_noop!(View: ignore wl_surface::WlSurface);
delegate_noop!(View: ignore wl_output::WlOutput);
delegate_noop!(View: ignore wl_seat::WlSeat);

struct Peer {
    server: Client,
    queue: EventQueue<View>,
    view: View,
}
pub(crate) struct Harness {
    pub(crate) data: CalloopData,
    _events: EventLoop<'static, CalloopData>,
}
impl Harness {
    pub(crate) fn new() -> Self {
        let events = EventLoop::try_new().unwrap();
        let display = Display::new().unwrap();
        let mut state = Navi::new(display.handle(), events.handle(), events.get_signal()).unwrap();
        state.layout = Default::default();
        state.memory = crate::persist::tests::volatile();
        Self {
            data: CalloopData {
                state,
                display,
                udev: None,
            },
            _events: events,
        }
    }
    pub(crate) fn output(&mut self, name: &str, x: i32) -> Output {
        let o = Output::new(
            name.into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        o.create_global::<Navi>(&self.data.state.display_handle);
        o.change_current_state(
            Some(Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            }),
            None,
            None,
            Some((x, 0).into()),
        );
        self.data.state.space.map_output(&o, (x, 0));
        self.data.state.add_output(&o, 0);
        o
    }
    fn peer(&mut self, locker: bool) -> Peer {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let server = self
            .data
            .state
            .display_handle
            .insert_client(
                ours,
                Arc::new(ClientState {
                    locker,
                    ..Default::default()
                }),
            )
            .unwrap();
        let c = Connection::from_socket(theirs).unwrap();
        let queue = c.new_event_queue();
        c.display().get_registry(&queue.handle(), ());
        let mut p = Peer {
            server,
            queue,
            view: View::default(),
        };
        self.pump(&mut p);
        p
    }
    fn pump(&mut self, p: &mut Peer) {
        for _ in 0..8 {
            p.queue.flush().unwrap();
            self.data
                .display
                .dispatch_clients(&mut self.data.state)
                .unwrap();
            crate::after_dispatch(&mut self.data);
            if let Some(read) = p.queue.prepare_read() {
                match read.read() {
                    Ok(_) => {}
                    Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("client read: {e}"),
                }
            }
            p.queue.dispatch_pending(&mut p.view).unwrap();
        }
    }
    fn disconnect(&mut self, p: &Peer) {
        self.data.state.display_handle.backend_handle().kill_client(
            p.server.id(),
            smithay::reexports::wayland_server::backend::DisconnectReason::ConnectionClosed,
        );
        self.data
            .display
            .dispatch_clients(&mut self.data.state)
            .unwrap();
    }
}

#[test]
fn lock_is_private_waits_for_all_presentations_and_survives_client_death() {
    let mut h = Harness::new();
    let a = h.output("a", 0);
    let b = h.output("b", 800);
    let mut app = h.peer(false);
    assert!(
        app.view.manager.is_none(),
        "apps cannot create a replacement unlocker"
    );
    let app_surface = app
        .view
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&app.queue.handle(), ());
    let _keyboard = app
        .view
        .seat
        .as_ref()
        .unwrap()
        .get_keyboard(&app.queue.handle(), ());
    h.pump(&mut app);
    let normal: WlSurface = app
        .server
        .object_from_protocol_id(&h.data.state.display_handle, app_surface.id().protocol_id())
        .unwrap();
    h.data.state.focus_lock_surface(Some(normal.clone()));
    // The app under the pointer hid the cursor before the lock.
    h.data.state.cursor.focus(Some(normal.clone()));
    h.data.state.cursor.request = crate::cursor::Request::Hidden;
    let mut locker = h.peer(true);
    let lock = locker
        .view
        .manager
        .as_ref()
        .unwrap()
        .lock(&locker.queue.handle(), ());
    h.pump(&mut locker);
    assert!(h.data.state.lock.active);
    assert!(
        h.data
            .state
            .seat
            .get_keyboard()
            .unwrap()
            .current_focus()
            .is_none()
    );
    assert!(h.data.state.pointer_focus((10., 10.).into()).is_none());
    assert_eq!(
        h.data.state.cursor.request,
        crate::cursor::Request::default(),
        "the app's cursor request ends with its pointer focus at lock"
    );
    assert_eq!(locker.view.locked, 0);
    let epoch = h.data.state.lock.epoch;
    h.data.state.lock_presented(&a, epoch.wrapping_sub(1));
    h.data.state.lock_presented(&b, epoch);
    h.pump(&mut locker);
    assert_eq!(locker.view.locked, 0);
    let c = h.output("hotplug", 1600);
    let epoch = h.data.state.lock.epoch;
    h.data.state.lock_presented(&a, epoch);
    h.data.state.lock_presented(&b, epoch);
    h.pump(&mut locker);
    assert_eq!(locker.view.locked, 0);
    h.data.state.lock_presented(&c, epoch);
    h.pump(&mut locker);
    assert_eq!(locker.view.locked, 1);

    let s = locker
        .view
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&locker.queue.handle(), ());
    let _ls = lock.get_lock_surface(&s, &locker.view.outputs[0], &locker.queue.handle(), ());
    let _lock_keyboard = locker
        .view
        .seat
        .as_ref()
        .unwrap()
        .get_keyboard(&locker.queue.handle(), ());
    h.pump(&mut locker);
    assert_eq!(locker.view.configures, vec![(800, 600)]);
    let protected = h.data.state.lock.surfaces[0].1.wl_surface().clone();
    h.data.state.focus_lock_surface(Some(protected));
    // Background layout updates cannot redirect the password.
    h.data.state.focus(None);
    h.data
        .state
        .keyboard_key(38u32.into(), KeyState::Pressed, 1);
    h.data
        .state
        .keyboard_key(38u32.into(), KeyState::Released, 2);
    h.pump(&mut locker);
    h.pump(&mut app);
    assert_eq!(locker.view.keys, 2);
    assert_eq!(app.view.keys, 0);
    // Regression: a key held by Navi before locking must not repeat its quit binding.
    h.data.state.suppressed.push(22u32.into());
    for code in [37u32, 64, 22] {
        h.data.state.keyboard_key(code.into(), KeyState::Pressed, 3);
    }
    assert!(h.data.state.lock.active);
    assert_eq!(h.data.state.pointer_focus((810., 10.).into()), None);
    h.disconnect(&locker);
    assert!(h.data.state.lock.active, "disconnect must never unlock");
    h.data.state.lock.surfaces.clear();
    assert!(h.data.state.pointer_focus((10., 10.).into()).is_none());

    // A new privately authorized locker can recover the session and then unlock normally.
    let mut recovery = h.peer(true);
    let new_lock = recovery
        .view
        .manager
        .as_ref()
        .unwrap()
        .lock(&recovery.queue.handle(), ());
    h.pump(&mut recovery);
    assert_eq!(recovery.view.locked, 0);
    let epoch = h.data.state.lock.epoch;
    for o in [a, b, c] {
        h.data.state.lock_presented(&o, epoch);
    }
    h.pump(&mut recovery);
    assert_eq!(recovery.view.locked, 1);
    new_lock.unlock_and_destroy();
    h.pump(&mut recovery);
    assert!(!h.data.state.lock.active);
}

#[test]
fn rejected_locker_cannot_unlock_the_owner() {
    let mut h = Harness::new();
    let a = h.output("a", 0);
    let mut owner = h.peer(true);
    let _lock = owner
        .view
        .manager
        .as_ref()
        .unwrap()
        .lock(&owner.queue.handle(), ());
    h.pump(&mut owner);
    let epoch = h.data.state.lock.epoch;
    h.data.state.lock_presented(&a, epoch);
    h.pump(&mut owner);
    let mut other = h.peer(true);
    let rejected = other
        .view
        .manager
        .as_ref()
        .unwrap()
        .lock(&other.queue.handle(), ());
    h.pump(&mut other);
    assert_eq!(other.view.finished, 1);
    rejected.unlock_and_destroy();
    other.queue.flush().unwrap();
    h.data.display.dispatch_clients(&mut h.data.state).unwrap();
    assert!(h.data.state.lock.active);
    assert_eq!(
        h.data.state.lock.owner.as_ref().unwrap().client().unwrap(),
        owner.server
    );
}

#[test]
fn owner_cannot_unlock_before_presentation_and_external_lock_is_fail_closed() {
    let mut h = Harness::new();
    h.output("a", 0);
    h.data.state.protect_session();
    assert!(h.data.state.lock.active);
    assert!(h.data.state.pointer_focus((10., 10.).into()).is_none());
    let mut owner = h.peer(true);
    let lock = owner
        .view
        .manager
        .as_ref()
        .unwrap()
        .lock(&owner.queue.handle(), ());
    h.pump(&mut owner);
    assert_eq!(owner.view.locked, 0);
    lock.unlock_and_destroy();
    owner.queue.flush().unwrap();
    h.data.display.dispatch_clients(&mut h.data.state).unwrap();
    assert!(h.data.state.lock.active);
}

/// Runs a real swaylock on an isolated Wayland socket. No host display, password,
/// PAM authentication attempt, sleep, logout or system configuration change.
#[test]
#[ignore = "requires swaylock; set NAVI_TEST_SWAYLOCK to test an extracted package"]
fn swaylock_real_client_handshake() {
    use std::time::Instant;
    let mut h = Harness::new();
    let output = h.output("isolated", 0);
    let (ours, theirs) = UnixStream::pair().unwrap();
    h.data
        .state
        .display_handle
        .insert_client(
            ours,
            Arc::new(ClientState {
                locker: true,
                ..Default::default()
            }),
        )
        .unwrap();
    // NAVI_TEST_SWAYLOCK is read by this test in release too, never by release Navi.
    let path = std::env::var_os("NAVI_TEST_SWAYLOCK")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| SWAYLOCK.into());
    let backgrounds = vec![background::create("isolated".into(), None, 800, 600).unwrap()];
    let mut cmd = command(&path, &backgrounds);
    let mut child = cmd
        .stdin(Stdio::from(OwnedFd::from(theirs)))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        loop {
            assert!(Instant::now() < deadline, "swaylock handshake timed out");
            assert!(
                child.try_wait().unwrap().is_none(),
                "swaylock exited before mapping"
            );
            h.data.display.dispatch_clients(&mut h.data.state).unwrap();
            if h.data.state.lock.active {
                h.data
                    .state
                    .lock_presented(&output, h.data.state.lock.epoch);
            }
            crate::render::send_frames(&mut h.data.state, &output, None);
            h.data.display.flush_clients().unwrap();
            let mapped = h.data.state.lock.surfaces.first().is_some_and(|(_, s)| {
                smithay::wayland::compositor::with_states(s.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<smithay::backend::renderer::utils::RendererSurfaceStateUserData>()
                        .is_some_and(|s| s.lock().unwrap().buffer().is_some())
                })
            });
            if mapped {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(h.data.state.lock.active);
        assert!(h.data.state.lock.confirmation.is_none());
        assert_eq!(h.data.state.lock.surfaces.len(), 1);
    }));
    let _ = child.kill();
    let _ = child.wait();
    let mut diagnostics = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut diagnostics).unwrap();
    assert!(
        !diagnostics.contains("Failed to load background"),
        "{diagnostics}"
    );
    h.data.display.dispatch_clients(&mut h.data.state).unwrap();
    assert!(
        h.data.state.lock.active,
        "killing swaylock must never unlock"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
