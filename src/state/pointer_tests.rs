//! Job 030: real clients on an in-process navi. A still pointer's focus follows the scene.
//! `step` dispatches without `after_dispatch`, so each test first shows what main did (nothing
//! moved), then that `after_dispatch`'s re-pick alone moves pointer focus.
use super::*;
use crate::cursor::Shape;
use crate::layout::{Nudge, Side};
use smithay::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    backend::{ObjectId, WaylandError},
    delegate_noop,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
};
use smithay_client_toolkit::reexports::protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1 as lock_manager, ext_session_lock_surface_v1 as lock_surface,
    ext_session_lock_v1 as session_lock,
};
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::{
    xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel as top, xdg_wm_base,
};
use smithay_client_toolkit::reexports::protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1 as layer_shell, zwlr_layer_surface_v1 as layer_surface,
};
use std::time::{Duration, Instant};

type Loop = EventLoop<'static, CalloopData>;

#[derive(Clone, Debug, PartialEq)]
enum Ev {
    Enter(ObjectId, f64, f64, u32),
    Leave(ObjectId),
    Motion(f64, f64),
}

#[derive(Default)]
struct C {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    layers: Option<layer_shell::ZwlrLayerShellV1>,
    locks: Option<lock_manager::ExtSessionLockManagerV1>,
    output: Option<wl_output::WlOutput>,
    pointer: Option<wl_pointer::WlPointer>,
    ev: Vec<Ev>,
    /// Each role object's last configured size, by index; `None` until configured.
    size: Vec<Option<(i32, i32)>>,
    /// Pending toplevel/popup sizes, until their xdg_surface configure.
    pending: Vec<(i32, i32)>,
    /// Redraws and commits this surface on every enter and motion (AC7).
    echo: Option<(wl_surface::WlSurface, wl_buffer::WlBuffer)>,
    synced: bool,
    /// The tags of frame callbacks done (job 032).
    done: Vec<usize>,
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
                "wl_shm" => c.shm = Some(r.bind(name, 1, qh, ())),
                "xdg_wm_base" => c.wm = Some(r.bind(name, 3, qh, ())),
                "wl_seat" => {
                    let seat: wl_seat::WlSeat = r.bind(name, 5, qh, ());
                    c.pointer = Some(seat.get_pointer(qh, ()));
                    c.seat = Some(seat);
                }
                "zwlr_layer_shell_v1" => c.layers = Some(r.bind(name, 4, qh, ())),
                "ext_session_lock_manager_v1" => c.locks = Some(r.bind(name, 1, qh, ())),
                "wl_output" => c.output = Some(r.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for C {
    fn event(
        c: &mut Self,
        _: &wl_pointer::WlPointer,
        e: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let ev = match e {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => Ev::Enter(surface.id(), surface_x, surface_y, serial),
            wl_pointer::Event::Leave { surface, .. } => Ev::Leave(surface.id()),
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => Ev::Motion(surface_x, surface_y),
            _ => return,
        };
        if !matches!(ev, Ev::Leave(_))
            && let Some((s, b)) = &c.echo
        {
            s.attach(Some(b), 0, 0);
            s.damage_buffer(0, 0, i32::MAX, i32::MAX);
            s.commit();
        }
        c.ev.push(ev);
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

impl Dispatch<xdg_surface::XdgSurface, usize> for C {
    fn event(
        c: &mut Self,
        s: &xdg_surface::XdgSurface,
        e: xdg_surface::Event,
        &i: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            s.ack_configure(serial);
            c.size[i] = Some(c.pending[i]);
        }
    }
}

impl Dispatch<top::XdgToplevel, usize> for C {
    fn event(
        c: &mut Self,
        _: &top::XdgToplevel,
        e: top::Event,
        &i: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let top::Event::Configure { width, height, .. } = e
            && width > 0
            && height > 0
        {
            c.pending[i] = (width, height);
        }
    }
}

impl Dispatch<xdg_popup::XdgPopup, usize> for C {
    fn event(
        c: &mut Self,
        _: &xdg_popup::XdgPopup,
        e: xdg_popup::Event,
        &i: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::Configure { width, height, .. } = e {
            c.pending[i] = (width, height);
        }
    }
}

impl Dispatch<layer_surface::ZwlrLayerSurfaceV1, usize> for C {
    fn event(
        c: &mut Self,
        l: &layer_surface::ZwlrLayerSurfaceV1,
        e: layer_surface::Event,
        &i: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let layer_surface::Event::Configure {
            serial,
            width,
            height,
        } = e
        {
            l.ack_configure(serial);
            c.size[i] = Some((width as i32, height as i32));
        }
    }
}

impl Dispatch<lock_surface::ExtSessionLockSurfaceV1, usize> for C {
    fn event(
        c: &mut Self,
        l: &lock_surface::ExtSessionLockSurfaceV1,
        e: lock_surface::Event,
        &i: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let lock_surface::Event::Configure {
            serial,
            width,
            height,
        } = e
        {
            l.ack_configure(serial);
            c.size[i] = Some((width as i32, height as i32));
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

impl Dispatch<wl_callback::WlCallback, usize> for C {
    fn event(
        c: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        &tag: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        c.done.push(tag);
    }
}

delegate_noop!(C: ignore wl_compositor::WlCompositor);
delegate_noop!(C: ignore wl_surface::WlSurface);
delegate_noop!(C: ignore wl_shm::WlShm);
delegate_noop!(C: ignore wl_shm_pool::WlShmPool);
delegate_noop!(C: ignore wl_buffer::WlBuffer);
delegate_noop!(C: ignore wl_seat::WlSeat);
delegate_noop!(C: ignore wl_output::WlOutput);
delegate_noop!(C: ignore xdg_positioner::XdgPositioner);
delegate_noop!(C: ignore layer_shell::ZwlrLayerShellV1);
delegate_noop!(C: ignore lock_manager::ExtSessionLockManagerV1);
delegate_noop!(C: ignore session_lock::ExtSessionLockV1);

/// One surface with a role: its wl_surface and what keeps the role alive.
struct S {
    surface: wl_surface::WlSurface,
    xdg: Option<xdg_surface::XdgSurface>,
    top: Option<top::XdgToplevel>,
    popup: Option<xdg_popup::XdgPopup>,
    layer: Option<layer_surface::ZwlrLayerSurfaceV1>,
    lock: Option<lock_surface::ExtSessionLockSurfaceV1>,
}

struct Conn {
    conn: Connection,
    queue: EventQueue<C>,
    c: C,
    server: Client,
    s: Vec<S>,
}

impl Conn {
    fn new(el: &mut Loop, data: &mut CalloopData, cs: ClientState) -> Self {
        let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        let server = data
            .state
            .display_handle
            .insert_client(server, std::sync::Arc::new(cs))
            .unwrap();
        let conn = Connection::from_socket(client).unwrap();
        let queue = conn.new_event_queue::<C>();
        conn.display().get_registry(&queue.handle(), ());
        let mut c = Self {
            conn,
            queue,
            c: C::default(),
            server,
            s: Vec::new(),
        };
        c.sync(el, data);
        c.sync(el, data);
        c
    }

    fn read(&mut self) {
        if let Some(read) = self.queue.prepare_read() {
            match read.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("client read: {e}"),
            }
        }
        self.queue.dispatch_pending(&mut self.c).unwrap();
        self.queue.flush().unwrap();
    }

    /// A round-trip in which navi dispatches but never runs `after_dispatch`: main's loop
    /// without the re-pick. `full` runs it, as navi does.
    fn trip(&mut self, el: &mut Loop, data: &mut CalloopData, full: bool) {
        self.c.synced = false;
        self.conn.display().sync(&self.queue.handle(), ());
        self.queue.flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.c.synced {
            assert!(Instant::now() < deadline, "round-trip timed out");
            el.dispatch(Duration::from_millis(5), data).unwrap();
            match full {
                true => crate::after_dispatch(data),
                false => data.display.flush_clients().unwrap(),
            }
            self.read();
        }
    }

    fn step(&mut self, el: &mut Loop, data: &mut CalloopData) {
        self.trip(el, data, false);
    }

    fn sync(&mut self, el: &mut Loop, data: &mut CalloopData) {
        self.trip(el, data, true);
    }

    fn surface(&mut self) -> wl_surface::WlSurface {
        let qh = self.queue.handle();
        self.c.compositor.as_ref().unwrap().create_surface(&qh, ())
    }

    fn slot(&mut self, s: S) -> usize {
        self.s.push(s);
        self.c.size.push(None);
        self.c.pending.push((0, 0));
        self.s.len() - 1
    }

    /// A toplevel with app ID `app`, under `parent` if any, with minimum size `min` if any,
    /// committed without a buffer.
    fn toplevel(&mut self, app: &str, parent: Option<usize>, min: Option<(i32, i32)>) -> usize {
        let (qh, i) = (self.queue.handle(), self.s.len());
        let surface = self.surface();
        let xdg = self
            .c
            .wm
            .as_ref()
            .unwrap()
            .get_xdg_surface(&surface, &qh, i);
        let t = xdg.get_toplevel(&qh, i);
        t.set_app_id(app.into());
        if let Some(p) = parent {
            t.set_parent(self.s[p].top.as_ref());
        }
        if let Some((w, h)) = min {
            t.set_min_size(w, h);
        }
        surface.commit();
        self.slot(S {
            surface,
            xdg: Some(xdg),
            top: Some(t),
            popup: None,
            layer: None,
            lock: None,
        })
    }

    /// A grab-less popup of `parent`, `w`×`h` at `(x, y)` in its window geometry.
    fn popup(&mut self, parent: usize, (x, y): (i32, i32), (w, h): (i32, i32)) -> usize {
        let (qh, i) = (self.queue.handle(), self.s.len());
        let surface = self.surface();
        let wm = self.c.wm.as_ref().unwrap();
        let positioner = wm.create_positioner(&qh, ());
        positioner.set_size(w, h);
        positioner.set_anchor_rect(x, y, 1, 1);
        positioner.set_anchor(xdg_positioner::Anchor::TopLeft);
        positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
        let xdg = wm.get_xdg_surface(&surface, &qh, i);
        let popup = xdg.get_popup(self.s[parent].xdg.as_ref(), &positioner, &qh, i);
        surface.commit();
        self.slot(S {
            surface,
            xdg: Some(xdg),
            top: None,
            popup: Some(popup),
            layer: None,
            lock: None,
        })
    }

    /// A Top-layer surface `w`×`h` at the output's top left, offset by `(x, y)`.
    fn layer(&mut self, (x, y): (i32, i32), (w, h): (u32, u32)) -> usize {
        let (qh, i) = (self.queue.handle(), self.s.len());
        let surface = self.surface();
        let l = self
            .c
            .layers
            .as_ref()
            .expect("privileged")
            .get_layer_surface(
                &surface,
                None,
                layer_shell::Layer::Top,
                "menu".into(),
                &qh,
                i,
            );
        l.set_size(w, h);
        l.set_anchor(layer_surface::Anchor::Top | layer_surface::Anchor::Left);
        l.set_margin(y, 0, 0, x);
        surface.commit();
        self.slot(S {
            surface,
            xdg: None,
            top: None,
            popup: None,
            layer: Some(l),
            lock: None,
        })
    }

    /// An shm buffer `w`×`h`, in a pool of its own.
    fn buffer(&mut self, w: i32, h: i32) -> wl_buffer::WlBuffer {
        let qh = self.queue.handle();
        let path = std::env::temp_dir().join(format!(
            "navi-030-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        file.set_len((w * h * 4) as u64).unwrap();
        let shm = self.c.shm.as_ref().unwrap();
        let pool = shm.create_pool(std::os::fd::AsFd::as_fd(&file), w * h * 4, &qh, ());
        let b = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, &qh, ());
        pool.destroy();
        b
    }

    /// Attaches a buffer of surface `i`'s configured size (or `size`) and commits.
    fn draw(&mut self, i: usize, size: Option<(i32, i32)>) -> wl_buffer::WlBuffer {
        let (w, h) = size.or(self.c.size[i]).expect("configured");
        let b = self.buffer(w, h);
        let s = &self.s[i].surface;
        s.attach(Some(&b), 0, 0);
        s.damage_buffer(0, 0, w, h);
        s.commit();
        b
    }

    /// A lock surface on the output, from this client's lock.
    fn lock_surface(&mut self, lock: &session_lock::ExtSessionLockV1) -> usize {
        let (qh, i) = (self.queue.handle(), self.s.len());
        let surface = self.surface();
        let output = self.c.output.as_ref().unwrap();
        let l = lock.get_lock_surface(&surface, output, &qh, i);
        self.slot(S {
            surface,
            xdg: None,
            top: None,
            popup: None,
            layer: None,
            lock: Some(l),
        })
    }

    fn unmap(&mut self, i: usize) {
        self.s[i].surface.attach(None, 0, 0);
        self.s[i].surface.commit();
    }

    fn destroy(&mut self, i: usize) {
        let s = &self.s[i];
        if let Some(p) = &s.popup {
            p.destroy();
        }
        if let Some(t) = &s.top {
            t.destroy();
        }
        if let Some(l) = &s.layer {
            l.destroy();
        }
        if let Some(l) = &s.lock {
            l.destroy();
        }
        if let Some(x) = &s.xdg {
            x.destroy();
        }
        s.surface.destroy();
    }

    /// navi's side of surface `i`.
    fn server(&self, data: &CalloopData, i: usize) -> WlSurface {
        let id = self.s[i].surface.id().protocol_id();
        let dh = &data.state.display_handle;
        self.server.object_from_protocol_id(dh, id).unwrap()
    }

    fn id(&self, i: usize) -> ObjectId {
        self.s[i].surface.id()
    }

    fn take(&mut self) -> Vec<Ev> {
        std::mem::take(&mut self.c.ev)
    }

    /// A `w`×`h` cursor surface set at `hot` with enter `serial`, drawn and committed with a
    /// frame callback tagged `frame`.
    fn cursor(
        &mut self,
        serial: u32,
        (w, h): (i32, i32),
        hot: (i32, i32),
        frame: usize,
    ) -> wl_surface::WlSurface {
        let s = self.surface();
        let b = self.buffer(w, h);
        let pointer = self.c.pointer.as_ref().unwrap();
        pointer.set_cursor(serial, Some(&s), hot.0, hot.1);
        s.attach(Some(&b), 0, 0);
        s.damage_buffer(0, 0, w, h);
        s.frame(&self.queue.handle(), frame);
        s.commit();
        s
    }

    /// navi's side of any of this client's surfaces.
    fn server_of(&self, data: &CalloopData, s: &wl_surface::WlSurface) -> WlSurface {
        let dh = &data.state.display_handle;
        let id = s.id().protocol_id();
        self.server.object_from_protocol_id(dh, id).unwrap()
    }
}

fn pointer(data: &CalloopData) -> PointerHandle<Navi> {
    data.state.seat.get_pointer().unwrap()
}

/// The keyboard's focus and the stacking order: what a re-pick must never change (AC6).
fn keys_and_stack(data: &CalloopData) -> (Option<Focus>, Vec<Window>) {
    let kb = data.state.seat.get_keyboard().unwrap().current_focus();
    (kb, data.state.space.elements().cloned().collect())
}

/// Moves the pointer, as input does, and drops what the clients heard of it.
fn put_pointer(el: &mut Loop, data: &mut CalloopData, at: (f64, f64), cs: &mut [&mut Conn]) {
    data.state.pointer_motion(at.into(), 0);
    for c in cs {
        c.sync(el, data);
        c.take();
    }
}

/// Runs `after_dispatch` once, as navi's loop does, asserting the keyboard and the stacking order
/// stay as they were (AC6); then lets every client read.
fn repick(el: &mut Loop, data: &mut CalloopData, cs: &mut [&mut Conn]) {
    let before = keys_and_stack(data);
    crate::after_dispatch(data);
    assert!(
        keys_and_stack(data) == before,
        "the re-pick moved the keyboard or the stack"
    );
    for c in cs {
        c.step(el, data);
    }
}

/// The centre of window `app`'s geometry, global.
fn centre(data: &CalloopData, app: &str) -> (f64, f64) {
    let m = data
        .state
        .windows
        .iter()
        .find(|m| window_app(&m.window) == app)
        .unwrap();
    let g = data.state.space.element_geometry(&m.window).unwrap();
    (
        f64::from(g.loc.x + g.size.w / 2),
        f64::from(g.loc.y + g.size.h / 2),
    )
}

/// Opens toplevel `app` and draws it at its configured size.
fn window(el: &mut Loop, data: &mut CalloopData, c: &mut Conn, app: &str) -> usize {
    let i = c.toplevel(app, None, None);
    c.sync(el, data);
    c.draw(i, None);
    c.sync(el, data);
    i
}

fn entered(ev: &[Ev], id: &ObjectId) -> Option<(f64, f64, u32)> {
    ev.iter().find_map(|e| match e {
        Ev::Enter(s, x, y, serial) if s == id => Some((*x, *y, *serial)),
        _ => None,
    })
}

fn left(ev: &[Ev], id: &ObjectId) -> bool {
    ev.contains(&Ev::Leave(id.clone()))
}

/// Where surface `i`'s (0, 0) is, global, for a toplevel `app`.
fn origin(data: &CalloopData, app: &str, nth: usize) -> (f64, f64) {
    let w = &data
        .state
        .windows
        .iter()
        .filter(|m| window_app(&m.window) == app)
        .nth(nth)
        .unwrap()
        .window;
    let loc = data.state.space.element_location(w).unwrap() - w.geometry().loc;
    (f64::from(loc.x), f64::from(loc.y))
}

fn local(at: (f64, f64), o: (f64, f64)) -> (f64, f64) {
    (at.0 - o.0, at.1 - o.1)
}

/// The `since` navi's `token_created` would give a token request from `s` (AC6).
fn token_since(data: &CalloopData, s: &WlSurface) -> Option<u32> {
    let kb = data.state.seat.get_keyboard().unwrap();
    let held = kb.current_focus().and_then(|f| f.surface());
    kb.last_enter()
        .filter(|_| held.as_ref() == Some(s))
        .map(u32::from)
}

fn focus(data: &CalloopData) -> Option<WlSurface> {
    pointer(data).current_focus()
}

/// AC1, AC2, AC6, AC8: a float (too big for any tile) maps, unmaps, maps again and closes over
/// a still pointer on window A; then A's dialog maps and closes. Each time the surface on top
/// gets the pointer without motion, at the right local position, and the cursor shape resets.
#[test]
fn a_float_and_a_dialog_over_a_still_pointer_take_and_give_it_back() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    let mut b = Conn::new(&mut el, &mut data, ClientState::default());
    let ai = window(&mut el, &mut data, &mut a, "a");
    let at = centre(&data, "a");
    put_pointer(&mut el, &mut data, at, &mut [&mut a, &mut b]);
    let a_srv = a.server(&data, ai);
    assert_eq!(focus(&data), Some(a_srv.clone()));
    // A asked for the I-beam.
    data.state.cursor.request = crate::cursor::Request::Named("text");
    assert_eq!(data.state.cursor_shape(), Shape::Named("text"));

    // AC1: B floats centred, over the pointer, at its first buffer.
    let bi = b.toplevel("b", None, Some((900, 500)));
    b.sync(&mut el, &mut data);
    b.draw(bi, None);
    b.step(&mut el, &mut data);
    a.step(&mut el, &mut data);
    assert!(a.take().is_empty() && b.take().is_empty(), "main: no event");
    assert_eq!(focus(&data), Some(a_srv.clone()), "main: A keeps it");
    repick(&mut el, &mut data, &mut [&mut a, &mut b]);
    assert!(left(&a.take(), &a.id(ai)));
    let (x, y, _) = entered(&b.take(), &b.id(bi)).expect("B entered");
    assert_eq!((x, y), local(at, origin(&data, "b", 0)));
    assert_eq!(focus(&data), Some(b.server(&data, bi)));
    // AC8: A's I-beam doesn't carry over to B.
    assert_eq!(data.state.cursor_shape(), Shape::Named("default"));

    // AC2: B's null buffer gives A the pointer back.
    b.unmap(bi);
    b.step(&mut el, &mut data);
    a.step(&mut el, &mut data);
    assert!(a.take().is_empty() && b.take().is_empty(), "main: no event");
    repick(&mut el, &mut data, &mut [&mut a, &mut b]);
    assert!(left(&b.take(), &b.id(bi)));
    let (x, y, serial) = entered(&a.take(), &a.id(ai)).expect("A entered");
    assert_eq!((x, y), local(at, origin(&data, "a", 0)));
    // AC6: that enter's serial mints no token for A, which doesn't hold the keyboard.
    let kb = data.state.seat.get_keyboard().unwrap().current_focus();
    assert!(kb.and_then(|f| f.surface()) != Some(a_srv.clone()));
    let now = u32::from(SERIAL_COUNTER.next_serial());
    assert!(!may_issue(Some(serial), token_since(&data, &a_srv), now));

    // Mapped again, then closed: a tiled toplevel (a float has a tile) relayouts on close.
    b.draw(bi, None);
    b.sync(&mut el, &mut data);
    a.sync(&mut el, &mut data);
    assert!(entered(&b.take(), &b.id(bi)).is_some() && left(&a.take(), &a.id(ai)));
    b.destroy(bi);
    b.sync(&mut el, &mut data);
    a.sync(&mut el, &mut data);
    assert!(entered(&a.take(), &a.id(ai)).is_some(), "A entered");
    assert_eq!(focus(&data), Some(a_srv.clone()));

    // AC1/AC2 for an untiled dialog: it maps over its parent, then closes (`toplevel_destroyed`
    // without a relayout).
    let di = a.toplevel("a", Some(ai), None);
    a.sync(&mut el, &mut data);
    a.draw(di, Some((400, 300)));
    a.step(&mut el, &mut data);
    assert!(a.take().is_empty(), "main: no event");
    repick(&mut el, &mut data, &mut [&mut a]);
    let ev = a.take();
    assert!(left(&ev, &a.id(ai)));
    let (x, y, _) = entered(&ev, &a.id(di)).expect("the dialog entered");
    assert_eq!((x, y), local(at, origin(&data, "a", 1)));
    a.destroy(di);
    a.step(&mut el, &mut data);
    assert!(a.take().is_empty(), "main: no event");
    assert!(data.state.windows.len() == 1, "the dialog is gone");
    repick(&mut el, &mut data, &mut [&mut a]);
    assert!(entered(&a.take(), &a.id(ai)).is_some(), "A entered");
}

/// AC3, AC6: a tile that floats over a still pointer gets it once its buffer at the new rect
/// commits; a keyboard raise (`focus_by(.., Raise::User)`) of a window under a dialog moves the
/// pointer to it, and back.
#[test]
fn floating_and_raising_under_a_still_pointer_move_it() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    let mut b = Conn::new(&mut el, &mut data, ClientState::default());
    let ai = window(&mut el, &mut data, &mut a, "a");
    // B tiles next to A, drawn small at its tile's corner.
    let bi = b.toplevel("b", None, None);
    b.sync(&mut el, &mut data);
    b.draw(bi, Some((100, 100)));
    b.sync(&mut el, &mut data);
    let at = centre(&data, "a");
    put_pointer(&mut el, &mut data, at, &mut [&mut a, &mut b]);
    assert_eq!(focus(&data), Some(a.server(&data, ai)));

    // B's minimum fits no tile: it floats centred. Its old 100 px buffer there misses the
    // pointer; its buffer at the new size covers it.
    b.s[bi].top.as_ref().unwrap().set_min_size(900, 500);
    b.s[bi].surface.commit();
    b.sync(&mut el, &mut data);
    a.sync(&mut el, &mut data);
    let id = data.state.windows[1].tile.unwrap();
    assert!(data.state.layout.tile(id).unwrap().floating, "B floats");
    assert!(
        a.take().is_empty() && b.take().is_empty(),
        "its old buffer misses the pointer"
    );
    b.draw(bi, None);
    b.step(&mut el, &mut data);
    a.step(&mut el, &mut data);
    assert!(a.take().is_empty() && b.take().is_empty(), "main: no event");
    repick(&mut el, &mut data, &mut [&mut a, &mut b]);
    assert!(left(&a.take(), &a.id(ai)));
    let (x, y, _) = entered(&b.take(), &b.id(bi)).expect("B entered");
    assert_eq!((x, y), local(at, origin(&data, "b", 0)));
    b.destroy(bi);
    b.sync(&mut el, &mut data);
    a.sync(&mut el, &mut data);
    a.take();

    // A's wide dialog reaches over its neighbour C. With the pointer on both, the dialog has
    // it; raising C from the keyboard gives C the pointer, raising the dialog takes it back.
    let mut c = Conn::new(&mut el, &mut data, ClientState::default());
    let ci = window(&mut el, &mut data, &mut c, "c");
    a.sync(&mut el, &mut data);
    let di = a.toplevel("a", Some(ai), None);
    a.sync(&mut el, &mut data);
    a.draw(di, Some((1200, 200)));
    a.sync(&mut el, &mut data);
    let (dx, dy) = origin(&data, "a", 1);
    let (cx, cy) = centre(&data, "c");
    let over = (cx, dy + 100.0);
    assert!(over.0 < dx + 1200.0, "the dialog reaches over C");
    let _ = cy;
    // C, newer than A, is on top of A's dialog until the dialog is raised.
    put_pointer(&mut el, &mut data, over, &mut [&mut a, &mut c]);
    assert_eq!(focus(&data), Some(c.server(&data, ci)));
    let window_of = |d: &CalloopData, app: &str, n: usize| {
        let ws = d
            .state
            .windows
            .iter()
            .filter(|m| window_app(&m.window) == app);
        ws.map(|m| m.window.clone()).nth(n)
    };
    let dw = window_of(&data, "a", 1);
    data.state.focus_by(dw, Raise::User);
    repick(&mut el, &mut data, &mut [&mut a, &mut c]);
    assert!(left(&c.take(), &c.id(ci)));
    let (x, y, _) = entered(&a.take(), &a.id(di)).expect("the dialog entered");
    assert_eq!((x, y), local(over, origin(&data, "a", 1)));
    let cw = window_of(&data, "c", 0);
    data.state.focus_by(cw, Raise::User);
    repick(&mut el, &mut data, &mut [&mut a, &mut c]);
    assert!(left(&a.take(), &a.id(di)));
    let (x, y, _) = entered(&c.take(), &c.id(ci)).expect("C entered");
    assert_eq!((x, y), local(over, origin(&data, "c", 0)));
    let dw = window_of(&data, "a", 1);
    data.state.focus_by(dw, Raise::User);
    repick(&mut el, &mut data, &mut [&mut a, &mut c]);
    assert!(left(&c.take(), &c.id(ci)));
    assert!(
        entered(&a.take(), &a.id(di)).is_some(),
        "the dialog entered"
    );
}

/// AC4: a grab-less popup and a Top-layer surface mapping under a still pointer take it and
/// give it back when they go; over a fullscreen window a Top layer gets nothing.
#[test]
fn popups_and_layers_under_a_still_pointer_take_it() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    let privileged = ClientState {
        privileged: true,
        ..Default::default()
    };
    let mut l = Conn::new(&mut el, &mut data, privileged);
    let ai = window(&mut el, &mut data, &mut a, "a");
    let at = centre(&data, "a");
    put_pointer(&mut el, &mut data, at, &mut [&mut a, &mut l]);
    let (lx, ly) = local(at, origin(&data, "a", 0));

    let pi = a.popup(ai, (lx as i32 - 50, ly as i32 - 50), (100, 100));
    a.sync(&mut el, &mut data);
    a.draw(pi, None);
    a.step(&mut el, &mut data);
    assert!(a.take().is_empty(), "main: no event");
    repick(&mut el, &mut data, &mut [&mut a]);
    let ev = a.take();
    assert!(left(&ev, &a.id(ai)));
    assert_eq!(
        entered(&ev, &a.id(pi)).map(|e| (e.0, e.1)),
        Some((50.0, 50.0))
    );
    a.destroy(pi);
    a.step(&mut el, &mut data);
    assert!(a.take().is_empty(), "main: no event");
    repick(&mut el, &mut data, &mut [&mut a]);
    assert!(entered(&a.take(), &a.id(ai)).is_some(), "A entered");

    let menu = |l: &mut Conn| l.layer((at.0 as i32 - 50, at.1 as i32 - 50), (100, 100));
    let li = menu(&mut l);
    l.sync(&mut el, &mut data);
    l.draw(li, None);
    l.step(&mut el, &mut data);
    a.step(&mut el, &mut data);
    assert!(a.take().is_empty() && l.take().is_empty(), "main: no event");
    repick(&mut el, &mut data, &mut [&mut a, &mut l]);
    assert!(left(&a.take(), &a.id(ai)));
    let entry = entered(&l.take(), &l.id(li)).map(|e| (e.0, e.1));
    assert_eq!(entry, Some((50.0, 50.0)));
    l.unmap(li);
    l.step(&mut el, &mut data);
    a.step(&mut el, &mut data);
    assert!(a.take().is_empty() && l.take().is_empty(), "main: no event");
    repick(&mut el, &mut data, &mut [&mut a, &mut l]);
    assert!(left(&l.take(), &l.id(li)));
    assert!(entered(&a.take(), &a.id(ai)).is_some(), "A entered");
    l.draw(li, None);
    l.sync(&mut el, &mut data);
    a.sync(&mut el, &mut data);
    assert!(left(&a.take(), &a.id(ai)) && entered(&l.take(), &l.id(li)).is_some());
    l.destroy(li);
    l.step(&mut el, &mut data);
    a.step(&mut el, &mut data);
    assert!(a.take().is_empty(), "main: no event");
    repick(&mut el, &mut data, &mut [&mut a, &mut l]);
    assert!(entered(&a.take(), &a.id(ai)).is_some(), "A entered");

    // Fullscreen A hides the Top layer: a menu mapping there gets nothing.
    let id = data.state.windows[0].tile.unwrap();
    assert!(data.state.layout.fullscreen(id));
    data.state.apply_layout();
    a.sync(&mut el, &mut data);
    a.draw(ai, None);
    a.sync(&mut el, &mut data);
    a.take();
    let li = menu(&mut l);
    l.sync(&mut el, &mut data);
    l.draw(li, None);
    l.sync(&mut el, &mut data);
    a.sync(&mut el, &mut data);
    assert!(l.take().is_empty() && a.take().is_empty());
    assert_eq!(focus(&data), Some(a.server(&data, ai)));
}

/// AC5, AC6, AC8: locked, only the lock surface gets the pointer, at its first buffer; a window
/// mapping meanwhile gets nothing; after unlock the window under the pointer gets it, and the
/// keyboard is as unlock leaves it.
#[test]
fn the_lock_surface_takes_a_still_pointer_and_unlock_gives_it_back() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    let locker = ClientState {
        locker: true,
        ..Default::default()
    };
    let mut k = Conn::new(&mut el, &mut data, locker);
    let ai = window(&mut el, &mut data, &mut a, "a");
    let at = centre(&data, "a");
    put_pointer(&mut el, &mut data, at, &mut [&mut a, &mut k]);
    data.state.cursor.request = crate::cursor::Request::Named("text");

    let lock = k.c.locks.as_ref().unwrap().lock(&k.queue.handle(), ());
    k.sync(&mut el, &mut data);
    a.sync(&mut el, &mut data);
    assert!(data.state.lock.active);
    assert!(left(&a.take(), &a.id(ai)));
    assert_eq!(focus(&data), None, "isolate_lock_input");
    // `pointer_focus`'s lock rule takes a live lock surface, drawn yet or not: it has the
    // pointer from its creation, so by its first buffer, without motion.
    let si = k.lock_surface(&lock);
    k.step(&mut el, &mut data);
    assert!(k.take().is_empty(), "main: no event");
    repick(&mut el, &mut data, &mut [&mut k]);
    let (x, y, _) = entered(&k.take(), &k.id(si)).expect("the lock surface entered");
    assert_eq!((x, y), at);
    k.sync(&mut el, &mut data);
    k.draw(si, None);
    k.sync(&mut el, &mut data);
    assert!(k.take().is_empty(), "its buffer changes nothing");
    assert_eq!(focus(&data), Some(k.server(&data, si)));
    // AC8: the lock's rule: its client's request, `default` until it makes one.
    assert_eq!(data.state.cursor_shape(), Shape::Named("default"));

    // A window mapping and drawing while locked gets nothing.
    let mut b = Conn::new(&mut el, &mut data, ClientState::default());
    let bi = b.toplevel("b", None, Some((900, 500)));
    b.sync(&mut el, &mut data);
    b.draw(bi, None);
    b.sync(&mut el, &mut data);
    a.draw(ai, None);
    a.sync(&mut el, &mut data);
    k.sync(&mut el, &mut data);
    assert!(b.take().is_empty() && a.take().is_empty() && k.take().is_empty());

    let output = data.state.space.outputs().next().unwrap().clone();
    let epoch = data.state.lock.epoch;
    data.state.lock_presented(&output, epoch);
    k.sync(&mut el, &mut data);
    lock.unlock_and_destroy();
    k.step(&mut el, &mut data);
    b.step(&mut el, &mut data);
    assert!(!data.state.lock.active);
    assert!(b.take().is_empty(), "main: no event");
    let kb = data.state.seat.get_keyboard().unwrap().current_focus();
    assert!(kb.is_none(), "unlock leaves the keyboard with nobody");
    repick(&mut el, &mut data, &mut [&mut a, &mut b, &mut k]);
    // B (the float that mapped while locked) is on top at the pointer.
    let (x, y, _) = entered(&b.take(), &b.id(bi)).expect("B entered");
    assert_eq!((x, y), local(at, origin(&data, "b", 0)));
    assert!(a.take().is_empty());
}

/// AC7: a client that commits on every enter and motion hears nothing more from those commits;
/// a dialog resize that moves its surface under a still pointer sends exactly one motion.
#[test]
fn commits_with_nothing_moved_send_no_pointer_events() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    let ai = window(&mut el, &mut data, &mut a, "a");
    let at = centre(&data, "a");
    let size = a.c.size[ai].unwrap();
    let buffer = a.draw(ai, Some(size));
    a.c.echo = Some((a.s[ai].surface.clone(), buffer));
    put_pointer(&mut el, &mut data, at, &mut [&mut a]);
    data.state
        .pointer_motion((at.0 + 3.0, at.1 + 3.0).into(), 1);
    for _ in 0..50 {
        a.sync(&mut el, &mut data);
    }
    let moved = local((at.0 + 3.0, at.1 + 3.0), origin(&data, "a", 0));
    assert_eq!(a.take(), vec![Ev::Motion(moved.0, moved.1)]);
    a.c.echo = None;

    // A dialog centred over A grows: navi re-centres it, so the pointer's local position on it
    // changes, once.
    let di = a.toplevel("a", Some(ai), None);
    a.sync(&mut el, &mut data);
    a.draw(di, Some((400, 300)));
    a.sync(&mut el, &mut data);
    assert!(entered(&a.take(), &a.id(di)).is_some());
    a.draw(di, Some((500, 360)));
    for _ in 0..50 {
        a.sync(&mut el, &mut data);
    }
    let moved = local((at.0 + 3.0, at.1 + 3.0), origin(&data, "a", 1));
    assert_eq!(a.take(), vec![Ev::Motion(moved.0, moved.1)]);
}

/// AC10: outputs come and go under a still pointer: after each, the pointer's focus is what
/// `pointer_focus` gives at its location, and nothing panics when its output is gone.
#[test]
fn hotplug_keeps_pointer_focus_on_what_is_under_it() {
    use smithay::output::{Mode as OutputMode, PhysicalProperties, Subpixel};
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let add = |data: &mut CalloopData, name: &str, x: i32| {
        let o = Output::new(
            name.into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        let mode = OutputMode {
            size: (1280, 720).into(),
            refresh: 60_000,
        };
        o.change_current_state(Some(mode), None, None, Some((x, 0).into()));
        o.create_global::<Navi>(&data.state.display_handle);
        data.state.space.map_output(&o, (x, 0));
        data.state.add_output(&o, 0);
        data.state.apply_layout();
        o
    };
    let first = data.state.space.outputs().next().unwrap().clone();
    add(&mut data, "two", 1280);
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    // On output two: new windows go nearest the pointer.
    data.state.pointer_motion((1900.0, 360.0).into(), 0);
    let ai = window(&mut el, &mut data, &mut a, "a");
    let at = centre(&data, "a");
    assert!(at.0 > 1280.0, "on output two");
    put_pointer(&mut el, &mut data, at, &mut [&mut a]);
    let settled = |data: &CalloopData| {
        let p = pointer(data);
        let want = data
            .state
            .pointer_focus(p.current_location())
            .map(|(s, _)| s);
        p.current_focus() == want
    };
    assert!(settled(&data));
    add(&mut data, "three", 2560);
    a.sync(&mut el, &mut data);
    a.draw(ai, None);
    a.sync(&mut el, &mut data);
    assert!(settled(&data));
    data.state.remove_output(&first);
    a.sync(&mut el, &mut data);
    a.draw(ai, None);
    a.sync(&mut el, &mut data);
    assert!(settled(&data));
    assert_eq!(pointer(&data).current_location(), at.into(), "never warped");
    // The pointer's own output goes: focus is whatever is there, likely nothing.
    let two = data
        .state
        .space
        .outputs()
        .find(|o| o.name() == "two")
        .unwrap()
        .clone();
    data.state.remove_output(&two);
    a.sync(&mut el, &mut data);
    assert!(settled(&data));
}

/// AC9 (job 028's rule), AC8: A's dialog, raised over its neighbour C, covers the edge zones
/// between them. Re-picks as C's layout changes keep the pointer on the dialog; once the dialog
/// unmaps, the zone is under the pointer and the cursor is the zone's.
#[test]
fn a_repick_never_picks_what_the_dialog_covers() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    let mut c = Conn::new(&mut el, &mut data, ClientState::default());
    let ai = window(&mut el, &mut data, &mut a, "a");
    let ci = window(&mut el, &mut data, &mut c, "c");
    let di = a.toplevel("a", Some(ai), None);
    a.sync(&mut el, &mut data);
    a.draw(di, Some((1200, 200)));
    a.sync(&mut el, &mut data);
    let dialog = data.state.windows[2].window.clone();
    data.state.focus_by(Some(dialog), Raise::User);
    let (dx, dy) = origin(&data, "a", 1);
    let y = dy as i32 + 100;
    let (p, hit) = (dx as i32..dx as i32 + 1200)
        .find_map(|x| Some(((x, y), data.state.layout.hit((x, y))?)))
        .expect("a zone under the dialog");
    let at = (f64::from(p.0), f64::from(p.1));
    put_pointer(&mut el, &mut data, at, &mut [&mut a, &mut c]);
    let d_srv = a.server(&data, di);
    assert_eq!(
        focus(&data),
        Some(d_srv.clone()),
        "the dialog covers {hit:?}"
    );
    assert_eq!(data.state.cursor_shape(), Shape::Named("default"));

    // C shrinks from the bottom and redraws; then grows back. Every re-pick keeps the dialog.
    let cid = data.state.windows[1].tile.unwrap();
    for side in [Nudge::Shrink, Nudge::Grow] {
        assert!(
            data.state
                .layout
                .keyboard_adjust(cid, Side::Bottom, side)
                .is_ok()
        );
        data.state.apply_layout();
        c.sync(&mut el, &mut data);
        c.draw(ci, None);
        c.sync(&mut el, &mut data);
        a.sync(&mut el, &mut data);
        assert_eq!(focus(&data), Some(d_srv.clone()));
        assert!(data.state.zone_at(at.into()).is_none());
        assert_eq!(data.state.cursor_shape(), Shape::Named("default"));
    }
    assert!(
        a.take().is_empty() && c.take().is_empty(),
        "no pointer events at all"
    );

    // AC8: the dialog unmaps; the zone takes the pointer (no client gets it) and its cursor.
    a.unmap(di);
    a.step(&mut el, &mut data);
    assert_eq!(focus(&data), Some(d_srv), "main: still the dialog");
    repick(&mut el, &mut data, &mut [&mut a, &mut c]);
    assert!(left(&a.take(), &a.id(di)));
    assert!(c.take().is_empty());
    assert_eq!(focus(&data), None);
    let zone = data.state.zone_at(at.into()).expect("the zone");
    assert_eq!(data.state.cursor_shape(), Shape::Named(zone.cursor()));
    assert_ne!(zone.cursor(), "default");
}

// Job 032: client cursor surfaces.

/// Moves the pointer to `at` and gives `c`'s enter serial on surface `i`; the others' events are
/// dropped.
fn enter(
    el: &mut Loop,
    data: &mut CalloopData,
    at: (f64, f64),
    c: &mut Conn,
    i: usize,
    others: &mut [&mut Conn],
) -> u32 {
    data.state.pointer_motion(at.into(), 0);
    c.sync(el, data);
    for o in others {
        o.sync(el, data);
        o.take();
    }
    entered(&c.take(), &c.id(i)).expect("entered").2
}

/// A point in navi's strip above window `app`.
fn strip(data: &CalloopData, app: &str) -> (f64, f64) {
    let (x, _) = centre(data, app);
    let w = &data
        .state
        .windows
        .iter()
        .find(|m| window_app(&m.window) == app);
    let top = data
        .state
        .space
        .element_geometry(&w.unwrap().window)
        .unwrap()
        .loc
        .y;
    (top - 40..top)
        .rev()
        .map(|y| (x, f64::from(y)))
        .find(|&p| data.state.zone_at(p.into()).is_some())
        .expect("a zone above the window")
}

/// One frame on the test output as a backend without a theme draws it, then its frame
/// callbacks, which `c` reads: the frame's cursor elements.
fn frame(el: &mut Loop, data: &mut CalloopData, c: &mut Conn) -> usize {
    use smithay::backend::renderer::element::{Element, Kind};
    let output = data.state.space.outputs().next().unwrap().clone();
    let arrow = match data.state.cursor_shape() {
        Shape::Client(s) => crate::render::Arrow::Client(s),
        _ => crate::render::Arrow::None,
    };
    let drawn = arrow.surface();
    let cursors = crate::render::elements(
        &mut smithay::backend::renderer::test::DummyRenderer,
        &data.state,
        &output,
        arrow,
        &mut None,
        &mut crate::render::Solids::default(),
    )
    .iter()
    .filter(|e| e.kind() == Kind::Cursor)
    .count();
    crate::render::send_frames(&mut data.state, &output, drawn.as_ref());
    c.step(el, data);
    cursors
}

fn client(s: &WlSurface, hot: (i32, i32)) -> Shape<crate::state::CursorSurface> {
    Shape::Client((s.clone(), hot.into()))
}

/// AC3: a focused client's 24×24 cursor surface at (4, 4) is what navi draws; a pointer focus
/// change forgets it; its offset moves the hotspot; destroyed, it is the arrow.
#[test]
fn client_ac3_cursor_surface_follows_pointer_focus() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    let mut b = Conn::new(&mut el, &mut data, ClientState::default());
    let ai = window(&mut el, &mut data, &mut a, "a");
    let bi = window(&mut el, &mut data, &mut b, "b");
    let (at, bt) = (centre(&data, "a"), centre(&data, "b"));
    let serial = enter(&mut el, &mut data, at, &mut a, ai, &mut [&mut b]);
    let cs = a.cursor(serial, (24, 24), (4, 4), 0);
    a.sync(&mut el, &mut data);
    let srv = a.server_of(&data, &cs);
    assert_eq!(data.state.cursor_shape(), client(&srv, (4, 4)));

