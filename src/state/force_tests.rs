//! Job 034: the window list's close and force close, with real clients on an in-process navi.
use super::*;
use crate::force::FORCE_AFTER;
use smithay::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop, event_created_child,
    protocol::{wl_callback, wl_compositor, wl_registry, wl_surface},
};
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::{
    xdg_surface, xdg_toplevel, xdg_wm_base,
};
use smithay_client_toolkit::reexports::protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as list_handle, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as list_manager, ZwlrForeignToplevelManagerV1},
};
use std::time::{Duration, Instant};

type Loop = EventLoop<'static, CalloopData>;

#[derive(Default)]
struct C {
    globals: Vec<String>,
    compositor: Option<wl_compositor::WlCompositor>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    list: Option<ZwlrForeignToplevelManagerV1>,
    /// The window list's handles: their last states and whether they closed.
    handles: Vec<(ZwlrForeignToplevelHandleV1, Vec<u32>, bool)>,
    /// `xdg_toplevel.close` events received; the client never acts on them.
    closes: usize,
    synced: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for C {
    fn event(
        c: &mut Self,
        r: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = e
        {
            match interface.as_str() {
                "wl_compositor" => c.compositor = Some(r.bind(name, 4, qh, ())),
                "xdg_wm_base" => c.wm = Some(r.bind(name, 3, qh, ())),
                "zwlr_foreign_toplevel_manager_v1" => c.list = Some(r.bind(name, 3, qh, ())),
                _ => {}
            }
            c.globals.push(interface);
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for C {
    fn event(
        _: &mut Self,
        wm: &xdg_wm_base::XdgWmBase,
        e: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for C {
    fn event(
        _: &mut Self,
        s: &xdg_surface::XdgSurface,
        e: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            s.ack_configure(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for C {
    fn event(
        c: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        e: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = e {
            c.closes += 1;
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for C {
    fn event(
        c: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        e: list_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let list_manager::Event::Toplevel { toplevel } = e {
            c.handles.push((toplevel, Vec::new(), false));
        }
    }

    event_created_child!(C, ZwlrForeignToplevelManagerV1, [
        list_manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for C {
    fn event(
        c: &mut Self,
        h: &ZwlrForeignToplevelHandleV1,
        e: list_handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(at) = c.handles.iter_mut().find(|(o, ..)| o == h) else {
            return;
        };
        match e {
            list_handle::Event::State { state } => {
                at.1 = state
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&b| u32::from_ne_bytes(b))
                    .collect();
            }
            list_handle::Event::Closed => at.2 = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for C {
    fn event(
        c: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        c.synced = true;
    }
}

delegate_noop!(C: ignore wl_compositor::WlCompositor);
delegate_noop!(C: ignore wl_surface::WlSurface);

struct Conn {
    conn: Connection,
    queue: EventQueue<C>,
    c: C,
    server: Client,
    /// Set once navi ended the connection.
    dead: bool,
}

impl Conn {
    fn new(el: &mut Loop, data: &mut CalloopData, privileged: bool) -> Self {
        let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        // The listening socket's own capture: a socketpair's peer is this test process.
        let cs = ClientState {
            privileged,
            ..ClientState::accepted(&server)
        };
        let server = data
            .state
            .display_handle
            .insert_client(server, Arc::new(cs))
            .unwrap();
        let conn = Connection::from_socket(client).unwrap();
        let queue = conn.new_event_queue::<C>();
        conn.display().get_registry(&queue.handle(), ());
        let mut c = Self {
            conn,
            queue,
            c: C::default(),
            server,
            dead: false,
        };
        c.sync(el, data);
        c.sync(el, data);
        c
    }

    /// A round trip; `false` once navi has ended the connection.
    fn sync(&mut self, el: &mut Loop, data: &mut CalloopData) -> bool {
        self.c.synced = false;
        self.conn.display().sync(&self.queue.handle(), ());
        let _ = self.queue.flush();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.c.synced && !self.dead {
            assert!(Instant::now() < deadline, "round-trip timed out");
            el.dispatch(Duration::from_millis(5), data).unwrap();
            crate::after_dispatch(data);
            if let Some(read) = self.queue.prepare_read() {
                let _ = read.read();
            }
            self.dead |= self.queue.dispatch_pending(&mut self.c).is_err();
            self.dead |= self.queue.flush().is_err() || self.conn.protocol_error().is_some();
        }
        !self.dead
    }

    /// A toplevel with app ID `app`, committed once.
    fn window(&mut self, app: &str) -> xdg_toplevel::XdgToplevel {
        let qh = self.queue.handle();
        let surface = self.c.compositor.as_ref().unwrap().create_surface(&qh, ());
        let xdg = self
            .c
            .wm
            .as_ref()
            .unwrap()
            .get_xdg_surface(&surface, &qh, ());
        let top = xdg.get_toplevel(&qh, ());
        top.set_app_id(app.into());
        surface.commit();
        top
    }
}

/// navi's window and tile for app `app`.
fn managed<'a>(data: &'a mut CalloopData, app: &str) -> Option<&'a mut Managed> {
    let windows = data.state.windows.iter_mut();
    windows.into_iter().find(|m| window_app(&m.window) == app)
}

fn tile(data: &mut CalloopData, app: &str) -> WinId {
    managed(data, app).and_then(|m| m.tile).expect("tiled")
}

/// AC8: only navi-bar sees the window list; its close is the polite one, refused while locked.
#[test]
fn ac8_bar_close_is_polite_and_refused_while_locked() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut bar = Conn::new(&mut el, &mut data, true);
    let mut a = Conn::new(&mut el, &mut data, false);
    let _top = a.window("a");
    a.sync(&mut el, &mut data);
    bar.sync(&mut el, &mut data);
    let manager = "zwlr_foreign_toplevel_manager_v1".to_string();
    assert!(bar.c.globals.contains(&manager));
    assert!(
        !a.c.globals.contains(&manager),
        "an ordinary client never sees it"
    );
    assert_eq!(bar.c.handles.len(), 1);
    let h = bar.c.handles[0].0.clone();

    h.close();
    bar.sync(&mut el, &mut data);
    a.sync(&mut el, &mut data);
    assert_eq!(a.c.closes, 1, "xdg_toplevel.close");
    assert!(
        managed(&mut data, "a").is_some_and(|m| m.tile.is_some()),
        "still mapped"
    );
    assert!(managed(&mut data, "a").unwrap().close_sent.is_some());

    data.state.lock.active = true;
    h.close();
    bar.sync(&mut el, &mut data);
    a.sync(&mut el, &mut data);
    assert_eq!(a.c.closes, 1, "nothing while locked");
}

/// AC12: force close needs a close unanswered for 90 s, ends only that client, and never
/// signals a socketpair client: its pid is the test's own.
#[test]
fn ac12_force_close_ends_only_the_overdue_client() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut bar = Conn::new(&mut el, &mut data, true);
    let mut a = Conn::new(&mut el, &mut data, false);
    let mut b = Conn::new(&mut el, &mut data, false);
    // A pidfd is held, so only the own-pid rule keeps the signal from this test process.
    let cs = a.server.get_data::<ClientState>().unwrap();
    assert_eq!(cs.pid, Some(std::process::id()));
    assert!(cs.pidfd.is_some());
    let (_ta, _tb) = (a.window("a"), b.window("b"));
    a.sync(&mut el, &mut data);
    b.sync(&mut el, &mut data);
    bar.sync(&mut el, &mut data);
    let (ia, ib) = (tile(&mut data, "a"), tile(&mut data, "b"));
    let server = bar.server.clone();
    let handle = |data: &CalloopData, id| data.state.toplevels.handle_of(&server, id);
    let ha = handle(&data, ia).expect("a's handle");
    let force = |data: &mut CalloopData, id: u32| {
        let (tx, rx) = std::sync::mpsc::channel();
        let signalled = data.state.force_close(&server, id, tx);
        (rx.recv_timeout(Duration::from_secs(5)).unwrap(), signalled)
    };

    // Before any close, and 89 s after it: refused.
    assert_eq!(force(&mut data, ha), (false, false));
    let h = bar
        .c
        .handles
        .iter()
        .find(|(o, ..)| o.id().protocol_id() == ha);
    h.unwrap().0.close();
    bar.sync(&mut el, &mut data);
    a.sync(&mut el, &mut data);
    assert_eq!(a.c.closes, 1);
    let stamp = |data: &mut CalloopData, ago| {
        managed(data, "a").unwrap().close_sent = Some(Instant::now() - ago);
    };
    stamp(&mut data, FORCE_AFTER - Duration::from_secs(1));
    assert_eq!(force(&mut data, ha), (false, false));
    assert!(a.sync(&mut el, &mut data), "a still connected");
    let overdue = |bar: &Conn| {
        let h = bar
            .c
            .handles
            .iter()
            .find(|(o, ..)| o.id().protocol_id() == ha);
        h.unwrap().1.contains(&crate::control::CLOSE_OVERDUE)
    };
    assert!(!overdue(&bar));

    // At 90 s: the state shows, but not while locked, nor for a made-up handle.
    stamp(&mut data, FORCE_AFTER);
    data.state.refresh_toplevels();
    bar.sync(&mut el, &mut data);
    assert!(overdue(&bar), "CLOSE_OVERDUE in a's states");
    data.state.lock.active = true;
    assert_eq!(force(&mut data, ha), (false, false), "locked");
    data.state.lock.active = false;
    assert_eq!(
        force(&mut data, 0xFF00_0001),
        (false, false),
        "made-up handle"
    );
    assert!(a.sync(&mut el, &mut data));

    // Done: no signal (own pid), a disconnected, its window and handle gone; b untouched.
    assert_eq!(force(&mut data, ha), (true, false));
    assert!(!a.sync(&mut el, &mut data), "a's connection is killed");
    bar.sync(&mut el, &mut data);
    assert!(managed(&mut data, "a").is_none());
    let h = bar
        .c
        .handles
        .iter()
        .find(|(o, ..)| o.id().protocol_id() == ha);
    assert!(h.unwrap().2, "a's handle closed");
    assert!(b.sync(&mut el, &mut data));
    assert_eq!(tile(&mut data, "b"), ib);
    assert!(handle(&data, ib).is_some());
    assert_eq!(b.c.closes, 0);
}

/// Job 040 AC2: a ✕ on navi's title bar is always the polite close; once the first close is
/// 90 s old it also sends navi-bar one `FORCE_OFFER` with the bar's handle for the window.
#[test]
fn ac2_close_button_offers_force_close_at_90_s() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut bar = Conn::new(&mut el, &mut data, true);
    data.state.protect(bar.server.clone());
    let (bar_end, navi_end) = std::os::unix::net::UnixStream::pair().unwrap();
    bar_end.set_nonblocking(true).unwrap();
    data.state.bar_control = Some(navi_end);
    let offers = || {
        let mut buf = [0; 64];
        let n = std::io::Read::read(&mut &bar_end, &mut buf).unwrap_or(0);
        buf[..n].to_vec()
    };
    let mut a = Conn::new(&mut el, &mut data, false);
    let mut b = Conn::new(&mut el, &mut data, false);
    let (_ta, _tb) = (a.window("a"), b.window("b"));
    a.sync(&mut el, &mut data);
    b.sync(&mut el, &mut data);
    bar.sync(&mut el, &mut data);
    let (ia, ib) = (tile(&mut data, "a"), tile(&mut data, "b"));
    let handle = data.state.toplevels.handle_of(&bar.server, ia).unwrap();

    // The first ✕ starts the clock, however late it is.
    data.state
        .close_button(ia, Instant::now() + Duration::from_secs(1000));
    a.sync(&mut el, &mut data);
    assert_eq!((a.c.closes, offers()), (1, vec![]), "first ✕");
    let sent = managed(&mut data, "a").unwrap().close_sent.unwrap();

    data.state
        .close_button(ia, sent + FORCE_AFTER - Duration::from_millis(1));
    a.sync(&mut el, &mut data);
    assert_eq!((a.c.closes, offers()), (2, vec![]), "at 89.999 s");

    data.state.close_button(ia, sent + FORCE_AFTER);
    a.sync(&mut el, &mut data);
    let mut want = vec![crate::control::FORCE_OFFER];
    want.extend(handle.to_le_bytes());
    assert_eq!((a.c.closes, offers()), (3, want), "at 90 s");
    assert_eq!(managed(&mut data, "a").unwrap().close_sent, Some(sent));

    // Locked: no offer. Another window's first ✕: none either.
    data.state.lock.active = true;
    data.state.close_button(ia, sent + FORCE_AFTER);
    data.state.lock.active = false;
    data.state.close_button(ib, sent + FORCE_AFTER);
    b.sync(&mut el, &mut data);
    assert_eq!((b.c.closes, offers()), (1, vec![]));
}
