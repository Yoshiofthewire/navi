//! X11 compatibility through Smithay's rootless XWayland server and window manager.

use crate::{
    CalloopData,
    layout::{Layout, WinId},
    state::{Managed, Navi, window_app},
};
use smithay::wayland::selection::{
    SelectionTarget,
    data_device::{
        clear_data_device_selection, current_data_device_selection_userdata,
        request_data_device_client_selection, set_data_device_selection,
    },
};
use smithay::{
    delegate_xwayland_shell,
    desktop::Window,
    reexports::{calloop::LoopHandle, wayland_server::protocol::wl_surface::WlSurface},
    utils::{Logical, Rectangle},
    wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    xwayland::{
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
        xwm::{Reorder, ResizeEdge, WmWindowProperty, XwmId},
    },
};
use std::{os::fd::OwnedFd, process::Stdio};
use tracing::{info, warn};

pub fn start(handle: &LoopHandle<'static, CalloopData>, state: &Navi) -> Option<String> {
    let (server, client) = match XWayland::spawn(
        &state.display_handle,
        None,
        std::iter::empty::<(String, String)>(),
        false,
        Stdio::null(),
        Stdio::inherit(),
        |_| {},
    ) {
        Ok(pair) => pair,
        Err(e) => {
            warn!("XWayland unavailable; native Wayland remains available: {e}");
            return None;
        }
    };
    let xdisplay = format!(":{}", server.display_number());
    let h = handle.clone();
    match handle.insert_source(server, move |event, _, data| match event {
        XWaylandEvent::Ready {
            x11_socket,
            display_number,
        } => match X11Wm::start_wm(h.clone(), x11_socket, client.clone()) {
            Ok(wm) => {
                data.state.xwm = Some(wm);
                info!(display = format!(":{display_number}"), "XWayland ready");
            }
            Err(e) => warn!("XWayland window manager failed: {e}"),
        },
        XWaylandEvent::Error => warn!("XWayland startup failed; native Wayland remains available"),
    }) {
        Ok(_) => {
            info!(display = %xdisplay, "XWayland display allocated");
            Some(xdisplay)
        }
        Err(e) => {
            warn!("XWayland event source: {e}");
            None
        }
    }
}

impl Navi {
    pub fn map_x11(&mut self, x: X11Surface) {
        if x.wl_surface().is_none()
            || self
                .windows
                .iter()
                .any(|m| m.window.x11_surface() == Some(&x))
        {
            return;
        }
        let window = Window::new_x11_window(x.clone());
        let x11_parent = x.is_transient_for().or_else(|| {
            if !x.is_override_redirect() {
                return None;
            }
            match self.seat.get_keyboard().and_then(|k| k.current_focus()) {
                Some(crate::focus::Focus::X11(parent)) => Some(parent.window_id()),
                _ => self
                    .window_under(self.pointer_location())
                    .and_then(|(w, _)| w.x11_surface())
                    .map(|p| p.window_id()),
            }
        });
        // Unsolicited X11 menus must not float over unrelated native applications.
        if x.is_override_redirect() && x11_parent.is_none() {
            return;
        }
        let floating = x.is_override_redirect() || x11_parent.is_some();
        let mut tile = None;
        if !floating {
            let app = window_app(&window);
            if let Some((id, step)) = self.layout.place(&app, self.pointer_px()) {
                self.layout.set_decorated(id, !x.is_decorated());
                if x.is_fullscreen() {
                    self.layout.fullscreen(id);
                } else if x.is_maximized() {
                    self.layout.maximize(id);
                }
                tile = Some(id);
                info!(app, id, ?step, "X11 placed");
                self.memory.sync(&self.layout);
            }
        }
        self.windows.push(Managed {
            window: window.clone(),
            tile,
            last_size: x.geometry().size,
            clipped: None,
            refitted: None,
            refit_queued: false,
            want_fullscreen: false,
            escaped: false,
            x11_parent,
        });
        self.apply_layout();
        if !x.is_override_redirect() && self.space.element_location(&window).is_some() {
            self.focus(Some(window));
        }
    }

    fn remove_x11(&mut self, x: &X11Surface) {
        let Some(at) = self
            .windows
            .iter()
            .position(|m| m.window.x11_surface() == Some(x))
        else {
            return;
        };
        let m = self.windows.remove(at);
        self.space.unmap_elem(&m.window);
        if let Some(id) = m.tile {
            self.layout.remove(id);
            info!(id, "X11 closed; its cells stay empty");
        }
        self.apply_layout();
        self.refresh_toplevels();
    }

    /// Transient dialogs follow their parent; override-redirect menus keep their X11 geometry.
    pub fn recenter_x11(&mut self) {
        let floating: Vec<_> = self
            .windows
            .iter()
            .filter(|m| m.tile.is_none())
            .filter_map(|m| {
                Some((
                    m.window.clone(),
                    m.window.x11_surface()?.clone(),
                    m.x11_parent,
                ))
            })
            .collect();
        for (window, x, parent_id) in floating {
            let parent = parent_id.and_then(|id| {
                self.windows
                    .iter()
                    .find(|m| m.window.x11_surface().is_some_and(|p| p.window_id() == id))
            });
            if x.is_override_redirect() && parent.is_none() {
                self.space.unmap_elem(&window);
                continue;
            }
            let over = match parent {
                Some(m) => self.space.element_geometry(&m.window),
                None => self
                    .main_output()
                    .and_then(|o| self.space.output_geometry(&o)),
            };
            let Some(over) = over else {
                self.space.unmap_elem(&window);
                continue;
            };
            let mut geo = x.geometry();
            if !x.is_override_redirect() {
                geo.loc = over.loc + (over.size - geo.size).downscale(2).to_point();
                let _ = x.configure(geo);
            }
            self.space.map_element(window, geo.loc, false);
        }
    }