    // Onto B: the arrow, and A's surface is no longer held.
    enter(&mut el, &mut data, bt, &mut b, bi, &mut [&mut a]);
    assert_eq!(data.state.cursor_shape(), Shape::Named("default"));
    assert_eq!(data.state.cursor.request, crate::cursor::Request::default());

    // Back on A, which sets it again, then moves its buffer by (1, 2): the hotspot follows.
    let serial = enter(&mut el, &mut data, at, &mut a, ai, &mut [&mut b]);
    a.c.pointer
        .as_ref()
        .unwrap()
        .set_cursor(serial, Some(&cs), 4, 4);
    let buf = a.buffer(24, 24);
    cs.attach(Some(&buf), 1, 2);
    cs.commit();
    a.sync(&mut el, &mut data);
    assert_eq!(data.state.cursor_shape(), client(&srv, (3, 2)));
    // An offset that would overflow the hotspot saturates, and is outside: the arrow.
    cs.attach(Some(&buf), i32::MIN, i32::MAX);
    cs.commit();
    a.sync(&mut el, &mut data);
    assert_eq!(data.state.cursor_shape(), Shape::Named("default"));
    cs.attach(Some(&buf), i32::MIN, i32::MAX);
    cs.commit();
    a.sync(&mut el, &mut data);
    assert_eq!(data.state.cursor_shape(), Shape::Named("default"));

