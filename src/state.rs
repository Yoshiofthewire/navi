use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    backend::session::libseat::LibSeatSession,
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output,
    delegate_seat, delegate_shm, delegate_xdg_activation, delegate_xdg_decoration,
    delegate_xdg_shell,
    desktop::{
        LayerSurface, PopupKind, PopupManager, Space, Window, WindowSurfaceType,
        find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
    },
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::Keycode,
        pointer::{CursorImageStatus, MotionEvent},
    },
    output::Output,
    reexports::{
        calloop::{
            LoopHandle, LoopSignal, RegistrationToken,
            ping::Ping,
            timer::{TimeoutAction, Timer},
        },
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as Decoration,
            shell::server::xdg_toplevel,
        },
        wayland_protocols_wlr::foreign_toplevel::v1::server::{
            zwlr_foreign_toplevel_handle_v1::State as ListState,
            zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
        },
        wayland_server::{
            Client, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_output::WlOutput, wl_seat, wl_surface::WlSurface},
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
        shell::{
            wlr_layer::{
                Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
                WlrLayerShellState,
            },
            xdg::{
                PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
                XdgToplevelSurfaceData,
                decoration::{XdgDecorationHandler, XdgDecorationState},
            },
        },
        shm::{ShmHandler, ShmState},
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
    },
};
use tracing::{info, warn};

use crate::CalloopData;
use crate::activation::{self, may_issue, may_redeem};
use crate::focus::Focus;
use crate::layout::{self, Decor, Drag, Layout, Rect, WinId, decoration};
use crate::monitors::{BarHover, Hover};
use crate::persist::{self, Store};
use crate::toplevels::{self, Toplevels};
use crate::wallpaper::usable_area;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::xwayland_shell::XWaylandShellState;
use smithay::xwayland::{X11Wm, XWaylandClientData};

/// A toplevel navi knows about. It's unmapped until its first commit, when its app ID is known.
pub struct Managed {
    pub window: Window,
    /// `None` for dialogs, which float over their parent.
    pub tile: Option<WinId>,
    pub(crate) last_size: Size<i32, Logical>,
    /// Asked for fullscreen before it was placed.
    pub(crate) want_fullscreen: bool,
    pub(crate) x11_parent: Option<u32>,
}