    fn x11_request(&mut self, x: &X11Surface, action: fn(&mut Layout, WinId) -> bool) {
        if let Some(id) = self
            .windows
            .iter()
            .find(|m| m.window.x11_surface() == Some(x))
            .and_then(|m| m.tile)
            && action(&mut self.layout, id)
        {
            self.apply_layout();
        }
    }
}

impl XWaylandShellHandler for Navi {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell
    }
    fn surface_associated(&mut self, _: XwmId, _: WlSurface, x: X11Surface) {
        self.map_x11(x);
    }
}
impl XwmHandler for Navi {
    fn xwm_state(&mut self, _: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().expect("live XWayland WM")
    }
    fn new_window(&mut self, _: XwmId, _: X11Surface) {}
    fn new_override_redirect_window(&mut self, _: XwmId, _: X11Surface) {}
    fn map_window_request(&mut self, _: XwmId, x: X11Surface) {
        if let Err(e) = x.set_mapped(true) {
            warn!("X11 map: {e}");
            return;
        }
        self.map_x11(x);
    }
    fn map_window_notify(&mut self, _: XwmId, x: X11Surface) {
        self.map_x11(x);
    }
    fn mapped_override_redirect_window(&mut self, _: XwmId, x: X11Surface) {
        self.map_x11(x);
    }
    fn unmapped_window(&mut self, _: XwmId, x: X11Surface) {
        self.remove_x11(&x);
        if !x.is_override_redirect() {
            let _ = x.set_mapped(false);
        }
    }
    fn destroyed_window(&mut self, _: XwmId, x: X11Surface) {
        self.remove_x11(&x);
    }
    fn configure_request(
        &mut self,
        _: XwmId,
        x: X11Surface,
        _: Option<i32>,
        _: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _: Option<Reorder>,
    ) {
        if let Some(m) = self
            .windows
            .iter()
            .find(|m| m.window.x11_surface() == Some(&x))
        {
            if m.tile.is_some() {
                self.apply_layout();
            } else {
                let mut geo = x.geometry();
                if let Some(w) = w {
                    geo.size.w = (w as i32).clamp(1, 8192);
                }
                if let Some(h) = h {
                    geo.size.h = (h as i32).clamp(1, 8192);
                }
                let _ = x.configure(geo);
                self.recenter_x11();
            }
        } else {
            let _ = x.configure(None);
        }
    }
    fn configure_notify(
        &mut self,
        _: XwmId,
        x: X11Surface,
        _: Rectangle<i32, Logical>,
        _: Option<u32>,
    ) {
        if x.is_override_redirect() {
            self.recenter_x11();
        }
    }
    fn property_notify(&mut self, _: XwmId, _: X11Surface, _: WmWindowProperty) {
        self.refresh_toplevels();
    }
    fn maximize_request(&mut self, _: XwmId, x: X11Surface) {
        self.x11_request(&x, Layout::maximize);
    }
    fn unmaximize_request(&mut self, _: XwmId, x: X11Surface) {
        self.x11_request(&x, Layout::unmaximize);
    }
    fn fullscreen_request(&mut self, _: XwmId, x: X11Surface) {
        let id = self
            .windows
            .iter()
            .find(|m| m.window.x11_surface() == Some(&x))
            .and_then(|m| m.tile);
        if !self.refuse_escaped(id) {
            self.x11_request(&x, Layout::fullscreen);
        }
    }
    fn unfullscreen_request(&mut self, _: XwmId, x: X11Surface) {
        self.x11_request(&x, Layout::unfullscreen);
    }
    fn minimize_request(&mut self, _: XwmId, x: X11Surface) {
        self.x11_request(&x, Layout::minimize);
    }
    fn unminimize_request(&mut self, _: XwmId, _: X11Surface) {} // Hidden apps cannot steal focus.
    fn resize_request(&mut self, _: XwmId, _: X11Surface, _: u32, _: ResizeEdge) {}
    fn move_request(&mut self, _: XwmId, _: X11Surface, _: u32) {}
    fn allow_selection_access(&mut self, xwm: XwmId, ty: SelectionTarget) -> bool {
        !self.lock.active
            && ty == SelectionTarget::Clipboard
            && matches!(
            self.seat.get_keyboard().and_then(|kb| kb.current_focus()),
            Some(crate::focus::Focus::X11(x)) if x.xwm_id() == Some(xwm))
    }
    fn send_selection(&mut self, _: XwmId, ty: SelectionTarget, mime: String, fd: OwnedFd) {
        if ty == SelectionTarget::Clipboard
            && let Err(e) = request_data_device_client_selection(&self.seat, mime, fd)
        {
            warn!("Wayland clipboard transfer: {e}");
        }
    }
    fn new_selection(&mut self, id: XwmId, ty: SelectionTarget, mut mime_types: Vec<String>) {
        if XwmHandler::allow_selection_access(self, id, ty) {
            // MIME names identify the transfer target: reject unsafe names rather than altering
            // them. Leave room for the complete Wayland offer event in the 4096-byte buffer.
            mime_types.retain(|mime| mime.len() <= 2048 && !mime.contains('\0'));
            set_data_device_selection(&self.display_handle, &self.seat, mime_types, id);
        }
    }
    fn cleared_selection(&mut self, id: XwmId, ty: SelectionTarget) {
        let ours =
            current_data_device_selection_userdata(&self.seat).is_some_and(|owner| *owner == id);
        if ty == SelectionTarget::Clipboard && ours {
            clear_data_device_selection(&self.display_handle, &self.seat);
        }
    }
    fn disconnected(&mut self, id: XwmId) {
        XwmHandler::cleared_selection(self, id, SelectionTarget::Clipboard);
        let windows: Vec<_> = self
            .windows
            .iter()
            .filter_map(|m| m.window.x11_surface().cloned())
            .collect();
        for x in windows {
            self.remove_x11(&x);
        }
        warn!("XWayland disconnected; restart Navi to run X11 apps again");
    }
}
// The event loop owns CalloopData; Wayland protocol dispatch owns Navi. Both reach the same WM.
macro_rules! forward_xwm {
    ($(fn $name:ident($($arg:ident: $ty:ty),*);)*) => {
        impl XwmHandler for CalloopData {
            fn xwm_state(&mut self, id: XwmId) -> &mut X11Wm { XwmHandler::xwm_state(&mut self.state, id) }
            fn allow_selection_access(&mut self, id: XwmId, ty: SelectionTarget) -> bool {
                XwmHandler::allow_selection_access(&mut self.state, id, ty)
            }
            $(fn $name(&mut self, $($arg: $ty),*) { XwmHandler::$name(&mut self.state, $($arg),*); })*
        }
    };
}
forward_xwm! {
    fn new_window(id: XwmId, x: X11Surface);
    fn new_override_redirect_window(id: XwmId, x: X11Surface);
    fn map_window_request(id: XwmId, x: X11Surface);
    fn map_window_notify(id: XwmId, x: X11Surface);
    fn mapped_override_redirect_window(id: XwmId, x: X11Surface);
    fn unmapped_window(id: XwmId, x: X11Surface);
    fn destroyed_window(id: XwmId, x: X11Surface);
    fn configure_request(id: XwmId, x: X11Surface, px: Option<i32>, py: Option<i32>, w: Option<u32>, h: Option<u32>, order: Option<Reorder>);
    fn configure_notify(id: XwmId, x: X11Surface, geo: Rectangle<i32, Logical>, above: Option<u32>);
    fn property_notify(id: XwmId, x: X11Surface, prop: WmWindowProperty);
    fn maximize_request(id: XwmId, x: X11Surface);
    fn unmaximize_request(id: XwmId, x: X11Surface);
    fn fullscreen_request(id: XwmId, x: X11Surface);
    fn unfullscreen_request(id: XwmId, x: X11Surface);
    fn minimize_request(id: XwmId, x: X11Surface);
    fn unminimize_request(id: XwmId, x: X11Surface);
    fn resize_request(id: XwmId, x: X11Surface, button: u32, edge: ResizeEdge);
    fn move_request(id: XwmId, x: X11Surface, button: u32);
    fn send_selection(id: XwmId, ty: SelectionTarget, mime: String, fd: OwnedFd);
    fn new_selection(id: XwmId, ty: SelectionTarget, mime_types: Vec<String>);
    fn cleared_selection(id: XwmId, ty: SelectionTarget);
    fn disconnected(id: XwmId);
}
impl XWaylandShellHandler for CalloopData {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.state.xwayland_shell
    }
    fn surface_associated(&mut self, _: XwmId, _: WlSurface, x: X11Surface) {
        self.state.map_x11(x);
    }
}
delegate_xwayland_shell!(Navi);

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::{
        backend::input::KeyState,
        output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel},
        reexports::{
            calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
            wayland_server::Display,
            x11rb::{
                connection::Connection,
                protocol::{Event, xproto::*},
                wrapper::ConnectionExt as _,
            },
        },
    };
    use smithay_client_toolkit::reexports::{
        client::{
            Connection as WaylandConnection, Dispatch as ClientDispatch, EventQueue, Proxy,
            QueueHandle,
            backend::WaylandError,
            delegate_noop, event_created_child,
            protocol::{
                wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer, wl_registry,
                wl_seat, wl_surface,
            },
        },
        protocols_wlr::foreign_toplevel::v1::client::{
            zwlr_foreign_toplevel_handle_v1::{self as handle, ZwlrForeignToplevelHandleV1},
            zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
        },
    };
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Metadata {
        manager: Option<ZwlrForeignToplevelManagerV1>,
        titles: Vec<String>,
        apps: Vec<String>,
        compositor: Option<wl_compositor::WlCompositor>,
        seat: Option<wl_seat::WlSeat>,
        data_manager: Option<wl_data_device_manager::WlDataDeviceManager>,
        offers: Vec<String>,
        selection_received: bool,
    }

    impl Metadata {
        fn read(&mut self, queue: &mut EventQueue<Self>) {
            if let Some(read) = queue.prepare_read() {
                match read.read() {
                    Ok(_) => {}
                    Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("window-list client: {e}"),
                }
            }
            queue.dispatch_pending(self).unwrap();
            queue.flush().unwrap();
        }
    }

    impl ClientDispatch<wl_registry::WlRegistry, ()> for Metadata {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &WaylandConnection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = event
            {
                match interface.as_str() {
                    "zwlr_foreign_toplevel_manager_v1" => {
                        state.manager = Some(registry.bind(name, version.min(3), qh, ()))
                    }
                    "wl_compositor" => {
                        state.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                    }
                    "wl_seat" => state.seat = Some(registry.bind(name, version.min(7), qh, ())),
                    "wl_data_device_manager" => {
                        state.data_manager = Some(registry.bind(name, version.min(3), qh, ()))
                    }
                    _ => {}
                }
            }
        }
    }

    impl ClientDispatch<ZwlrForeignToplevelManagerV1, ()> for Metadata {
        fn event(
            _: &mut Self,
            _: &ZwlrForeignToplevelManagerV1,
            _: manager::Event,
            _: &(),
            _: &WaylandConnection,
            _: &QueueHandle<Self>,
        ) {
        }
        event_created_child!(Metadata, ZwlrForeignToplevelManagerV1, [
            manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
        ]);
    }

    impl ClientDispatch<ZwlrForeignToplevelHandleV1, ()> for Metadata {
        fn event(
            state: &mut Self,
            _: &ZwlrForeignToplevelHandleV1,
            event: handle::Event,
            _: &(),
            _: &WaylandConnection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                handle::Event::Title { title } => state.titles.push(title),
                handle::Event::AppId { app_id } => state.apps.push(app_id),
                _ => {}
            }
        }
    }

    delegate_noop!(Metadata: ignore wl_compositor::WlCompositor);
    delegate_noop!(Metadata: ignore wl_surface::WlSurface);
    delegate_noop!(Metadata: ignore wl_seat::WlSeat);
    delegate_noop!(Metadata: ignore wl_data_device_manager::WlDataDeviceManager);

    impl ClientDispatch<wl_data_device::WlDataDevice, ()> for Metadata {
        fn event(
            state: &mut Self,
            _: &wl_data_device::WlDataDevice,
            event: wl_data_device::Event,
            _: &(),
            _: &WaylandConnection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_data_device::Event::Selection { id } = event {
                state.selection_received = id.is_some();
            }
        }
        event_created_child!(Metadata, wl_data_device::WlDataDevice, [
            wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, ()),
        ]);
    }

    impl ClientDispatch<wl_data_offer::WlDataOffer, ()> for Metadata {
        fn event(
            state: &mut Self,
            _: &wl_data_offer::WlDataOffer,
            event: wl_data_offer::Event,
            _: &(),
            _: &WaylandConnection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_data_offer::Event::Offer { mime_type } = event {
                state.offers.push(mime_type);
            }
        }
    }

    #[track_caller]
    fn pump(
        event_loop: &mut EventLoop<CalloopData>,
        data: &mut CalloopData,
        mut done: impl FnMut(&CalloopData) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(data) {
            assert!(Instant::now() < deadline, "XWayland operation timed out");
            event_loop.dispatch(Duration::from_millis(5), data).unwrap();
            data.display.flush_clients().unwrap();
        }
    }

    /// Real X server, real X11 events, no host display or GPU. Run explicitly in CI.
    #[test]
    #[ignore = "requires the Xwayland executable"]
    fn x11_lifecycle_and_input() {
        let mut event_loop = EventLoop::try_new().unwrap();
        let mut display = Display::new().unwrap();
        let mut state = Navi::new(
            display.handle(),
            event_loop.handle(),
            event_loop.get_signal(),
        )
        .unwrap();
        state.layout = Layout::default();
        state.memory = crate::persist::tests::volatile();
        let output = Output::new(
            "test".into(),
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
        output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
        output.set_preferred(mode);
        output.create_global::<Navi>(&display.handle());
        state.space.map_output(&output, (0, 0));
        state.add_output(&output, 0);
        let fd = display.backend().poll_fd().try_clone_to_owned().unwrap();
        event_loop
            .handle()
            .insert_source(
                Generic::new(fd, Interest::READ, Mode::Level),
                |_, _, data: &mut CalloopData| {
                    data.display.dispatch_clients(&mut data.state)?;
                    Ok(PostAction::Continue)
                },
            )
            .unwrap();
        let mut data = CalloopData {
            state,
            display,
            udev: None,
        };
        // Bind the same privileged window list that navi-bar uses, over a real Wayland socket.
        let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        data.state
            .display_handle
            .insert_client(
                server,
                std::sync::Arc::new(crate::state::ClientState {
                    privileged: true,
                    ..Default::default()
                }),
            )
            .unwrap();
        let shell = WaylandConnection::from_socket(client).unwrap();
        let mut queue = shell.new_event_queue::<Metadata>();
        let _registry = shell.display().get_registry(&queue.handle(), ());
        let mut metadata = Metadata::default();
        queue.flush().unwrap();
        let xdisplay = start(&event_loop.handle(), &data.state).expect("spawn Xwayland");
        pump(&mut event_loop, &mut data, |d| d.state.xwm.is_some());
        let (conn, screen) = smithay::reexports::x11rb::connect(Some(&xdisplay)).unwrap();
        let root = conn.setup().roots[screen].root;
        let window = conn.generate_id().unwrap();
        conn.create_window(
            smithay::reexports::x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            320,
            240,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(0x224466)
                .event_mask(
                    EventMask::KEY_PRESS | EventMask::KEY_RELEASE | EventMask::BUTTON_PRESS,
                ),
        )
        .unwrap();
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"navi-test\0NaviTest\0",
        )
        .unwrap();
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"X11 integration test",
        )
        .unwrap();
        let atom = |name: &[u8]| conn.intern_atom(false, name).unwrap().reply().unwrap().atom;
        let protocols = atom(b"WM_PROTOCOLS");
        let delete = atom(b"WM_DELETE_WINDOW");
        conn.change_property32(
            PropMode::REPLACE,
            window,
            protocols,
            AtomEnum::ATOM,
            &[delete],
        )
        .unwrap();
        conn.map_window(window).unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| d.state.windows.len() == 1);
        let managed = &data.state.windows[0];
        let id = managed.tile.expect("X11 window tiled");
        let surface = managed.window.x11_surface().unwrap().clone();
        assert_eq!(window_app(&managed.window), "NaviTest");
        assert_eq!(
            crate::state::window_title(&managed.window),
            "X11 integration test"
        );
        assert!(data.state.layout.tile(id).unwrap().decorated);
        assert_eq!(
            surface.geometry().size.w,
            data.state.layout.rect(id).unwrap().w
        );
        data.state.refresh_toplevels();
        pump(&mut event_loop, &mut data, |_| {
            metadata.read(&mut queue);
            metadata.apps.last().is_some_and(|app| app == "NaviTest")
                && metadata
                    .titles
                    .last()
                    .is_some_and(|title| title == "X11 integration test")
        });
        // X11 strings can contain NUL bytes; Wayland strings cannot. Exercise real wire events
        // for both metadata fields, including updates after the window list is already bound.
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"navi-test\0Navi\0Test\0",
        )
        .unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |_| {
            metadata.read(&mut queue);
            metadata
                .apps
                .last()
                .is_some_and(|app| app == "Navi\u{fffd}Test")
        });
        let name = atom(b"_NET_WM_NAME");
        let utf8 = atom(b"UTF8_STRING");
        conn.change_property8(PropMode::REPLACE, window, name, utf8, b"a\0b")
            .unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |_| {
            metadata.read(&mut queue);
            metadata
                .titles
                .last()
                .is_some_and(|title| title == "a\u{fffd}b")
        });
        // Oversized events must not disconnect the window-list client. Count bytes after
        // sanitizing, and keep complete characters even when UTF-8 or NUL replacement expands.
        for (raw, expected) in [
            ("x".repeat(5000), "x".repeat(2048)),
            ("\u{20ac}".repeat(2000), "\u{20ac}".repeat(682)),
            (format!("{}\0tail", "y".repeat(2047)), "y".repeat(2047)),
        ] {
            conn.change_property8(PropMode::REPLACE, window, name, utf8, raw.as_bytes())
                .unwrap();
            conn.flush().unwrap();
            pump(&mut event_loop, &mut data, |_| {
                metadata.read(&mut queue);
                metadata.titles.last() == Some(&expected)
            });
        }
        // WM_CLASS is decoded from Windows-1252: each 0x80 becomes a three-byte euro sign.
        let mut raw_class = b"navi-test\0".to_vec();
        raw_class.extend([0x80; 2000]);
        raw_class.push(0);
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            &raw_class,
        )
        .unwrap();
        conn.flush().unwrap();
        let expected_class = "\u{20ac}".repeat(682);
        pump(&mut event_loop, &mut data, |_| {
            metadata.read(&mut queue);
            metadata.apps.last() == Some(&expected_class)
        });
        // The test client and XWM use separate X11 connections. Mapping in Navi doesn't mean
        // the server has processed the XWM's focus request yet; wait for actual X input focus.
        pump(&mut event_loop, &mut data, |_| {
            conn.get_input_focus().unwrap().reply().unwrap().focus == window
        });
        data.state.keyboard_key(38u32.into(), KeyState::Pressed, 1);
        data.state.keyboard_key(38u32.into(), KeyState::Released, 2);
        let mut key = false;
        pump(&mut event_loop, &mut data, |_| {
            while let Some(event) = conn.poll_for_event().unwrap() {
                if let Event::KeyPress(event) = event {
                    key |= event.detail == 38;
                }
            }
            key
        });
        let cells = data.state.layout.tile(id).unwrap().cells;
        // Alt+Right moves one cell per press/repeat. Releasing Alt first must not leak a
        // later repeat or release to the X11 client, or leave a stale suppressed-key entry.
        for (code, state) in [
            (64u32, KeyState::Pressed),
            (114, KeyState::Pressed),
            (114, KeyState::Pressed),
            (64, KeyState::Released),
            (114, KeyState::Pressed),
            (114, KeyState::Released),
        ] {
            data.state.keyboard_key(code.into(), state, 3);
        }
        assert_eq!(data.state.layout.tile(id).unwrap().cells.col, cells.col + 2);
        assert_eq!(data.state.layout.tile(id).unwrap().cells.cols, cells.cols);
        assert!(data.state.suppressed.is_empty());
        for (code, state) in [
            (133u32, KeyState::Pressed),
            (114, KeyState::Pressed),
            (114, KeyState::Released),
            (133, KeyState::Released),
        ] {
            data.state.keyboard_key(code.into(), state, 3);
        }
        let expanded = data.state.layout.tile(id).unwrap().cells;
        assert_eq!(expanded.col, cells.col + 2);
        assert_eq!(expanded.cols, cells.cols + 1, "one column");
        assert_eq!(data.state.focused_tile(), Some(id));
        assert_eq!(
            surface.geometry().size.w,
            data.state.layout.rect(id).unwrap().w
        );
        // Super+Shift+Right takes that column back.
        for (code, state) in [
            (133u32, KeyState::Pressed),
            (50, KeyState::Pressed),
            (114, KeyState::Pressed),
            (114, KeyState::Released),
            (50, KeyState::Released),
            (133, KeyState::Released),
        ] {
            data.state.keyboard_key(code.into(), state, 3);
        }
        let shrunk = data.state.layout.tile(id).unwrap().cells;
        assert_eq!((shrunk.col, shrunk.cols), (cells.col + 2, cells.cols));
        assert_eq!(
            surface.geometry().size.w,
            data.state.layout.rect(id).unwrap().w
        );
        assert!(data.state.suppressed.is_empty());
        // With no tiled keyboard target, the compositor doesn't claim window shortcuts.
        data.state.focus(None);
        for (code, state) in [
            (64u32, KeyState::Pressed),
            (114, KeyState::Pressed),
            (114, KeyState::Released),
            (64, KeyState::Released),
        ] {
            data.state.keyboard_key(code.into(), state, 3);
        }
        assert_eq!(data.state.layout.tile(id).unwrap().cells, shrunk);
        assert!(data.state.suppressed.is_empty());
        data.state.focus(data.state.window_of(id));
        let rect = data.state.layout.rect(id).unwrap();
        data.state
            .pointer_motion((f64::from(rect.x + 80), f64::from(rect.y + 80)).into(), 3);
        data.state.pointer_button(0x110, true, true, 4);
        data.state.pointer_button(0x110, true, false, 5);
        let mut click = false;
        pump(&mut event_loop, &mut data, |_| {
            while let Some(event) = conn.poll_for_event().unwrap() {
                match event {
                    Event::ButtonPress(event) => click |= event.detail == 1,
                    Event::KeyPress(event) | Event::KeyRelease(event) => {
                        assert!(
                            ![111, 113, 114, 116].contains(&event.detail),
                            "shortcut arrow leaked to client"
                        );
                    }
                    _ => {}
                }
            }
            click
        });
        // An application-requested fullscreen round trip goes through the XWM and layout.
        let net_state = atom(b"_NET_WM_STATE");
        let fullscreen = atom(b"_NET_WM_STATE_FULLSCREEN");
        let request = |action| {
            conn.send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                ClientMessageEvent::new(32, window, net_state, [action, fullscreen, 0, 1, 0]),
            )
            .unwrap();
            conn.flush().unwrap();
        };
        request(1);
        pump(&mut event_loop, &mut data, |d| {
            d.state.layout.is_fullscreen(id)
        });
        assert_eq!(surface.geometry().size, (1280, 720).into());
        request(0);
        pump(&mut event_loop, &mut data, |d| {
            !d.state.layout.is_fullscreen(id)
        });
        assert_eq!(surface.geometry().size, (rect.w, rect.h).into());
        // Super+Esc ends it without the app; Esc never reaches the app.
        request(1);
        pump(&mut event_loop, &mut data, |d| {
            d.state.layout.is_fullscreen(id)
        });
        for (code, state) in [
            (133u32, KeyState::Pressed),
            (9, KeyState::Pressed),
            (9, KeyState::Released),
            (133, KeyState::Released),
        ] {
            data.state.keyboard_key(code.into(), state, 6);
        }
        assert!(!data.state.layout.is_fullscreen(id));
        assert!(data.state.suppressed.is_empty());
        pump(&mut event_loop, &mut data, |_| {
            surface.geometry().size == (rect.w, rect.h).into()
        });
        // A fullscreen request, then a title change that the XWM handles after it, in order:
        // once the title shows, the request has been decided.
        let mut asks = 0;
        let mut ask = |event_loop: &mut EventLoop<CalloopData>, data: &mut CalloopData| {
            asks += 1;
            let title = format!("ask {asks}");
            request(1);
            conn.change_property8(PropMode::REPLACE, window, name, utf8, title.as_bytes())
                .unwrap();
            conn.flush().unwrap();
            pump(event_loop, data, |_| {
                metadata.read(&mut queue);
                metadata.titles.last() == Some(&title)
            });
            data.state.layout.is_fullscreen(id)
        };
        // Its next request is refused, also after a bare Super tap: that is the first half of
        // the next Super+Esc, and the app does see it.
        assert!(!ask(&mut event_loop, &mut data), "re-entry refused");
        data.state.keyboard_key(133u32.into(), KeyState::Pressed, 7);
        data.state
            .keyboard_key(133u32.into(), KeyState::Released, 7);
        assert!(
            !ask(&mut event_loop, &mut data),
            "bare Super doesn't lift the guard"
        );
        // A second Super+Esc, with nothing fullscreen, is still navi's: kept from the app (the
        // leak check below covers keycode 9) and no lift.
        for (code, state) in [
            (133u32, KeyState::Pressed),
            (9, KeyState::Pressed),
            (9, KeyState::Released),
            (133, KeyState::Released),
        ] {
            data.state.keyboard_key(code.into(), state, 7);
        }
        assert!(data.state.suppressed.is_empty());
        assert!(
            !ask(&mut event_loop, &mut data),
            "a second Super+Esc doesn't lift the guard"
        );
        // A key press it receives lifts the guard.
        data.state.keyboard_key(38u32.into(), KeyState::Pressed, 7);
        data.state.keyboard_key(38u32.into(), KeyState::Released, 8);
        let mut key = false;
        pump(&mut event_loop, &mut data, |_| {
            while let Some(event) = conn.poll_for_event().unwrap() {
                if let Event::KeyPress(event) | Event::KeyRelease(event) = event {
                    assert_ne!(event.detail, 9, "Super+Esc leaked to client");
                    key |= event.detail == 38;
                }
            }
            key
        });
        assert!(
            ask(&mut event_loop, &mut data),
            "a typed key lifts the guard"
        );
        // So does a click on it, after another escape.
        for (code, state) in [
            (133u32, KeyState::Pressed),
            (9, KeyState::Pressed),
            (9, KeyState::Released),
            (133, KeyState::Released),
        ] {
            data.state.keyboard_key(code.into(), state, 9);
        }
        assert!(!ask(&mut event_loop, &mut data), "refused again");
        data.state.pointer_button(0x110, true, true, 10);
        data.state.pointer_button(0x110, true, false, 11);
        assert!(ask(&mut event_loop, &mut data), "a click lifts the guard");
        // navi-bar activating it keeps it fullscreen with no guard; activating another window
        // on its output (a tile with no client stands in) ends it with the Super+Esc guard.
        let (other, _) = data.state.layout.place("other", (1200, 700)).unwrap();
        assert!(
            ask(&mut event_loop, &mut data),
            "a new window is no user exit"
        );
        data.state.bar_activate(id);
        assert!(data.state.layout.is_fullscreen(id));
        data.state.bar_activate(other);
        assert!(!data.state.layout.is_fullscreen(id));
        assert!(
            !ask(&mut event_loop, &mut data),
            "refused after a bar activation"
        );
        data.state.layout.remove(other);
        data.state.apply_layout();
        data.state.pointer_button(0x110, true, true, 12);
        data.state.pointer_button(0x110, true, false, 13);
        assert!(ask(&mut event_loop, &mut data), "a click lifts it");
        request(0);
        pump(&mut event_loop, &mut data, |d| {
            !d.state.layout.is_fullscreen(id)
        });
        let wm_id = data.state.xwm.as_ref().unwrap().id();
        assert!(
            data.state
                .allow_selection_access(wm_id, SelectionTarget::Clipboard)
        );
        assert!(
            !data
                .state
                .allow_selection_access(wm_id, SelectionTarget::Primary)
        );
        data.state.focus(None);
        assert!(
            !data
                .state
                .allow_selection_access(wm_id, SelectionTarget::Clipboard)
        );
        data.state.focus(data.state.window_of(id));

        // An X11 clipboard offer reaches Wayland, including the actual byte transfer.
        let clipboard = atom(b"CLIPBOARD");
        let targets = atom(b"TARGETS");
        let utf8 = atom(b"UTF8_STRING");
        let oversized_mime = format!("application/{}", "x".repeat(5000));
        let oversized = atom(oversized_mime.as_bytes());
        let valid = atom(b"application/x-navi-test");
        let respond = || {
            while let Some(event) = conn.poll_for_event().unwrap() {
                if let Event::SelectionRequest(e) = event {
                    if e.target == targets {
                        conn.change_property32(
                            PropMode::REPLACE,
                            e.requestor,
                            e.property,
                            AtomEnum::ATOM,
                            &[utf8, oversized, valid],
                        )
                        .unwrap();
                    } else {
                        assert_eq!(e.target, utf8);
                        conn.change_property8(
                            PropMode::REPLACE,
                            e.requestor,
                            e.property,
                            utf8,
                            b"navi clipboard",
                        )
                        .unwrap();
                    }
                    conn.send_event(
                        false,
                        e.requestor,
                        EventMask::NO_EVENT,
                        SelectionNotifyEvent {
                            response_type: SELECTION_NOTIFY_EVENT,
                            sequence: 0,
                            time: e.time,
                            requestor: e.requestor,
                            selection: e.selection,
                            target: e.target,
                            property: e.property,
                        },
                    )
                    .unwrap();
                    conn.flush().unwrap();
                }
            }
        };
        conn.set_selection_owner(window, clipboard, smithay::reexports::x11rb::CURRENT_TIME)
            .unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| {
            respond();
            current_data_device_selection_userdata(&d.state.seat)
                .is_some_and(|owner| *owner == wm_id)
        });
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let seat = data.state.seat.clone();
        smithay::wayland::selection::SelectionHandler::send_selection(
            &mut data.state,
            SelectionTarget::Clipboard,
            "text/plain;charset=utf-8".into(),
            writer.into(),
            seat,
            &wm_id,
        );
        let mut bytes = Vec::new();
        pump(&mut event_loop, &mut data, |_| {
            use std::io::Read;
            respond();
            match reader.read_to_end(&mut bytes) {
                Ok(_) => true,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(e) => panic!("clipboard: {e}"),
            }
        });
        assert_eq!(bytes, b"navi clipboard");

        // A separate, unprivileged native client must receive valid clipboard offers even
        // when the X11 owner advertises a target too large for Wayland's event buffer.
        let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        let native_client = data
            .state
            .display_handle
            .insert_client(
                server,
                std::sync::Arc::new(crate::state::ClientState::default()),
            )
            .unwrap();
        let native_connection = WaylandConnection::from_socket(client).unwrap();
        let mut native_queue = native_connection.new_event_queue::<Metadata>();
        let _registry = native_connection
            .display()
            .get_registry(&native_queue.handle(), ());
        let mut native = Metadata::default();
        native_queue.flush().unwrap();
        pump(&mut event_loop, &mut data, |_| {
            native.read(&mut native_queue);
            native.compositor.is_some() && native.seat.is_some() && native.data_manager.is_some()
        });
        assert!(
            native.manager.is_none(),
            "unprivileged client saw the window list"
        );
        let surface = native
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&native_queue.handle(), ());
        let _device = native.data_manager.as_ref().unwrap().get_data_device(
            native.seat.as_ref().unwrap(),
            &native_queue.handle(),
            (),
        );
        native_queue.flush().unwrap();
        let mut server_surface = None;
        pump(&mut event_loop, &mut data, |d| {
            server_surface = native_client
                .object_from_protocol_id::<WlSurface>(
                    &d.state.display_handle,
                    surface.id().protocol_id(),
                )
                .ok();
            server_surface.is_some()
        });
        let keyboard = data.state.seat.get_keyboard().unwrap();
        keyboard.set_focus(
            &mut data.state,
            Some(crate::focus::Focus::Wayland(server_surface.unwrap())),
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );
        data.display.flush_clients().unwrap();
        pump(&mut event_loop, &mut data, |_| {
            native.read(&mut native_queue);
            native.selection_received
        });
        assert_eq!(
            native.offers,
            ["text/plain;charset=utf-8", "application/x-navi-test"]
        );
        data.state.focus(data.state.window_of(id));

        let dialog = conn.generate_id().unwrap();
        conn.create_window(
            smithay::reexports::x11rb::COPY_DEPTH_FROM_PARENT,
            dialog,
            root,
            0,
            0,
            160,
            100,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().background_pixel(0x446688),
        )
        .unwrap();
        conn.change_property32(
            PropMode::REPLACE,
            dialog,
            AtomEnum::WM_TRANSIENT_FOR,
            AtomEnum::WINDOW,
            &[window],
        )
        .unwrap();
        conn.map_window(dialog).unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| d.state.windows.len() == 2);
        let dialog_window = data
            .state
            .windows
            .iter()
            .find(|m| m.x11_parent == Some(window))
            .unwrap()
            .window
            .clone();
        assert!(
            data.state
                .windows
                .iter()
                .find(|m| m.window == dialog_window)
                .unwrap()
                .tile
                .is_none()
        );
        assert!(data.state.space.element_location(&dialog_window).is_some());
        // Super+Esc from the parent's focused dialog acts on the parent's output, with the
        // pointer off every output so only the dialog's parent can decide.
        let output = data.state.layout.tile(id).unwrap().output.clone();
        data.state.focus(Some(dialog_window.clone()));
        assert_eq!(data.state.focused_tile(), None);
        data.state.pointer_motion((-50.0, -50.0).into(), 12);
        assert!(data.state.layout.fullscreen(id));
        data.state.apply_layout();
        assert_eq!(data.state.escape_target(), Some(output));
        data.state
            .keyboard_key(133u32.into(), KeyState::Pressed, 13);
        data.state.keyboard_key(9u32.into(), KeyState::Pressed, 13);
        assert!(!data.state.layout.is_fullscreen(id));
        assert_eq!(data.state.escape_target(), None);
        data.state.keyboard_key(9u32.into(), KeyState::Released, 13);
        data.state
            .keyboard_key(133u32.into(), KeyState::Released, 13);
        assert!(data.state.suppressed.is_empty());
        data.state.pointer_motion((80.0, 80.0).into(), 14);
        data.state.layout.minimize(id);
        data.state.apply_layout();
        assert!(data.state.space.element_location(&dialog_window).is_none());
        assert!(
            data.state
                .seat
                .get_keyboard()
                .unwrap()
                .current_focus()
                .is_none()
        );
        data.state.layout.activate(id);
        data.state.apply_layout();
        assert!(data.state.space.element_location(&dialog_window).is_some());
        conn.destroy_window(dialog).unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| d.state.windows.len() == 1);
        data.state.focus(data.state.window_of(id));
        let menu = conn.generate_id().unwrap();
        conn.create_window(
            smithay::reexports::x11rb::COPY_DEPTH_FROM_PARENT,
            menu,
            root,
            70,
            80,
            100,
            60,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(0x6688aa),
        )
        .unwrap();
        conn.map_window(menu).unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| d.state.windows.len() == 2);
        let popup = data
            .state
            .windows
            .iter()
            .find(|m| m.window.x11_surface().unwrap().is_override_redirect())
            .unwrap()
            .window
            .clone();
        assert_eq!(
            data.state.space.element_location(&popup),
            Some((70, 80).into())
        );
        assert_eq!(data.state.focused_tile(), Some(id));
        data.state.layout.minimize(id);
        data.state.apply_layout();
        assert!(data.state.space.element_location(&popup).is_none());
        data.state.layout.activate(id);
        data.state.apply_layout();
        assert_eq!(
            data.state.space.element_location(&popup),
            Some((70, 80).into())
        );
        conn.destroy_window(menu).unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| d.state.windows.len() == 1);
        // Navi's close control respects WM_DELETE_WINDOW rather than killing the client.
        data.state.close_window(id);
        let mut closed = false;
        pump(&mut event_loop, &mut data, |_| {
            while let Some(event) = conn.poll_for_event().unwrap() {
                if let Event::ClientMessage(event) = event {
                    closed |= event.type_ == protocols && event.data.as_data32()[0] == delete;
                }
            }
            closed
        });
        conn.unmap_window(window).unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| d.state.windows.is_empty());
        assert!(data.state.layout.tile(id).is_none());
        conn.map_window(window).unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| d.state.windows.len() == 1);
        conn.destroy_window(window).unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| d.state.windows.is_empty());
    }
}