    // Destroyed while set: no panic, the arrow, and a frame draws no cursor.
    cs.destroy();
    a.sync(&mut el, &mut data);
    assert_eq!(data.state.cursor_shape(), Shape::Named("default"));
    assert_eq!(frame(&mut el, &mut data, &mut a), 0);
}

/// AC4: requests from a client without the pointer or with a stale serial change nothing;
/// a 512×512 surface and one with its hotspot outside are the arrow.
#[test]
fn client_ac4_unfocused_stale_and_oversized_cursors_are_refused() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    let mut b = Conn::new(&mut el, &mut data, ClientState::default());
    let ai = window(&mut el, &mut data, &mut a, "a");
    let bi = window(&mut el, &mut data, &mut b, "b");
    let (at, bt) = (centre(&data, "a"), centre(&data, "b"));
    let stale = enter(&mut el, &mut data, at, &mut a, ai, &mut [&mut b]);
    a.c.pointer.as_ref().unwrap().set_cursor(stale, None, 0, 0);
    a.sync(&mut el, &mut data);
    assert_eq!(data.state.cursor.request, crate::cursor::Request::Hidden);
    // B never had the pointer.
    b.cursor(stale, (24, 24), (4, 4), 0);
    b.sync(&mut el, &mut data);
    assert_eq!(data.state.cursor.request, crate::cursor::Request::Hidden);
    // A, back after B, with its first serial.
    enter(&mut el, &mut data, bt, &mut b, bi, &mut [&mut a]);
    let serial = enter(&mut el, &mut data, at, &mut a, ai, &mut [&mut b]);
    assert_ne!(serial, stale);
    a.cursor(stale, (24, 24), (4, 4), 0);
    a.sync(&mut el, &mut data);
    assert_eq!(data.state.cursor.request, crate::cursor::Request::default());

    for (size, hot) in [
        ((512, 512), (0, 0)),
        ((24, 24), (100, 100)),
        ((24, 24), (-1, 0)),
    ] {
        let cs = a.cursor(serial, size, hot, 1);
        a.sync(&mut el, &mut data);
        let srv = a.server_of(&data, &cs);
        assert_eq!(
            data.state.cursor.request,
            crate::cursor::Request::Surface(srv)
        );
        assert_eq!(
            data.state.cursor_shape(),
            Shape::Named("default"),
            "{size:?} {hot:?}"
        );
        assert_eq!(frame(&mut el, &mut data, &mut a), 0);
        cs.destroy();
    }
    // AC7: none of them was drawn, so none got a frame callback.
    a.sync(&mut el, &mut data);
    assert!(a.c.done.is_empty(), "{:?}", a.c.done);
}