pub struct Navi {
    pub lock: crate::locking::Lock,
    pub session_lock_state: smithay::wayland::session_lock::SessionLockManagerState,
    pub start_time: std::time::Instant,
    pub display_handle: DisplayHandle,
    pub space: Space<Window>,
    pub loop_signal: LoopSignal,
    /// Nested repaint signal; hardware keeps its DRM/vblank scheduling.
    pub redraw: Option<Ping>,
    pub layout: Layout,
    pub memory: Store,
    pub xwm: Option<X11Wm>,
    pub xwayland_shell: XWaylandShellState,
    pub windows: Vec<Managed>,
    /// A drag in navi's strip or edges, from press to release.
    pub drag: Option<Drag>,
    pub edge_clicks: layout::EdgeClicks,
    /// The layout's outputs, with the area layer surfaces leave for the grid.
    grid: Vec<layout::Output>,
    pub toplevels: Toplevels,
    pub loop_handle: LoopHandle<'static, CalloopData>,
    /// The seat session on real hardware, for VT switches; `None` nested.
    pub session: Option<LibSeatSession>,
    /// Keys whose press navi kept for itself; their release is kept too.
    pub suppressed: Vec<Keycode>,
    bar_hover: BarHover,
    hover_timer: Option<RegistrationToken>,
    /// The output the taskbar goes on, as last moved there.
    bar_output: Option<String>,

    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    _xdg_decoration_state: XdgDecorationState,
    layer_shell_state: WlrLayerShellState,
    xdg_activation_state: XdgActivationState,
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
        loop_handle: LoopHandle<'static, CalloopData>,
        loop_signal: LoopSignal,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 200, 25)?;
        seat.add_pointer();
        // Before the first window is placed.
        let mut layout = Layout::default();
        let memory = Store::open(&mut layout);
        // Only navi-bar sees the window list and layer shell; other clients don't even see them.
        dh.create_global::<Self, ZwlrForeignToplevelManagerV1, _>(3, toplevels::Filter);

        Ok(Self {
            lock: Default::default(),
            session_lock_state: smithay::wayland::session_lock::SessionLockManagerState::new::<
                Self,
                _,
            >(&dh, |c| {
                c.get_data::<ClientState>().is_some_and(|s| s.locker)
            }),
            start_time: std::time::Instant::now(),
            compositor_state: CompositorState::new::<Self>(&dh),
            // Only what navi does: clients hide window-menu entries.
            xdg_shell_state: XdgShellState::new_with_capabilities::<Self>(
                &dh,
                [
                    xdg_toplevel::WmCapabilities::Maximize,
                    xdg_toplevel::WmCapabilities::Minimize,
                    xdg_toplevel::WmCapabilities::Fullscreen,
                ],
            ),
            _xdg_decoration_state: XdgDecorationState::new::<Self>(&dh),
            layer_shell_state: WlrLayerShellState::new_with_filter::<Self, _>(&dh, |c| {
                privileged(c.get_data())
            }),
            xdg_activation_state: XdgActivationState::new::<Self>(&dh),
            shm_state: ShmState::new::<Self>(&dh, vec![]),
            _output_manager_state:
                smithay::wayland::output::OutputManagerState::new_with_xdg_output::<Self>(&dh),
            data_device_state: DataDeviceState::new::<Self>(&dh),
            display_handle: dh.clone(),
            space: Space::default(),
            loop_signal,
            redraw: None,
            layout,
            memory,
            xwm: None,
            xwayland_shell: XWaylandShellState::new::<Self>(&dh),
            windows: Vec::new(),
            drag: None,
            edge_clicks: layout::EdgeClicks::default(),
            grid: Vec::new(),
            toplevels: Toplevels::default(),
            loop_handle,
            session: None,
            suppressed: Vec::new(),
            bar_hover: BarHover::default(),
            hover_timer: None,
            bar_output: persist::load_bar(),
            seat_state,
            popups: PopupManager::default(),
            seat,
        })
    }

    pub fn request_redraw(&self) {
        if let Some(ping) = &self.redraw {
            ping.ping();
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    pub fn pointer_location(&self) -> Point<f64, Logical> {
        self.seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default()
    }

    pub fn pointer_px(&self) -> (i32, i32) {
        px(self.pointer_location())
    }

    fn window_surface_under(
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

    /// The topmost layer surface at `pos` on one of `layers`, and the surface under `pos` in it.
    /// The Top layer is skipped on an output with a fullscreen window.
    fn layer_under(
        &self,
        pos: Point<f64, Logical>,
        layers: [Layer; 2],
    ) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        let output = self.space.output_under(pos).next()?;
        let origin = self.space.output_geometry(output)?.loc;
        let fullscreen = self.layout.fullscreen_on(&identity(output));
        let map = layer_map_for_output(output);
        layers.into_iter().find_map(|l| {
            if l == Layer::Top && fullscreen {
                return None;
            }
            let layer = map.layer_under(l, pos - origin.to_f64())?;
            let loc = origin + map.layer_geometry(layer)?.loc;
            let (s, p) = layer.surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)?;
            Some((layer.clone(), s, (p + loc).to_f64()))
        })
    }

    /// The layer surface a click at `pos` lands on: one above the grid, or one below it with no
    /// zone or window in between.
    pub fn layer_at(&self, pos: Point<f64, Logical>) -> Option<LayerSurface> {
        let lower = || {
            let clear =
                self.layout.hit(px(pos)).is_none() && self.space.element_under(pos).is_none();
            clear.then(|| self.layer_under(pos, [Layer::Bottom, Layer::Background]))?
        };
        self.layer_under(pos, [Layer::Overlay, Layer::Top])
            .or_else(lower)
            .map(|(l, ..)| l)
    }

    /// Sends every visible tiled window its layout size and state and moves it to its rect;
    /// unmaps hidden ones, and their dialogs.
    pub fn apply_layout(&mut self) {
        self.request_redraw();
        let (mut hidden, mut shown) = (Vec::new(), Vec::new());
        for m in &self.windows {
            let Some(id) = m.tile else {
                continue;
            };
            if let Some(x) = m.window.x11_surface() {
                let visible = self.layout.visible(id);
                let _ = x.set_maximized(self.layout.is_maximized(id));
                let _ = x.set_fullscreen(self.layout.is_fullscreen(id));
                let _ = x.set_suspended(self.layout.tile(id).is_some_and(|t| t.minimized));
                if let Some(r) = self.layout.rect(id).filter(|_| visible) {
                    if let Err(e) =
                        x.configure(Rectangle::new((r.x, r.y).into(), (r.w, r.h).into()))
                    {
                        warn!(id, "X11 configure: {e}");
                    }
                    if self.space.element_location(&m.window).is_none() {
                        shown.push(id);
                    }
                    self.space.map_element(m.window.clone(), (r.x, r.y), false);
                } else if self.space.element_location(&m.window).is_some() {
                    self.space.unmap_elem(&m.window);
                    m.window.set_activated(false);
                    hidden.push(id);
                }
                continue;
            }
            let Some(toplevel) = m.window.toplevel() else {
                continue;
            };
            let mapped = self.space.element_location(&m.window).is_some();
            let rect = self.layout.rect(id).filter(|_| self.layout.visible(id));
            let fullscreen = self.layout.is_fullscreen(id);
            let was = toplevel.with_pending_state(|s| {
                let was = s.states.contains(xdg_toplevel::State::Fullscreen);
                match fullscreen {
                    true => s.states.set(xdg_toplevel::State::Fullscreen),
                    false => s.states.unset(xdg_toplevel::State::Fullscreen),
                };
                was
            });
            if was != fullscreen {
                let what = if fullscreen {
                    "fullscreen"
                } else {
                    "unfullscreen"
                };
                info!(id, "{what}");
            }
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
        if !shown.is_empty() {
            info!(?shown, "shown");
        }
        self.recenter_dialogs();
        self.recenter_x11();
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

    /// What gets pointer focus at `pos`, top down: Overlay and Top layers, navi's zones (which
    /// get nothing), windows, then Bottom and Background layers. Nothing during a drag.
    pub fn pointer_focus(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.lock.active {
            let o = self.space.output_under(pos).next()?;
            let loc = self.space.output_geometry(o)?.loc.to_f64();
            return self
                .lock
                .surfaces
                .iter()
                .find(|(out, s)| out == o && s.alive())
                .map(|(_, s)| (s.wl_surface().clone(), loc));
        }
        if self.drag.is_some() {
            return None;
        }
        let surface =
            |l: Option<(LayerSurface, WlSurface, Point<f64, Logical>)>| l.map(|(_, s, p)| (s, p));
        if let Some(upper) = surface(self.layer_under(pos, [Layer::Overlay, Layer::Top])) {
            return Some(upper);
        }
        if self.layout.hit(px(pos)).is_some() {
            return None;
        }
        self.window_surface_under(pos)
            .or_else(|| surface(self.layer_under(pos, [Layer::Bottom, Layer::Background])))
    }

    /// Adds an output to the grid, all of it until layer surfaces reserve space.
    pub fn add_output(&mut self, output: &Output, priority: u32) {
        let none = Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
        self.grid.push(layout::Output {
            name: identity(output),
            area: none,
            full: none,
            priority,
        });
        self.arrange_layers(output);
    }

    /// Takes `output` away: its layer surfaces are closed (navi-bar makes new ones elsewhere) and
    /// its tiles move to the other outputs.
    pub fn remove_output(&mut self, output: &Output) {
        let name = identity(output);
        {
            let mut map = layer_map_for_output(output);
            for l in map.layers().cloned().collect::<Vec<_>>() {
                l.layer_surface().send_close();
                map.unmap_layer(&l);
            }
        }
        self.space.unmap_output(output);
        self.lock_outputs_changed();
        self.grid.retain(|o| o.name != name);
        self.layout.remove_output(&name);
        info!(output = name, "output removed");
        self.apply_layout();
    }

    /// New priorities by output name, lowest first.
    pub fn set_priorities(&mut self, order: &[String]) {
        for o in &mut self.grid {
            o.priority = order
                .iter()
                .position(|n| *n == o.name)
                .unwrap_or(order.len()) as u32;
        }
        self.layout.set_outputs(self.grid.clone());
        self.apply_layout();
    }

    /// Re-arranges `output`'s layer surfaces, and gives the grid what they leave of it.
    pub fn arrange_layers(&mut self, output: &Output) {
        self.request_redraw();
        let Some(geo) = self.space.output_geometry(output) else {
            return;
        };
        let zone = {
            let mut map = layer_map_for_output(output);
            map.arrange();
            map.non_exclusive_zone()
        };
        let rect = |loc: Point<i32, Logical>, size: Size<i32, Logical>| Rect {
            x: loc.x,
            y: loc.y,
            w: size.w,
            h: size.h,
        };
        let area = usable_area(rect(geo.loc, geo.size), rect(geo.loc + zone.loc, zone.size));
        let name = identity(output);
        let Some(o) = self.grid.iter_mut().find(|o| o.name == name) else {
            return;
        };
        let full = rect(geo.loc, geo.size);
        if (o.area, o.full) == (area, full) {
            return;
        }
        (o.area, o.full) = (area, full);
        self.lock_outputs_changed();
        info!(output = name, ?area, "grid area");
        self.layout.set_outputs(self.grid.clone());
        self.apply_layout();
    }

    /// The output whose layer map holds `surface`, a layer surface's root.
    fn layer_output(&self, surface: &WlSurface) -> Option<Output> {
        self.space
            .outputs()
            .find(|o| {
                layer_map_for_output(o)
                    .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .cloned()
    }

    /// A click on a layer surface: it gets the keyboard if it takes keyboard input at all.
    /// `Exclusive` counts as `OnDemand`, so no layer surface holds the keyboard unasked.
    pub fn click_layer(&mut self, layer: &LayerSurface) {
        if self.lock.active {
            return;
        }
        let Some(kb) = self.seat.get_keyboard() else {
            return;
        };
        let surface = layer.wl_surface().clone();
        if !layer.can_receive_keyboard_focus()
            || kb.current_focus().and_then(|f| f.surface()).as_ref() == Some(&surface)
        {
            return;
        }
        self.space.elements().for_each(|w| {
            w.set_activated(false);
            if let Some(t) = w.toplevel() {
                t.send_pending_configure();
            }
        });
        let focus = Some(format!("layer {}", layer.namespace()));
        info!(?focus, "keyboard focus");
        kb.set_focus(
            self,
            Some(Focus::Wayland(surface)),
            SERIAL_COUNTER.next_serial(),
        );
    }

    fn is_mapped(&self, s: &WlSurface) -> bool {
        self.space
            .elements()
            .any(|w| window_surface(w).as_ref() == Some(s))
    }

    /// A toplevel in the space, or a mapped layer surface.
    fn holds_focus(&self, s: &WlSurface) -> bool {
        self.is_mapped(s) || self.layer_output(s).is_some()
    }

    /// The tile of the toplevel `s`; `None` for dialogs and windows not placed yet.
    fn tile_of(&self, s: &WlSurface) -> Option<WinId> {
        self.windows
            .iter()
            .find(|m| window_surface(&m.window).as_ref() == Some(s))?
            .tile
    }

    /// The tile whose window holds the keyboard; `None` while a layer surface, a dialog or
    /// nothing does.
    pub fn focused_tile(&self) -> Option<WinId> {
        let focus = self.seat.get_keyboard()?.current_focus()?.surface()?;
        self.tile_of(&focus)
    }

    /// The output with navi-bar's taskbar.
    fn bar_on(&self) -> Option<String> {
        self.space
            .outputs()
            .find(|o| {
                layer_map_for_output(o)
                    .layers()
                    .any(|l| l.namespace() == BAR)
            })
            .map(identity)
    }

    /// Where a new taskbar goes: where it was last moved, if that's connected, else main.
    fn bar_target(&self) -> Option<Output> {
        let last = self.bar_output.as_ref();
        last.and_then(|n| self.space.outputs().find(|o| identity(o) == *n).cloned())
            .or_else(|| self.main_output())
    }

    /// Feeds the taskbar's hover band at `pos`; after a rest on another output's bottom edge the
    /// taskbar moves there.
    pub fn bar_hover(&mut self, pos: Point<f64, Logical>) {
        if self.lock.active {
            return;
        }
        let Some(bar) = self.bar_on() else {
            return;
        };
        let outputs: Vec<(String, Rect)> = self
            .space
            .outputs()
            .filter_map(|o| {
                let g = self.space.output_geometry(o)?;
                let r = Rect {
                    x: g.loc.x,
                    y: g.loc.y,
                    w: g.size.w,
                    h: g.size.h,
                };
                Some((identity(o), r))
            })
            .collect();
        let now = self.now_ms();
        match self.bar_hover.feed((pos.x, pos.y), &outputs, &bar, now) {
            Hover::Idle => {}
            Hover::Arm(at) => self.arm_hover(at.saturating_sub(now)),
            Hover::MoveTo(to) => self.move_bar(to),
        }
    }

    /// Re-feeds the pointer in `ms`, so a still pointer moves the taskbar too.
    fn arm_hover(&mut self, ms: u64) {
        if self.hover_timer.is_some() {
            return;
        }
        let timer = Timer::from_duration(std::time::Duration::from_millis(ms));
        let armed = self.loop_handle.insert_source(timer, |_, _, data| {
            let s = &mut data.state;
            s.hover_timer = None;
            let p = s.pointer_location();
            s.bar_hover(p);
            TimeoutAction::Drop
        });
        match armed {
            Ok(t) => self.hover_timer = Some(t),
            Err(e) => warn!("taskbar hover not timed: {e}"),
        }
    }

    /// Closes the taskbar where it is; navi-bar makes a new one, which maps on `to`.
    fn move_bar(&mut self, to: String) {
        info!(output = to, "taskbar moves");
        persist::save_bar(&to);
        self.bar_output = Some(to);
        self.close_layers(|_, ns| ns == BAR);
    }

    /// After the outputs change: the status band and notifications go back to the main output,
    /// and the taskbar to where it belongs. navi-bar remakes what is closed.
    pub fn place_shell(&mut self) {
        let main = self.main_output().map(|o| identity(&o));
        let bar = self.bar_target().map(|o| identity(&o));
        self.close_layers(|o, ns| match ns == BAR {
            true => Some(o) != bar.as_deref(),
            false => Some(o) != main.as_deref(),
        });
    }

    /// Closes and unmaps the layer surfaces `which(output, namespace)` picks.
    fn close_layers(&mut self, which: impl Fn(&str, &str) -> bool) {
        for o in self.space.outputs().cloned().collect::<Vec<_>>() {
            let name = identity(&o);
            let closed = {
                let mut map = layer_map_for_output(&o);
                let picked: Vec<LayerSurface> = map
                    .layers()
                    .filter(|l| which(&name, l.namespace()))
                    .cloned()
                    .collect();
                for l in &picked {
                    info!(namespace = l.namespace(), output = name, "layer closed");
                    l.layer_surface().send_close();
                    map.unmap_layer(l);
                }
                !picked.is_empty()
            };
            if closed {
                self.arrange_layers(&o);
            }
        }
    }

    /// The output layer surfaces go on when they name none: the main one, priority 0.
    pub fn main_output(&self) -> Option<Output> {
        self.space
            .outputs()
            .min_by_key(|o| self.priority_of(&identity(o)))
            .cloned()
    }

    pub fn priority_of(&self, output: &str) -> u32 {
        let g = self.grid.iter().find(|g| g.name == output);
        g.map_or(u32::MAX, |g| g.priority)
    }

    /// Focus or restore, from the bar or an activation token: `id` is shown in its group,
    /// restored, uncovered, and gets keyboard focus.
    pub fn focus_or_restore(&mut self, id: WinId) {
        if self.lock.active {
            return;
        }
        if self.layout.activate(id) {
            self.apply_layout();
        }
        if self.layout.visible(id) {
            self.focus(self.window_of(id));
        }
    }

    /// Sends the window list what changed: tiled windows in open order.
    pub fn refresh_toplevels(&mut self) {
        if self.lock.active {
            return;
        }
        let focus = self
            .seat
            .get_keyboard()
            .and_then(|kb| kb.current_focus())
            .and_then(|f| f.surface());
        let now = self
            .windows
            .iter()
            .filter_map(|m| {
                let id = m.tile?;
                let surface = window_surface(&m.window)?;
                let tile = self.layout.tile(id)?;
                let states = [
                    (ListState::Maximized, self.layout.is_maximized(id)),
                    (ListState::Minimized, tile.minimized),
                    (ListState::Activated, focus.as_ref() == Some(&surface)),
                    (ListState::Fullscreen, tile.fullscreen),
                ];
                let info = toplevels::Info {
                    title: window_title(&m.window),
                    app_id: window_app(&m.window),
                    output: self
                        .space
                        .outputs()
                        .find(|o| identity(o) == tile.output)
                        .cloned(),
                    states: states.into_iter().filter(|s| s.1).map(|s| s.0).collect(),
                };
                Some((id, info))
            })
            .collect();
        self.toplevels.refresh(&self.display_handle, now);
    }

    fn managed_mut(&mut self, s: &WlSurface) -> Option<&mut Managed> {
        self.windows
            .iter_mut()
            .find(|m| window_surface(&m.window).as_ref() == Some(s))
    }

    pub fn window_of(&self, id: WinId) -> Option<Window> {
        let m = self.windows.iter().find(|m| m.tile == Some(id))?;
        Some(m.window.clone())
    }

    pub fn close_window(&self, id: WinId) {
        if let Some(w) = self.window_of(id) {
            if let Some(t) = w.toplevel() {
                t.send_close();
            } else if let Some(x) = w.x11_surface()
                && let Err(e) = x.close()
            {
                warn!("X11 close: {e}");
            }
        }
    }

    /// Gives `window` keyboard focus and activates it, or nobody.
    pub fn focus(&mut self, window: Option<Window>) {
        if self.lock.active {
            return;
        }
        self.request_redraw();
        let Some(kb) = self.seat.get_keyboard() else {
            return;
        };
        let target = window.as_ref().and_then(|w| match w.x11_surface() {
            Some(x) => Some(Focus::X11(x.clone())),
            None => window_surface(w).map(Focus::Wayland),
        });
        let surface = target.as_ref().and_then(Focus::surface);
        if let Some(x) = window.as_ref().and_then(|w| w.x11_surface())
            && let Some(wm) = self.xwm.as_mut()
            && let Err(e) = wm.raise_window(x)
        {
            warn!("X11 raise: {e}");
        }
        match &window {
            Some(w) => self.space.raise_element(w, true),
            None => self.space.elements().for_each(|w| {
                w.set_activated(false);
            }),
        }
        if kb.current_focus() != target {
            let focus = surface.as_ref().map(|s| match self.tile_of(s) {
                Some(id) => id.to_string(),
                None => "dialog".into(),
            });
            info!(?focus, "keyboard focus");
            kb.set_focus(self, target, SERIAL_COUNTER.next_serial());
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
        let Some(focus) = self
            .seat
            .get_keyboard()
            .and_then(|kb| kb.current_focus())
            .and_then(|f| f.surface())
        else {
            return;
        };
        if self.holds_focus(&focus) {
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
                if self.windows[idx].want_fullscreen {
                    self.layout.fullscreen(id);
                }
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
                    .main_output()
                    .and_then(|o| self.space.output_geometry(&o)),
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
        let Some(window_geo) = self.space.element_geometry(window) else {
            return;
        };
        let Some(output) = self.space.output_under(window_geo.loc.to_f64()).next() else {
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

pub fn window_surface(w: &Window) -> Option<WlSurface> {
    w.wl_surface().map(std::borrow::Cow::into_owned)
}
/// Keep X11 text valid and small enough for Wayland's 4096-byte event buffer, including
/// the string length, terminator, padding, and event header.
fn x11_text(text: String) -> String {
    let mut text = text.replace('\0', "\u{fffd}");
    text.truncate(text.floor_char_boundary(2048));
    text
}

pub fn window_app(w: &Window) -> String {
    match w.x11_surface() {
        Some(x) => x11_text(x.class()),
        None => w
            .toplevel()
            .map(|t| app_id(t.wl_surface()))
            .unwrap_or_default(),
    }
}
pub fn window_title(w: &Window) -> String {
    match w.x11_surface() {
        Some(x) => x11_text(x.title()),
        None => w
            .toplevel()
            .map(|t| title(t.wl_surface()))
            .unwrap_or_default(),
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

fn title(s: &WlSurface) -> String {
    with_states(s, |s| {
        s.data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|d| d.lock().ok()?.title.clone())
    })
    .unwrap_or_default()
}

/// The monitor identity (from its EDID) an output is known by inside navi: layout, memory, the
/// wallpaper and the taskbar file key by it. Clients only see the output's name, its connector.
struct Identity(String);

/// `output`'s identity; nested, and before one is set, its name.
pub fn identity(output: &Output) -> String {
    output
        .user_data()
        .get::<Identity>()
        .map_or_else(|| output.name(), |i| i.0.clone())
}

pub fn set_identity(output: &Output, id: String) {
    output.user_data().insert_if_missing(|| Identity(id));
}

/// navi-bar's taskbar namespace; only navi-bar has layer shell, so it can be trusted.
const BAR: &str = "navi-bar";

/// The logical pixel containing `p`.
pub fn px(p: Point<f64, Logical>) -> (i32, i32) {
    (p.x.floor() as i32, p.y.floor() as i32)
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    /// Set only on the connection navi made for navi-bar; never for the listening socket.
    pub privileged: bool,
    /// Only the dedicated connection passed to the swaylock child.
    pub locker: bool,
}

/// The filter on privileged globals: layer shell and the window list.
pub fn privileged(client: Option<&ClientState>) -> bool {
    client.is_some_and(|c| c.privileged)
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
        if let Some(data) = client.get_data::<ClientState>() {
            &data.compositor_state
        } else {
            &client
                .get_data::<XWaylandClientData>()
                .expect("known client type")
                .compositor_state
        }
    }

    fn destroyed(&mut self, _: &WlSurface) {
        self.request_redraw();
    }

    fn commit(&mut self, surface: &WlSurface) {
        self.request_redraw();
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(m) = self
                .windows
                .iter_mut()
                .find(|m| window_surface(&m.window).as_ref() == Some(&root))
            {
                m.window.on_commit();
                let size = m.window.geometry().size;
                if m.tile.is_some() && size != m.last_size {
                    m.last_size = size;
                    let app = window_app(&m.window);
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

        if let Some(output) = self.layer_output(surface) {
            self.arrange_layers(&output);
            let initial_sent = with_states(surface, |s| {
                s.data_map
                    .get::<LayerSurfaceData>()
                    .is_some_and(|d| d.lock().is_ok_and(|d| d.initial_configure_sent))
            });
            let layer = layer_map_for_output(&output)
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .cloned();
            if let Some(l) = layer {
                if !initial_sent {
                    l.layer_surface().send_configure();
                }
                // A surface that stops taking keys loses the keyboard.
                let kb = self.seat.get_keyboard();
                let focused = kb.is_some_and(|kb| {
                    kb.current_focus().and_then(|f| f.surface()).as_ref() == Some(surface)
                });
                if focused && !l.can_receive_keyboard_focus() {
                    self.focus(None);
                }
            }
        }

        if self.lock.active
            && self
                .lock
                .surfaces
                .iter()
                .any(|(_, s)| s.alive() && s.wl_surface() == surface)
        {
            self.focus_lock_surface(Some(surface.clone()));
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
            want_fullscreen: false,
            x11_parent: None,
        });
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.request_redraw();
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

    // It stays on its own output, whatever output it names. `apply_layout` logs the change.
    fn fullscreen_request(&mut self, surface: ToplevelSurface, _: Option<WlOutput>) {
        let s = surface.wl_surface();
        if !surface.is_initial_configure_sent() {
            // Applied when it's placed.
            if let Some(m) = self.managed_mut(s) {
                m.want_fullscreen = true;
            }
            return;
        }
        match self.tile_of(s) {
            Some(id) if self.layout.fullscreen(id) => self.apply_layout(),
            id => {
                info!(?id, by = "app", refused = "fullscreen", "request refused");
                surface.send_configure();
            }
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let s = surface.wl_surface();
        if let Some(m) = self.managed_mut(s) {
            m.want_fullscreen = false;
        }
        match self.tile_of(s) {
            Some(id) if self.layout.unfullscreen(id) => self.apply_layout(),
            _ if surface.is_initial_configure_sent() => {
                surface.send_configure();
            }
            _ => {}
        }
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
    type KeyboardFocus = Focus;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Navi> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&Focus>) {
        let dh = &self.display_handle;
        let client = (!self.lock.active)
            .then_some(focused)
            .flatten()
            .and_then(Focus::surface)
            .and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client);
    }
}

impl SelectionHandler for Navi {
    type SelectionUserData = smithay::xwayland::xwm::XwmId;

    fn new_selection(
        &mut self,
        ty: smithay::wayland::selection::SelectionTarget,
        source: Option<smithay::wayland::selection::SelectionSource>,
        _: Seat<Self>,
    ) {
        if !self.lock.active
            && ty == smithay::wayland::selection::SelectionTarget::Clipboard
            && let Some(wm) = self.xwm.as_mut()
            && let Err(e) = wm.new_selection(ty, source.map(|s| s.mime_types()))
        {
            warn!("X11 clipboard offer: {e}");
        }
    }

    fn send_selection(
        &mut self,
        ty: smithay::wayland::selection::SelectionTarget,
        mime: String,
        fd: std::os::fd::OwnedFd,
        _: Seat<Self>,
        owner: &Self::SelectionUserData,
    ) {
        if !self.lock.active
            && ty == smithay::wayland::selection::SelectionTarget::Clipboard
            && let Some(wm) = self.xwm.as_mut().filter(|wm| wm.id() == *owner)
            && let Err(e) = wm.send_selection(ty, mime, fd, self.loop_handle.clone())
        {
            warn!("X11 clipboard transfer: {e}");
        }
    }
}

impl DataDeviceHandler for Navi {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl WlrLayerShellHandler for Navi {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    /// Maps the surface on the output it asked for, else the main one. Its first commit arranges
    /// it and sends the configure.
    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        let Some(output) =
            output
                .as_ref()
                .and_then(Output::from_resource)
                .or_else(|| match namespace == BAR {
                    true => self.bar_target(),
                    false => self.main_output(),
                })
        else {
            surface.send_close();
            return;
        };
        let name = identity(&output);
        let surface = LayerSurface::new(surface, namespace);
        let namespace = surface.namespace();
        match layer_map_for_output(&output).map_layer(&surface) {
            Ok(()) => info!(namespace, ?layer, output = name, "layer mapped"),
            Err(e) => {
                tracing::warn!(namespace, output = name, "layer not mapped, closed: {e}");
                surface.layer_surface().send_close();
            }
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        self.request_redraw();
        let Some(output) = self.layer_output(surface.wl_surface()) else {
            return;
        };
        {
            let mut map = layer_map_for_output(&output);
            let layer = map
                .layer_for_surface(surface.wl_surface(), WindowSurfaceType::TOPLEVEL)
                .cloned();
            if let Some(layer) = layer {
                info!(namespace = layer.namespace(), "layer unmapped");
                map.unmap_layer(&layer);
            }
        }
        self.arrange_layers(&output);
    }
}

impl XdgActivationHandler for Navi {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        if self.lock.active {
            return false;
        }
        self.xdg_activation_state
            .retain_tokens(|_, d| d.timestamp.elapsed() <= activation::TTL);
        let kb = self.seat.get_keyboard();
        let focus = kb
            .as_ref()
            .and_then(|kb| kb.current_focus())
            .and_then(|f| f.surface());
        let since = kb
            .and_then(|kb| kb.last_enter())
            .filter(|_| data.surface.is_some() && data.surface == focus)
            .map(u32::from);
        let serial = data.serial.as_ref().map(|(s, _)| u32::from(*s));
        let now = u32::from(SERIAL_COUNTER.next_serial());
        let issued = may_issue(serial, since, now);
        info!(
            app = data.app_id,
            ?serial,
            ?since,
            issued,
            "activation token"
        );
        issued
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        let first_use = self.xdg_activation_state.remove_token(&token);
        if self.lock.active {
            return;
        }
        let id = self.tile_of(&surface);
        let focus = self
            .seat
            .get_keyboard()
            .and_then(|kb| kb.current_focus())
            .and_then(|f| f.surface());
        let focus_kept =
            focus.is_some() && (focus == data.surface || focus.as_ref() == Some(&surface));
        let age = data.timestamp.elapsed();
        match id.filter(|_| may_redeem(first_use, age, focus_kept)) {
            Some(id) => {
                info!(id, by = "activation", "activate");
                self.focus_or_restore(id);
            }
            None => info!(?id, first_use, ?age, focus_kept, "activation refused"),
        }
    }
}

impl ClientDndGrabHandler for Navi {}
impl ServerDndGrabHandler for Navi {}
impl OutputHandler for Navi {}

delegate_compositor!(Navi);
delegate_shm!(Navi);
delegate_xdg_shell!(Navi);
delegate_xdg_decoration!(Navi);
delegate_layer_shell!(Navi);
delegate_seat!(Navi);
delegate_data_device!(Navi);
delegate_output!(Navi);
delegate_xdg_activation!(Navi);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac1_only_navis_own_connection_is_privileged() {
        assert!(!ClientState::default().privileged);
        assert!(!privileged(Some(&ClientState::default())));
        assert!(!privileged(None));
        let bar = ClientState {
            privileged: true,
            ..Default::default()
        };
        assert!(privileged(Some(&bar)));
    }
}
