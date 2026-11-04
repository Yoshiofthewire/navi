use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        PopupKind, PopupManager, Space, Window, WindowSurfaceType, find_popup_root_surface,
        get_popup_toplevel_coords,
    },
    input::{
        Seat, SeatHandler, SeatState,
        pointer::{CursorImageStatus, MotionEvent},
    },
    reexports::{
        calloop::LoopSignal,
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as Decoration,
            shell::server::xdg_toplevel,
        },
        wayland_server::{
            Client, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial, Size},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent,
            is_sync_subsurface, with_states,
        },
        output::OutputHandler,
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
                set_data_device_focus,
            },
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
            decoration::{XdgDecorationHandler, XdgDecorationState},
        },
        shm::{ShmHandler, ShmState},
    },
};
use tracing::info;

use crate::layout::{Decor, Drag, Layout, WinId, decoration};
use crate::persist::Store;

/// A toplevel navi knows about. It's unmapped until its first commit, when its app ID is known.
pub struct Managed {
    pub window: Window,
    /// `None` for dialogs, which float over their parent.
    pub tile: Option<WinId>,
    last_size: Size<i32, Logical>,
}

pub struct Navi {
    pub start_time: std::time::Instant,
    pub display_handle: DisplayHandle,
    pub space: Space<Window>,
    pub loop_signal: LoopSignal,
    pub layout: Layout,
    pub memory: Store,
    pub windows: Vec<Managed>,
    /// A drag in navi's strip or edges, from press to release.
    pub drag: Option<Drag>,

    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    _xdg_decoration_state: XdgDecorationState,
    shm_state: ShmState,
    _output_manager_state: smithay::wayland::output::OutputManagerState,
    seat_state: SeatState<Navi>,
    data_device_state: DataDeviceState,
    pub popups: PopupManager,
    pub seat: Seat<Navi>,
}

impl Navi {
    pub fn new(
        dh: DisplayHandle,
        loop_signal: LoopSignal,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 200, 25)?;
        seat.add_pointer();
        // Before the first window is placed.
        let mut layout = Layout::default();
        let memory = Store::open(&mut layout);