/// AC5: over a zone, a Top layer, during a drag and locked, a stored client surface is never
/// what is drawn, even if it outlived the focus change that clears it.
#[test]
fn client_ac5_never_over_navi_ui_drags_or_the_lock() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    let privileged = ClientState {
        privileged: true,
        ..Default::default()
    };
    let mut l = Conn::new(&mut el, &mut data, privileged);
    let ai = window(&mut el, &mut data, &mut a, "a");
    let at = centre(&data, "a");
    let serial = enter(&mut el, &mut data, at, &mut a, ai, &mut [&mut l]);
    let cs = a.cursor(serial, (24, 24), (4, 4), 0);
    a.sync(&mut el, &mut data);
    let srv = a.server_of(&data, &cs);
    assert_eq!(data.state.cursor_shape(), client(&srv, (4, 4)));
    let stored = crate::cursor::Request::Surface(srv.clone());

    // The strip: its own cursor, before and after forcing the request back.
    let zone = strip(&data, "a");
    put_pointer(&mut el, &mut data, zone, &mut [&mut a, &mut l]);
    let grab = Shape::Named(data.state.zone_at(zone.into()).unwrap().cursor());
    assert_eq!(data.state.cursor_shape(), grab);
    data.state.cursor.request = stored.clone();
    assert_eq!(data.state.cursor_shape(), grab);
    assert_eq!(frame(&mut el, &mut data, &mut a), 0);

    // A drag from the strip: its cursor.
    data.state.pointer_button(0x110, true, true, 1);
    assert!(data.state.drag.is_some(), "a drag from the strip");
    data.state.cursor.request = stored.clone();
    let drag = Shape::Named(data.state.drag.as_ref().unwrap().cursor());
    assert_eq!(data.state.cursor_shape(), drag);
    assert_eq!(frame(&mut el, &mut data, &mut a), 0);
    data.state.pointer_button(0x110, true, false, 2);
    assert!(data.state.drag.is_none());

    // A Top layer over A: the layer client's cursor (`default`), never A's surface.
    let li = l.layer((at.0 as i32 - 50, at.1 as i32 - 50), (100, 100));
    l.sync(&mut el, &mut data);
    l.draw(li, None);
    l.sync(&mut el, &mut data);
    put_pointer(&mut el, &mut data, at, &mut [&mut a, &mut l]);
    assert_eq!(focus(&data), Some(l.server(&data, li)));
    data.state.cursor.request = stored.clone();
    assert_eq!(data.state.cursor_shape(), Shape::Named("default"));
    assert_eq!(frame(&mut el, &mut data, &mut a), 0);

    // Locked: job 025's rule (the lock client's request; a surface is `default`), and a lock
    // frame has no cursor even when handed one.
    data.state.lock.active = true;
    data.state.cursor.request = stored;
    assert_eq!(data.state.cursor_shape(), Shape::Named("default"));
    let output = data.state.space.outputs().next().unwrap().clone();
    let elements = crate::render::elements(
        &mut smithay::backend::renderer::test::DummyRenderer,
        &data.state,
        &output,
        crate::render::Arrow::Client((srv, (4, 4).into())),
        &mut None,
        &mut crate::render::Solids::default(),
    );
    assert!(
        elements.is_empty(),
        "{} elements while locked",
        elements.len()
    );
    a.sync(&mut el, &mut data);
    assert!(a.c.done.is_empty(), "{:?}", a.c.done);
}

