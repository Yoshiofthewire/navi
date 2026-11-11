//! X11 compatibility through Smithay's rootless XWayland server and window manager.

use crate::{
    CalloopData,
    layout::{Hints, Layout, Step, WinId},
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
    reexports::{
        calloop::LoopHandle, wayland_server::protocol::wl_surface::WlSurface,
        x11rb::properties::WmSizeHints,
    },
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

/// An X11 window's `WM_NORMAL_HINTS` minimum and maximum, untrusted; unset is 0, no limit.
/// Read raw: Smithay's `min_size()` builds a `Size`, which panics on a negative value in debug
/// builds. navi sets no XWayland client scale, so these px are logical.
pub(crate) fn x11_hints(x: &X11Surface) -> ((i32, i32), (i32, i32)) {
    min_max(x.size_hints())
}

fn min_max(h: Option<WmSizeHints>) -> ((i32, i32), (i32, i32)) {
    let h = h.unwrap_or_default();
    (
        h.min_size.unwrap_or_default(),
        h.max_size.unwrap_or_default(),
    )
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
            let (min, max) = x11_hints(&x);
            let hints = Hints::new(min, max);
            let at = self.pointer_px();
            if let Some((id, step)) = self.layout.place_with(&app, at, hints, !x.is_decorated()) {
                if hints != Hints::default() {
                    info!(app, id, min = ?hints.min, max = ?hints.max, "client size hints");
                }
                if x.is_fullscreen() {
                    self.layout.fullscreen(id);
                } else if x.is_maximized() {
                    self.layout.maximize(id);
                }
                tile = Some(id);
                let tile = self.layout.tile(id).map(|t| (t.output.clone(), t.cells));
                let rect = self.layout.rect(id);
                info!(app, id, ?step, ?tile, ?rect, "X11 placed");
                if step == Step::Floating {
                    self.log_float(id, &app);
                }
                self.memory.sync(&self.layout);
            }
        }
        self.windows.push(Managed {
            window,
            tile,
            last_size: x.geometry().size,
            clipped: None,
            refitted: None,
            refit_queued: false,
            want_fullscreen: false,
            escaped: false,
            x11_parent,
            // `recenter_dialogs` resolves a transient's parent.
            up: None,
            fresh: false,
            waiting: false,
            rank: 0,
            user_rank: 0,
            floor: 0,
            born: self.raises,
            asked: None,
            placed: None,
            last_output: None,
            capped_for: None,
        });
        self.apply_layout();
        if !x.is_override_redirect() {
            self.focus_new(self.windows.len() - 1);
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
        let m = self.forget(at);
        if let Some(id) = m.tile {
            self.layout.remove(id);
            info!(id, "X11 closed; its cells stay empty");
        }
        self.apply_layout();
        self.refresh_toplevels();
    }

    /// Places the X11 menus, then moves the keyboard off any window that isn't shown (a focused
    /// menu hidden with its parent).
    pub fn recenter_x11(&mut self) {
        self.place_menus();
        self.keep_focus_mapped(&[]);
    }

    /// Override-redirect menus keep their X11 geometry and show only while their X11 parent is
    /// shown; `recenter_dialogs` places transients.
    pub(crate) fn place_menus(&mut self) {
        let menus: Vec<_> = self
            .windows
            .iter()
            .filter(|m| m.tile.is_none())
            .filter_map(|m| {
                let x = m.window.x11_surface()?;
                x.is_override_redirect()
                    .then(|| (m.window.clone(), x.geometry().loc, m.x11_parent))
            })
            .collect();
        let mut new = false;
        for (window, loc, parent) in menus {
            let shown = parent.is_some_and(|id| {
                self.windows.iter().any(|m| {
                    m.window.x11_surface().is_some_and(|p| p.window_id() == id)
                        && self.space.element_location(&m.window).is_some()
                })
            });
            match shown {
                true => new |= crate::state::put(&mut self.space, &window, (loc.x, loc.y)),
                false => self.space.unmap_elem(&window),
            }
        }
        // Only a newly shown menu changes the order; dialog commits come through here too.
        if new {
            self.restack();
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
            .iter_mut()
            .find(|m| m.window.x11_surface() == Some(&x))
        {
            if m.tile.is_some() {
                self.apply_layout();
            } else if x.is_override_redirect() {
                self.recenter_x11();
            } else {
                // The client sees only the capped size `recenter_dialogs` configures.
                let (mut w0, mut h0) = m.asked.unwrap_or_else(|| {
                    let g = x.geometry().size;
                    (g.w, g.h)
                });
                if let Some(w) = w {
                    w0 = w.clamp(1, 8192) as i32;
                }
                if let Some(h) = h {
                    h0 = h.clamp(1, 8192) as i32;
                }
                m.asked = Some((w0, h0));
                self.recenter_dialogs();
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
    fn property_notify(&mut self, _: XwmId, x: X11Surface, prop: WmWindowProperty) {
        match (prop, x.wl_surface()) {
            // `refit` paces it and ignores windows without a tile.
            (WmWindowProperty::NormalHints, Some(root)) => self.refit(&root),
            (WmWindowProperty::NormalHints, None) => {}
            (WmWindowProperty::TransientFor, _) => self.recenter_dialogs(),
            _ => self.refresh_toplevels(),
        }
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

    #[test]
    fn unset_x11_hints_are_no_limit() {
        assert_eq!(min_max(None), ((0, 0), (0, 0)));
        let only_min = WmSizeHints {
            min_size: Some((-5, 70_000)),
            ..WmSizeHints::new()
        };
        assert_eq!(min_max(Some(only_min)), ((-5, 70_000), (0, 0)));
        let only_max = WmSizeHints {
            max_size: Some((400, 300)),
            ..WmSizeHints::new()
        };
        assert_eq!(min_max(Some(only_max)), ((0, 0), (400, 300)));
    }

    /// navi with one 1280 × 720 test output and a fresh layout, not yet running Xwayland.
    fn harness() -> (EventLoop<'static, CalloopData>, CalloopData) {
        let event_loop = EventLoop::try_new().unwrap();
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
        let data = CalloopData {
            state,
            display,
            udev: None,
        };
        (event_loop, data)
    }

    /// Real X server, real X11 events, no host display or GPU. Run explicitly in CI.
    #[test]
    #[ignore = "requires the Xwayland executable"]
    fn x11_lifecycle_and_input() {
        let (mut event_loop, mut data) = harness();
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
        use crate::state::Raise;
        // navi's own focus raises nothing.
        let raises = data.state.raises;
        data.state.focus(Some(dialog_window.clone()));
        assert_eq!(data.state.focused_window(), Some(dialog_window.clone()));
        assert_eq!(data.state.raises, raises, "navi's own focus doesn't raise");
        // A closed dialog the user focused gives the keyboard back to its parent, with no raise.
        data.state
            .focus_by(Some(dialog_window.clone()), Raise::User);
        let raises = data.state.raises;
        conn.destroy_window(dialog).unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| d.state.windows.len() == 1);
        assert_eq!(data.state.focused_window(), data.state.window_of(id));
        assert_eq!(data.state.raises, raises, "the hand-back doesn't raise");
        // Round 6/7 wiring: a user focus marks the keyboard as the user's; navi's refocus of the
        // same window keeps it and doesn't count as a move; any other move clears it.
        let parent = data.state.window_of(id);
        data.state.focus_by(parent.clone(), Raise::User);
        assert!(data.state.focus_by_user);
        let moves = data.state.focus_moves;
        data.state.focus(parent.clone());
        assert!(
            data.state.focus_by_user,
            "navi's refocus of the same window keeps it"
        );
        assert_eq!(
            data.state.focus_moves, moves,
            "and the keyboard didn't move"
        );
        data.state.focus(None);
        assert!(!data.state.focus_by_user);
        assert!(data.state.focus_moves > moves);
        data.state.focus_by(parent.clone(), Raise::New);
        assert!(
            !data.state.focus_by_user,
            "a window that took the keyboard on its own"
        );
        data.state.focus_by(parent, Raise::User);
        assert!(data.state.focus_by_user);

        // PR #39 audit P1: a focused dialog hidden by a re-parent (to a minimized window) loses
        // the keyboard at once, and later keys reach nobody.
        let make = |transient_for: Option<u32>| {
            let w = conn.generate_id().unwrap();
            conn.create_window(
                smithay::reexports::x11rb::COPY_DEPTH_FROM_PARENT,
                w,
                root,
                0,
                0,
                200,
                150,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .background_pixel(0x664422)
                    .event_mask(EventMask::KEY_PRESS),
            )
            .unwrap();
            if let Some(p) = transient_for {
                conn.change_property32(
                    PropMode::REPLACE,
                    w,
                    AtomEnum::WM_TRANSIENT_FOR,
                    AtomEnum::WINDOW,
                    &[p],
                )
                .unwrap();
            }
            conn.map_window(w).unwrap();
            conn.flush().unwrap();
            w
        };
        let hidden = make(None);
        pump(&mut event_loop, &mut data, |d| d.state.windows.len() == 2);
        let hidden_id = data.state.windows[1].tile.expect("tiled");
        data.state.layout.minimize(hidden_id);
        data.state.apply_layout();
        let hop = make(Some(window));
        pump(&mut event_loop, &mut data, |d| d.state.windows.len() == 3);
        let hop_window = data.state.windows[2].window.clone();
        pump(&mut event_loop, &mut data, |d| {
            d.state.space.element_location(&hop_window).is_some()
        });
        data.state.focus_by(Some(hop_window.clone()), Raise::User);
        assert_eq!(data.state.focused_window(), Some(hop_window.clone()));
        pump(&mut event_loop, &mut data, |_| {
            conn.get_input_focus().unwrap().reply().unwrap().focus == hop
        });
        // Round 11: a menu of that dialog, which the user clicked (it takes the keyboard), hides
        // with it and loses the keyboard too.
        let menu = conn.generate_id().unwrap();
        conn.create_window(
            smithay::reexports::x11rb::COPY_DEPTH_FROM_PARENT,
            menu,
            root,
            40,
            40,
            80,
            60,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(0x226644)
                .event_mask(EventMask::KEY_PRESS),
        )
        .unwrap();
        conn.map_window(menu).unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| d.state.windows.len() == 4);
        let menu_window = data.state.windows[3].window.clone();
        assert_eq!(
            data.state.windows[3].x11_parent,
            Some(hop),
            "the focused transient"
        );
        assert!(data.state.space.element_location(&menu_window).is_some());
        data.state.focus_by(Some(menu_window.clone()), Raise::User);
        assert_eq!(data.state.focused_window(), Some(menu_window.clone()));
        conn.change_property32(
            PropMode::REPLACE,
            hop,
            AtomEnum::WM_TRANSIENT_FOR,
            AtomEnum::WINDOW,
            &[hidden],
        )
        .unwrap();
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| {
            d.state.space.element_location(&hop_window).is_none()
        });
        assert!(
            data.state.space.element_location(&menu_window).is_none(),
            "the menu hid with it"
        );
        let kb = data.state.seat.get_keyboard().unwrap();
        assert_eq!(
            kb.current_focus(),
            None,
            "the hidden menu lost the keyboard"
        );
        data.state.keyboard_key(38u32.into(), KeyState::Pressed, 30);
        data.state
            .keyboard_key(38u32.into(), KeyState::Released, 31);
        let (mut rounds, mut leaked) = (0, false);
        pump(&mut event_loop, &mut data, |_| {
            while let Some(event) = conn.poll_for_event().unwrap() {
                if let Event::KeyPress(event) = event {
                    leaked |= event.event == hop || event.event == menu;
                }
            }
            rounds += 1;
            rounds > 60
        });
        assert!(!leaked, "no key reached the hidden dialog");
        for w in [menu, hop, hidden] {
            conn.destroy_window(w).unwrap();
        }
        conn.flush().unwrap();
        pump(&mut event_loop, &mut data, |d| d.state.windows.len() == 1);
        data.state.focus(data.state.window_of(id));

        // Round 12: a closed window hands the keyboard back only to the user's last choice.
        let parent = data.state.window_of(id);
        let close = |data: &mut CalloopData, event_loop: &mut EventLoop<CalloopData>, w: u32| {
            conn.destroy_window(w).unwrap();
            conn.flush().unwrap();
            pump(event_loop, data, |d| d.state.windows.len() == 1);
        };
        let open = |data: &mut CalloopData, event_loop: &mut EventLoop<CalloopData>| {
            let w = make(None);
            pump(event_loop, data, |d| d.state.windows.len() == 2);
            let win = data.state.windows[1].window.clone();
            pump(event_loop, data, |d| {
                d.state.space.element_location(&win).is_some()
            });
            (w, win)
        };
        // Allowed: the user was on the parent, then on B; B closes.
        data.state.focus_by(parent.clone(), Raise::User);
        let (b, b_win) = open(&mut data, &mut event_loop);
        data.state.focus_by(Some(b_win), Raise::User);
        close(&mut data, &mut event_loop, b);
        assert_eq!(
            data.state.focused_window(),
            parent,
            "back to the user's last choice"
        );
        // Refused: the parent took the keyboard on its own before the user went to B.
        data.state.focus_by(parent.clone(), Raise::New);
        let (b, b_win) = open(&mut data, &mut event_loop);
        data.state.focus_by(Some(b_win), Raise::User);
        close(&mut data, &mut event_loop, b);
        assert_eq!(
            data.state.focused_window(),
            None,
            "not a focus the user gave"
        );
        // Refused: the keyboard moved on (to nobody) after leaving the parent.
        data.state.focus_by(parent.clone(), Raise::User);
        let (b, b_win) = open(&mut data, &mut event_loop);
        data.state.focus_by(Some(b_win.clone()), Raise::User);
        data.state.focus(None);
        data.state.focus_by(Some(b_win), Raise::User);
        close(&mut data, &mut event_loop, b);
        assert_eq!(data.state.focused_window(), None, "a gap since the parent");
        // Refused: the parent is minimized (hidden) when B closes.
        data.state.focus_by(parent.clone(), Raise::User);
        let (b, b_win) = open(&mut data, &mut event_loop);
        data.state.focus_by(Some(b_win), Raise::User);
        data.state.layout.minimize(id);
        data.state.apply_layout();
        close(&mut data, &mut event_loop, b);
        assert_eq!(data.state.focused_window(), None, "the parent is hidden");
        data.state.layout.activate(id);
        data.state.apply_layout();
        // Refused: across the lock (it clears what the user left).
        data.state.focus_by(parent.clone(), Raise::User);
        let (b, b_win) = open(&mut data, &mut event_loop);
        data.state.focus_by(Some(b_win), Raise::User);
        data.state.before_focus = None;
        close(&mut data, &mut event_loop, b);
        assert_eq!(
            data.state.focused_window(),
            None,
            "nothing left to go back to"
        );
        data.state.focus(parent);
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

    /// `WM_NORMAL_HINTS` through the real XWM: read at map and on change, paced by `refit`, and
    /// ignored for dialogs and menus. Named so CI's `x11_lifecycle_and_input` filter runs it.
    #[test]
    #[ignore = "requires the Xwayland executable"]
    fn x11_lifecycle_and_input_size_hints() {
        use crate::layout::{BAR, Fit, GAP, Hints, Rect, STRIP};
        let (mut event_loop, mut data) = harness();
        let xdisplay = start(&event_loop.handle(), &data.state).expect("spawn Xwayland");
        pump(&mut event_loop, &mut data, |d| d.state.xwm.is_some());
        let (conn, screen) = smithay::reexports::x11rb::connect(Some(&xdisplay)).unwrap();
        let root = conn.setup().roots[screen].root;
        let hints = |min: Option<(i32, i32)>, max: Option<(i32, i32)>| WmSizeHints {
            min_size: min,
            max_size: max,
            ..WmSizeHints::new()
        };
        let set = |w: u32, h: &WmSizeHints| {
            h.set_normal_hints(&conn, w).unwrap();
            conn.flush().unwrap();
        };
        // Hints, and WM_TRANSIENT_FOR or override-redirect, are set before the window maps.
        let create =
            |class: &str, (x, y, w, h): (i16, i16, u16, u16), size: Option<WmSizeHints>| {
                let win = conn.generate_id().unwrap();
                let menu = class == "menu";
                conn.create_window(
                    smithay::reexports::x11rb::COPY_DEPTH_FROM_PARENT,
                    win,
                    root,
                    x,
                    y,
                    w,
                    h,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new()
                        .background_pixel(0x224466)
                        .override_redirect(u32::from(menu)),
                )
                .unwrap();
                let wm_class = format!("{class}\0{class}\0");
                conn.change_property8(
                    PropMode::REPLACE,
                    win,
                    AtomEnum::WM_CLASS,
                    AtomEnum::STRING,
                    wm_class.as_bytes(),
                )
                .unwrap();
                if let Some(size) = size {
                    size.set_normal_hints(&conn, win).unwrap();
                }
                win
            };
        let map = |win: u32| {
            conn.map_window(win).unwrap();
            conn.flush().unwrap();
        };
        let managed = |d: &CalloopData, win: u32| {
            d.state
                .windows
                .iter()
                .position(|m| m.window.x11_surface().is_some_and(|x| x.window_id() == win))
        };
        let destroy = |event_loop: &mut EventLoop<CalloopData>, data: &mut CalloopData, win| {
            conn.destroy_window(win).unwrap();
            conn.flush().unwrap();
            pump(event_loop, data, |d| managed(d, win).is_none());
        };
        // The XWM handles one connection's requests in order: once a later title shows, every
        // earlier request from it has been handled.
        let mut syncs = 0;
        let mut sync = |event_loop: &mut EventLoop<CalloopData>, data: &mut CalloopData, win| {
            syncs += 1;
            let title = format!("sync {syncs}");
            conn.change_property8(
                PropMode::REPLACE,
                win,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            )
            .unwrap();
            conn.flush().unwrap();
            pump(event_loop, data, |d| {
                managed(d, win).is_some_and(|i| {
                    crate::state::window_title(&d.state.windows[i].window) == title
                })
            });
        };
        let tile_of = |d: &CalloopData, win| managed(d, win).and_then(|i| d.state.windows[i].tile);
        let geometry = |d: &CalloopData, win| {
            let i = managed(d, win).unwrap();
            let g = d.state.windows[i].window.x11_surface().unwrap().geometry();
            Rect::new(g.loc.x, g.loc.y, g.size.w, g.size.h)
        };
        // The window rect its cells give it, before any max clamp; the output has no panels.
        let cells_window = |d: &CalloopData, id| {
            let t = d.state.layout.tile(id).unwrap();
            let full = Rect::new(0, 0, 1280, 720);
            let o = crate::layout::Output {
                name: t.output.clone(),
                area: full,
                full,
                priority: 0,
            };
            let r = o.cells_rect(t.cells);
            let top = STRIP + if d.state.layout.has_bar(id) { BAR } else { 0 };
            (r.w - GAP, r.h - top - (GAP - STRIP))
        };
        let overlapped = |d: &CalloopData, id| {
            let a = d.state.layout.tile(id).unwrap();
            d.state.layout.tiles().iter().any(|b| {
                let (a, b) = (a.cells, b.cells);
                b != a
                    && a.col < b.col + b.cols
                    && b.col < a.col + a.cols
                    && a.row < b.row + b.rows
                    && b.row < a.row + a.rows
            })
        };

        // AC1: no hints is Hints::default(); the neighbour stays mapped throughout.
        let plain = create("plain", (0, 0, 320, 240), None);
        map(plain);
        pump(&mut event_loop, &mut data, |d| tile_of(d, plain).is_some());
        let neighbour = tile_of(&data, plain).unwrap();
        assert_eq!(
            data.state.layout.tile(neighbour).unwrap().hints,
            Hints::default()
        );
        // Negative is no limit, above 65535 is capped, a max below its min is dropped.
        let raw = hints(Some((-5, 300)), Some((70_000, 200)));
        let odd = create("odd", (0, 0, 320, 240), Some(raw));
        map(odd);
        pump(&mut event_loop, &mut data, |d| tile_of(d, odd).is_some());
        let t = data
            .state
            .layout
            .tile(tile_of(&data, odd).unwrap())
            .unwrap();
        assert_eq!(t.hints, Hints::new((-5, 300), (70_000, 200)));
        assert_eq!(
            t.hints,
            Hints {
                min: (0, 300),
                max: (65535, 0)
            }
        );
        destroy(&mut event_loop, &mut data, odd);

        // AC2: a fixed size that fits is configured to exactly that size.
        let fixed = hints(Some((400, 300)), Some((400, 300)));
        let win = create("fixed", (0, 0, 320, 240), Some(fixed));
        map(win);
        pump(&mut event_loop, &mut data, |d| tile_of(d, win).is_some());
        let id = tile_of(&data, win).unwrap();
        let t = data.state.layout.tile(id).unwrap();
        assert_eq!(t.hints, Hints::new((400, 300), (400, 300)));
        assert!(!t.floating);
        let rect = data.state.layout.rect(id).unwrap();
        assert_eq!((rect.w, rect.h), (400, 300));
        pump(&mut event_loop, &mut data, |d| geometry(d, win) == rect);
        let (w, h) = cells_window(&data, id);
        assert!(w >= 400 && h >= 300, "{w} × {h}");
        assert!(!overlapped(&data, id));
        destroy(&mut event_loop, &mut data, win);

        // AC3: too big for any tile floats inside the output and refuses maximize.
        let huge = hints(Some((2000, 1500)), None);
        let win = create("huge", (0, 0, 320, 240), Some(huge));
        map(win);
        pump(&mut event_loop, &mut data, |d| tile_of(d, win).is_some());
        let id = tile_of(&data, win).unwrap();
        let t = data.state.layout.tile(id).unwrap();
        assert!(t.floating);
        assert_eq!(t.cells, crate::layout::Cells::default());
        let rect = data.state.layout.rect(id).unwrap();
        pump(&mut event_loop, &mut data, |d| geometry(d, win) == rect);
        let clip = data.state.layout.clip(id).unwrap();
        assert!(clip.x >= 0 && clip.y >= 0 && clip.x + clip.w <= 1280 && clip.y + clip.h <= 720);
        let atom = |name: &[u8]| conn.intern_atom(false, name).unwrap().reply().unwrap().atom;
        let (net_state, horz, vert) = (
            atom(b"_NET_WM_STATE"),
            atom(b"_NET_WM_STATE_MAXIMIZED_HORZ"),
            atom(b"_NET_WM_STATE_MAXIMIZED_VERT"),
        );
        conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ClientMessageEvent::new(32, win, net_state, [1, horz, vert, 1, 0]),
        )
        .unwrap();
        sync(&mut event_loop, &mut data, win);
        assert!(!data.state.layout.is_maximized(id));
        assert!(data.state.layout.tile(id).unwrap().floating);
        destroy(&mut event_loop, &mut data, win);

        // AC4: a late minimum arrives through property_notify and re-places the tile.
        let win = create("late", (0, 0, 320, 240), None);
        map(win);
        pump(&mut event_loop, &mut data, |d| tile_of(d, win).is_some());
        let id = tile_of(&data, win).unwrap();
        let before = data.state.layout.rect(id).unwrap();
        let min = (before.w + 100, 100);
        assert!(min.0 <= 1000, "{before:?}");
        let late = hints(Some(min), None);
        set(win, &late);
        pump(&mut event_loop, &mut data, |d| {
            d.state.layout.tile(id).unwrap().hints.min == min
        });
        let (w, h) = cells_window(&data, id);
        assert!(w >= min.0 && h >= min.1, "{w} × {h}");
        assert!(!overlapped(&data, id));
        let rect = data.state.layout.rect(id).unwrap();
        pump(&mut event_loop, &mut data, |d| geometry(d, win) == rect);
        // The same hints again change nothing.
        let i = managed(&data, win).unwrap();
        let (cells, refitted) = (
            data.state.layout.tile(id).unwrap().cells,
            data.state.windows[i].refitted,
        );
        set(win, &late);
        sync(&mut event_loop, &mut data, win);
        assert_eq!(data.state.layout.tile(id).unwrap().cells, cells);
        assert_eq!(data.state.windows[i].refitted, refitted);
        assert!(!data.state.windows[i].refit_queued);
        // Too big floats it; no minimum tiles it again. Each waits on the queued refit.
        set(win, &huge);
        pump(&mut event_loop, &mut data, |d| {
            d.state.layout.tile(id).unwrap().floating
        });
        set(win, &hints(None, None));
        pump(&mut event_loop, &mut data, |d| {
            !d.state.layout.tile(id).unwrap().floating
        });
        let t = data.state.layout.tile(id).unwrap();
        assert_eq!(t.hints, Hints::default());
        assert_ne!(t.cells, crate::layout::Cells::default());
        assert!(!overlapped(&data, id));
        let rect = data.state.layout.rect(id).unwrap();
        pump(&mut event_loop, &mut data, |d| geometry(d, win) == rect);
        destroy(&mut event_loop, &mut data, win);

        // AC5: changes within REFIT_PACE coalesce; the timer applies the hints current then.
        let win = create("paced", (0, 0, 320, 240), None);
        map(win);
        pump(&mut event_loop, &mut data, |d| tile_of(d, win).is_some());
        let id = tile_of(&data, win).unwrap();
        let i = managed(&data, win).unwrap();
        set(win, &hints(Some((300, 100)), None));
        pump(&mut event_loop, &mut data, |d| {
            d.state.layout.tile(id).unwrap().hints.min == (300, 100)
        });
        let first = data.state.windows[i].refitted.unwrap();
        for w in [310, 320, 330] {
            set(win, &hints(Some((w, 100)), None));
        }
        let mut seen = Vec::new();
        pump(&mut event_loop, &mut data, |d| {
            let min = d.state.layout.tile(id).unwrap().hints.min;
            if seen.last() != Some(&min) {
                seen.push(min);
            }
            min == (330, 100)
        });
        assert_eq!(
            seen,
            [(300, 100), (330, 100)],
            "no intermediate hints applied"
        );
        let second = data.state.windows[i].refitted.unwrap();
        assert!(
            second - first >= Duration::from_secs(1),
            "{:?}",
            second - first
        );
        assert!(!data.state.windows[i].refit_queued);
        destroy(&mut event_loop, &mut data, win);

        // AC6: a transient dialog ignores its hints: no tile, its own size, centred on its parent.
        let parent = data
            .state
            .space
            .element_geometry(&data.state.window_of(neighbour).unwrap())
            .unwrap();
        for min in [(160, 100), (2000, 1500)] {
            let tiles = data.state.layout.tiles().len();
            let dialog = create(
                "dialog",
                (0, 0, 200, 120),
                Some(hints(Some(min), Some(min))),
            );
            conn.change_property32(
                PropMode::REPLACE,
                dialog,
                AtomEnum::WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                &[plain],
            )
            .unwrap();
            map(dialog);
            pump(&mut event_loop, &mut data, |d| managed(d, dialog).is_some());
            let at = parent.loc + (parent.size - (200, 120).into()).downscale(2).to_point();
            pump(&mut event_loop, &mut data, |d| {
                geometry(d, dialog) == Rect::new(at.x, at.y, 200, 120)
            });
            assert_eq!(tile_of(&data, dialog), None);
            assert_eq!(data.state.layout.tiles().len(), tiles);
            // A late change is ignored too.
            set(dialog, &hints(Some((500, 400)), None));
            sync(&mut event_loop, &mut data, dialog);
            assert_eq!(tile_of(&data, dialog), None);
            assert_eq!(geometry(&data, dialog), Rect::new(at.x, at.y, 200, 120));
            destroy(&mut event_loop, &mut data, dialog);
        }
        // An override-redirect menu with hints keeps its X11 geometry.
        data.state.focus(data.state.window_of(neighbour));
        let menu = create(
            "menu",
            (70, 80, 100, 60),
            Some(hints(Some((500, 400)), None)),
        );
        map(menu);
        pump(&mut event_loop, &mut data, |d| managed(d, menu).is_some());
        assert_eq!(tile_of(&data, menu), None);
        assert_eq!(geometry(&data, menu), Rect::new(70, 80, 100, 60));
        destroy(&mut event_loop, &mut data, menu);
        assert_eq!(
            data.state.layout.set_hints(neighbour, (0, 0), (0, 0)),
            Fit::Unchanged,
            "the neighbour's hints were never touched"
        );
        destroy(&mut event_loop, &mut data, plain);
    }
}