        Ok(Self {
            start_time: std::time::Instant::now(),
            compositor_state: CompositorState::new::<Self>(&dh),
            // Only what navi does: clients hide fullscreen and window-menu entries.
            xdg_shell_state: XdgShellState::new_with_capabilities::<Self>(
                &dh,
                [
                    xdg_toplevel::WmCapabilities::Maximize,
                    xdg_toplevel::WmCapabilities::Minimize,
                ],
            ),
            _xdg_decoration_state: XdgDecorationState::new::<Self>(&dh),
            shm_state: ShmState::new::<Self>(&dh, vec![]),
            _output_manager_state:
                smithay::wayland::output::OutputManagerState::new_with_xdg_output::<Self>(&dh),
            data_device_state: DataDeviceState::new::<Self>(&dh),
            display_handle: dh,
            space: Space::default(),
            loop_signal,
            layout,
            memory,
            windows: Vec::new(),
            drag: None,
            seat_state,
            popups: PopupManager::default(),
            seat,
        })
    }

    pub fn now_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    pub fn pointer_px(&self) -> (i32, i32) {
        px(self
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default())
    }

    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space
            .element_under(pos)
            .and_then(|(window, location)| {
                window
                    .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(s, p)| (s, (p + location).to_f64()))
            })
    }

    /// Sends every visible tiled window its layout size and state and moves it to its rect;
    /// unmaps hidden ones, and their dialogs.
    pub fn apply_layout(&mut self) {
        let (mut hidden, mut shown) = (Vec::new(), Vec::new());
        for m in &self.windows {
            let (Some(id), Some(toplevel)) = (m.tile, m.window.toplevel()) else {
                continue;
            };
            let mapped = self.space.element_location(&m.window).is_some();
            let rect = self.layout.rect(id).filter(|_| self.layout.visible(id));
            let Some(r) = rect else {
                if mapped {
                    self.space.unmap_elem(&m.window);
                    m.window.set_activated(false);
                    hidden.push(id);
                }
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
                continue;
            };
            let maximized = self.layout.is_maximized(id);
            toplevel.with_pending_state(|s| {
                s.size = Some((r.w, r.h).into());
                match maximized {
                    true => s.states.set(xdg_toplevel::State::Maximized),
                    false => s.states.unset(xdg_toplevel::State::Maximized),
                };
            });
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
            if !mapped {
                shown.push(id);
            }
            self.space.map_element(m.window.clone(), (r.x, r.y), false);
        }
        if !hidden.is_empty() {
            info!(?hidden, "hidden");
        }
        self.recenter_dialogs();
        self.keep_focus_mapped(&shown);
        // Tiles moved: refocus a still pointer, so a click can't land in the window that left.
        if let Some(pointer) = self.seat.get_pointer() {
            let pos = pointer.current_location();
            let under = self.pointer_focus(pos);
            let motion = MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time: self.now_ms() as u32,
            };
            pointer.motion(self, under, &motion);
            pointer.frame(self);
        }
    }

    /// What gets pointer focus at `pos`: nothing over navi's zones or during a drag.
    pub fn pointer_focus(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        match self.drag.is_some() || self.layout.hit(px(pos)).is_some() {
            true => None,
            false => self.surface_under(pos),
        }
    }

    fn is_mapped(&self, s: &WlSurface) -> bool {
        self.space
            .elements()
            .any(|w| w.toplevel().is_some_and(|t| t.wl_surface() == s))
    }

    /// The tile of the toplevel `s`; `None` for dialogs and windows not placed yet.
    fn tile_of(&self, s: &WlSurface) -> Option<WinId> {
        self.windows
            .iter()
            .find(|m| m.window.toplevel().is_some_and(|t| t.wl_surface() == s))?
            .tile
    }

    pub fn window_of(&self, id: WinId) -> Option<Window> {
        let m = self.windows.iter().find(|m| m.tile == Some(id))?;
        Some(m.window.clone())
    }

    /// Gives `window` keyboard focus and activates it, or nobody.
    pub fn focus(&mut self, window: Option<Window>) {
        let Some(kb) = self.seat.get_keyboard() else {
            return;
        };
        let surface = window
            .as_ref()
            .and_then(|w| w.toplevel())
            .map(|t| t.wl_surface().clone());
        match &window {
            Some(w) => self.space.raise_element(w, true),
            None => self.space.elements().for_each(|w| {
                w.set_activated(false);
            }),
        }
        if kb.current_focus() != surface {
            let focus = surface.as_ref().map(|s| match self.tile_of(s) {
                Some(id) => id.to_string(),
                None => "dialog".into(),
            });
            info!(?focus, "keyboard focus");
            kb.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        }
        self.space.elements().for_each(|w| {
            if let Some(t) = w.toplevel() {
                t.send_pending_configure();
            }
        });
    }

    /// Keyboard focus never rests on an unmapped window: it moves to the most recent of the
    /// windows just `shown`, or to nobody.
    fn keep_focus_mapped(&mut self, shown: &[WinId]) {
        let Some(focus) = self.seat.get_keyboard().and_then(|kb| kb.current_focus()) else {
            return;
        };
        if self.is_mapped(&focus) {
            return;
        }
        let next = shown
            .iter()
            .filter_map(|&id| self.layout.tile(id))
            .max_by_key(|t| t.stamp)
            .and_then(|t| self.window_of(t.id));
        self.focus(next);
    }

    /// Applies a client's maximize, unmaximize or minimize request through the same layout calls
    /// as navi's buttons. Dialogs, unplaced and hidden windows are refused.
    /// Returns whether it changed anything; `apply_layout` has then sent the configure.
    fn app_request(
        &mut self,
        s: &ToplevelSurface,
        what: &str,
        f: fn(&mut Layout, WinId) -> bool,
    ) -> bool {
        let id = self.tile_of(s.wl_surface());
        let changed = id.is_some_and(|id| f(&mut self.layout, id));
        if changed {
            info!(?id, by = "app", "{what}");
            self.apply_layout();
        } else {
            info!(?id, by = "app", refused = what, "request refused");
        }
        changed
    }

    /// Records a toplevel's decoration mode, as `layout::decoration` decides it.
    fn set_decoration(&mut self, toplevel: ToplevelSurface, requested: Decoration) {
        let mode = wire(decoration(toplevel.parent().is_some(), decor(requested)));
        let changed = toplevel.with_pending_state(|s| {
            let changed = s.decoration_mode != Some(mode);
            s.decoration_mode = Some(mode);
            changed
        });
        let app = app_id(toplevel.wl_surface());
        let id = self.tile_of(toplevel.wl_surface());
        info!(app, ?id, ?mode, "decoration");
        if let Some(id) = id {
            self.layout
                .set_decorated(id, mode == Decoration::ServerSide);
            // Sends the new mode with the new size.
            self.apply_layout();
        }
        // The protocol wants a reply even when nothing changed.
        if toplevel.is_initial_configure_sent() && (id.is_none() || !changed) {
            toplevel.send_configure();
        }
    }

    /// First commit: the app ID is known, so tile it (or float it, for a dialog).
    fn map_new(&mut self, idx: usize, surface: &ToplevelSurface) {
        let app = app_id(surface.wl_surface());

        // Re-applied now that the parent is known: set_parent may come after the mode request.
        let parent = surface.parent().is_some();
        let mode = surface.with_pending_state(|s| {
            let mode = s.decoration_mode.map(|m| decoration(parent, decor(m)));
            s.decoration_mode = mode.map(wire);
            mode
        });
        if parent {
            info!(app, ?mode, "dialog: floating over its parent");
        } else {
            let pointer = self
                .seat
                .get_pointer()
                .map(|p| p.current_location().to_i32_round())
                .unwrap_or_default();
            if let Some((id, step)) = self.layout.place(&app, (pointer.x, pointer.y)) {
                self.windows[idx].tile = Some(id);
                info!(app, id, ?mode, "decoration");
                self.layout.set_decorated(id, mode == Some(Decor::Server));
                surface.with_pending_state(|s| {
                    for t in [
                        xdg_toplevel::State::TiledLeft,
                        xdg_toplevel::State::TiledRight,
                        xdg_toplevel::State::TiledTop,
                        xdg_toplevel::State::TiledBottom,
                    ] {
                        s.states.set(t);
                    }
                });
                let tile = self.layout.tile(id).map(|t| (t.output.clone(), t.cells));
                let rect = self.layout.rect(id);
                info!(app, id, ?step, ?tile, ?rect, "placed");
                self.memory.sync(&self.layout);
            }
        }
        // Sets the size before the first configure goes out.
        self.apply_layout();
        surface.send_configure();
        self.recenter_dialogs();

        let window = self.windows[idx].window.clone();
        if self.space.element_location(&window).is_some() {
            self.focus(Some(window));
        }
    }

    /// Dialogs float centred over their parent while it is mapped, and are unmapped while it
    /// isn't. A dialog whose parent is gone floats over the output.
    fn recenter_dialogs(&mut self) {
        for m in self.windows.iter().filter(|m| m.tile.is_none()) {
            let Some(toplevel) = m.window.toplevel() else {
                continue;
            };
            if !toplevel.is_initial_configure_sent() {
                continue;
            }
            // A tiled parent's layout rect: its committed size lags a maximize.
            let over = match toplevel.parent() {
                Some(p) if self.is_mapped(&p) => {
                    match self.tile_of(&p).and_then(|id| self.layout.rect(id)) {
                        Some(r) => Some(Rectangle::new((r.x, r.y).into(), (r.w, r.h).into())),
                        None => self
                            .space
                            .elements()
                            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &p))
                            .and_then(|w| self.space.element_geometry(w)),
                    }
                }
                Some(_) => None,
                None => self
                    .space
                    .outputs()
                    .next()
                    .and_then(|o| self.space.output_geometry(o)),
            };
            match over {
                Some(over) => {
                    let size = m.window.geometry().size;
                    let loc = over.loc + (over.size - size).downscale(2).to_point();
                    self.space.map_element(m.window.clone(), loc, false);
                }
                None if self.space.element_location(&m.window).is_some() => {
                    self.space.unmap_elem(&m.window);
                    m.window.set_activated(false);
                    toplevel.send_pending_configure();
                    info!(
                        app = app_id(toplevel.wl_surface()),
                        "dialog hidden with its parent"
                    );
                }
                None => {}
            }
        }
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
        else {
            return;
        };
        let (Some(output), Some(window_geo)) = (
            self.space.outputs().next(),
            self.space.element_geometry(window),
        ) else {
            return;
        };
        let Some(mut target) = self.space.output_geometry(output) else {
            return;
        };
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;
        popup.with_pending_state(|s| {
            s.geometry = s.positioner.get_unconstrained_geometry(target);
        });
    }
}