/// AC6 (job 025's AC4 on the new `pick`): a null surface hides the pointer only over the client
/// that asked.
#[test]
fn client_ac6_hidden_only_over_the_requesting_client() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    let mut b = Conn::new(&mut el, &mut data, ClientState::default());
    let ai = window(&mut el, &mut data, &mut a, "a");
    let bi = window(&mut el, &mut data, &mut b, "b");
    let (at, bt) = (centre(&data, "a"), centre(&data, "b"));
    let hide = |a: &mut Conn, serial| a.c.pointer.as_ref().unwrap().set_cursor(serial, None, 0, 0);

    let serial = enter(&mut el, &mut data, at, &mut a, ai, &mut [&mut b]);
    hide(&mut a, serial);
    a.sync(&mut el, &mut data);
    assert_eq!(data.state.cursor_shape(), Shape::Hidden);
    let zone = strip(&data, "a");
    put_pointer(&mut el, &mut data, zone, &mut [&mut a, &mut b]);
    let z = data.state.zone_at(zone.into()).unwrap().cursor();
    assert_eq!(data.state.cursor_shape(), Shape::Named(z));
    assert_ne!(z, "default");
    let serial = enter(&mut el, &mut data, at, &mut a, ai, &mut [&mut b]);
    assert_eq!(data.state.cursor_shape(), Shape::Named("default"));
    hide(&mut a, serial);
    a.sync(&mut el, &mut data);
    assert_eq!(data.state.cursor_shape(), Shape::Hidden);
    enter(&mut el, &mut data, bt, &mut b, bi, &mut [&mut a]);
    assert_eq!(data.state.cursor_shape(), Shape::Named("default"));
}

/// AC7: a drawn cursor surface gets its frame callback after the frame; over a zone it gets
/// none.
#[test]
fn client_ac7_frame_callbacks_only_when_drawn() {
    let (mut el, mut data) = crate::xwayland::tests::harness();
    let mut a = Conn::new(&mut el, &mut data, ClientState::default());
    let ai = window(&mut el, &mut data, &mut a, "a");
    let at = centre(&data, "a");
    let serial = enter(&mut el, &mut data, at, &mut a, ai, &mut []);
    let cs = a.cursor(serial, (24, 24), (4, 4), 7);
    a.sync(&mut el, &mut data);
    assert!(a.c.done.is_empty());
    assert_eq!(frame(&mut el, &mut data, &mut a), 1);
    assert_eq!(a.c.done, [7]);

    // A new frame request; then the pointer goes to the strip before navi draws.
    cs.frame(&a.queue.handle(), 8);
    cs.commit();
    a.sync(&mut el, &mut data);
    let zone = strip(&data, "a");
    put_pointer(&mut el, &mut data, zone, &mut [&mut a]);
    assert_eq!(frame(&mut el, &mut data, &mut a), 0);
    assert_eq!(a.c.done, [7]);
}
