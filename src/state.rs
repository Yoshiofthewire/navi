use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_shell,
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
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Client, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Logical, Point, SERIAL_COUNTER, Serial, Size},
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
        },
        shm::{ShmHandler, ShmState},
    },
};
use tracing::info;

use crate::layout::{Drag, Layout, WinId};

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
    pub windows: Vec<Managed>,
    /// A drag in navi's strip or edges, from press to release.
    pub drag: Option<Drag>,

    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
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

        Ok(Self {
            start_time: std::time::Instant::now(),
            compositor_state: CompositorState::new::<Self>(&dh),
            xdg_shell_state: XdgShellState::new::<Self>(&dh),
            shm_state: ShmState::new::<Self>(&dh, vec![]),
            _output_manager_state:
                smithay::wayland::output::OutputManagerState::new_with_xdg_output::<Self>(&dh),
            data_device_state: DataDeviceState::new::<Self>(&dh),
            display_handle: dh,
            space: Space::default(),
            loop_signal,
            layout: Layout::default(),
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

    /// Sends every tiled window its layout size and moves it to its rect.
    pub fn apply_layout(&mut self) {
        for m in &self.windows {
            let (Some(id), Some(toplevel)) = (m.tile, m.window.toplevel()) else {
                continue;
            };
            let Some(r) = self.layout.rect(id) else {
                continue;
            };
            toplevel.with_pending_state(|s| s.size = Some((r.w, r.h).into()));
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
            self.space.map_element(m.window.clone(), (r.x, r.y), false);
        }
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

    /// First commit: the app ID is known, so tile it (or float it, for a dialog).
    fn map_new(&mut self, idx: usize, surface: &ToplevelSurface) {
        let app = with_states(surface.wl_surface(), |s| {
            s.data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok()?.app_id.clone())
        })
        .unwrap_or_else(|| "unknown".into());

        if surface.parent().is_some() {
            info!(app, "dialog: floating over its parent");
        } else {
            let pointer = self
                .seat
                .get_pointer()
                .map(|p| p.current_location().to_i32_round())
                .unwrap_or_default();
            if let Some((id, step)) = self.layout.place(&app, (pointer.x, pointer.y)) {
                self.windows[idx].tile = Some(id);
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
            }
        }
        // Sets the size before the first configure goes out.
        self.apply_layout();
        surface.send_configure();
        self.recenter_dialogs();

        let window = self.windows[idx].window.clone();
        self.space.raise_element(&window, true);
        if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(
                self,
                Some(surface.wl_surface().clone()),
                SERIAL_COUNTER.next_serial(),
            );
        }
    }

    /// Dialogs float centred over their parent, or over the output when the parent is unknown.
    fn recenter_dialogs(&mut self) {
        for m in self.windows.iter().filter(|m| m.tile.is_none()) {
            let Some(toplevel) = m.window.toplevel() else {
                continue;
            };
            if !toplevel.is_initial_configure_sent() {
                continue;
            }
            let over = toplevel
                .parent()
                .and_then(|p| {
                    self.space
                        .elements()
                        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &p))
                })
                .and_then(|w| self.space.element_geometry(w))
                .or_else(|| {
                    let o = self.space.outputs().next()?;
                    self.space.output_geometry(o)
                });
            if let Some(over) = over {
                let size = m.window.geometry().size;
                let loc = over.loc + (over.size - size).downscale(2).to_point();
                self.space.map_element(m.window.clone(), loc, true);
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
                    let app = with_states(&root, |s| {
                        s.data_map
                            .get::<XdgToplevelSurfaceData>()
                            .and_then(|d| d.lock().ok()?.app_id.clone())
                    });
                    info!(?app, w = size.w, h = size.h, "client committed size");
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
        if let Some(id) = m.tile {
            self.layout.remove(id);
            info!(id, "closed; its cells stay empty");
        }
        self.space.unmap_elem(&m.window);
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
delegate_seat!(Navi);
delegate_data_device!(Navi);
delegate_output!(Navi);