fn decor(m: Decoration) -> Decor {
    match m {
        Decoration::ServerSide => Decor::Server,
        _ => Decor::Client,
    }
}

fn wire(d: Decor) -> Decoration {
    match d {
        Decor::Server => Decoration::ServerSide,
        Decor::Client => Decoration::ClientSide,
    }
}

fn app_id(s: &WlSurface) -> String {
    with_states(s, |s| {
        s.data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|d| d.lock().ok()?.app_id.clone())
    })
    .unwrap_or_else(|| "unknown".into())
}

/// The logical pixel containing `p`.
pub fn px(p: Point<f64, Logical>) -> (i32, i32) {
    (p.x.floor() as i32, p.y.floor() as i32)
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl CompositorHandler for Navi {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("every client is inserted with ClientState")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(m) = self
                .windows
                .iter_mut()
                .find(|m| m.window.toplevel().is_some_and(|t| t.wl_surface() == &root))
            {
                m.window.on_commit();
                let size = m.window.geometry().size;
                if m.tile.is_some() && size != m.last_size {
                    m.last_size = size;
                    let app = app_id(&root);
                    info!(app, w = size.w, h = size.h, "client committed size");
                }
            }
        }

        if let Some(idx) = self.windows.iter().position(|m| {
            m.window
                .toplevel()
                .is_some_and(|t| t.wl_surface() == surface && !t.is_initial_configure_sent())
        }) && let Some(toplevel) = self.windows[idx].window.toplevel().cloned()
        {
            self.map_new(idx, &toplevel);
        } else if self.windows.iter().any(|m| {
            m.tile.is_none()
                && m.window
                    .toplevel()
                    .is_some_and(|t| t.wl_surface() == surface)
        }) {
            self.recenter_dialogs();
        }

        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(xdg)) = self.popups.find_popup(surface)
            && !xdg.is_initial_configure_sent()
            && let Err(e) = xdg.send_configure()
        {
            tracing::warn!("popup configure failed: {e}");
        }
    }
}

impl BufferHandler for Navi {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Navi {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for Navi {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.windows.push(Managed {
            window: Window::new_wayland_window(surface),
            tile: None,
            last_size: Size::default(),
        });
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let Some(idx) = self.windows.iter().position(|m| {
            m.window
                .toplevel()
                .is_some_and(|t| t.wl_surface() == surface.wl_surface())
        }) else {
            return;
        };
        let m = self.windows.remove(idx);
        self.space.unmap_elem(&m.window);
        if let Some(id) = m.tile {
            self.layout.remove(id);
            info!(id, "closed; its cells stay empty");
            // A tab, or the tiles a maximized one covered, may show now.
            self.apply_layout();
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    // Only navi's own strip and edges move and resize tiles; clients can't start a drag.
    fn move_request(&mut self, _: ToplevelSurface, _: wl_seat::WlSeat, _: Serial) {}

    fn resize_request(
        &mut self,
        _: ToplevelSurface,
        _: wl_seat::WlSeat,
        _: Serial,
        _: xdg_toplevel::ResizeEdge,
    ) {
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    // xdg-shell needs a configure in reply, with the state as it now is; a refused request
    // changed nothing, so it gets one here.
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if !self.app_request(&surface, "maximized", Layout::maximize)
            && surface.is_initial_configure_sent()
        {
            surface.send_configure();
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if !self.app_request(&surface, "unmaximized", Layout::unmaximize)
            && surface.is_initial_configure_sent()
        {
            surface.send_configure();
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        self.app_request(&surface, "minimized", Layout::minimize);
    }
}

impl XdgDecorationHandler for Navi {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration(toplevel, Decoration::ClientSide);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Decoration) {
        self.set_decoration(toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration(toplevel, Decoration::ClientSide);
    }
}

impl SeatHandler for Navi {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Navi> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client);
    }
}

impl SelectionHandler for Navi {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Navi {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for Navi {}
impl ServerDndGrabHandler for Navi {}
impl OutputHandler for Navi {}

delegate_compositor!(Navi);
delegate_shm!(Navi);
delegate_xdg_shell!(Navi);
delegate_xdg_decoration!(Navi);
delegate_seat!(Navi);
delegate_data_device!(Navi);
delegate_output!(Navi);
