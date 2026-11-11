use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    backend::session::libseat::LibSeatSession,
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output,
    delegate_seat, delegate_shm, delegate_xdg_activation, delegate_xdg_decoration,
    delegate_xdg_shell,
    desktop::{
        LayerSurface, PopupKind, PopupManager, Space, Window, WindowSurfaceType,
        find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
        utils::under_from_surface_tree,
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
                KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData,
                WlrLayerShellHandler, WlrLayerShellState,
            },
            xdg::{
                PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface,
                XdgPopupSurfaceData, XdgShellHandler, XdgShellState, XdgToplevelSurfaceData,
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
use crate::focus::{Focus, HandBack, Handoff, may_hand_back};
use crate::layout::dialog::{self, Root};
use crate::layout::{self, Decor, Drag, Fit, Layout, Rect, WinId, decoration};
use crate::monitors::{BarHover, Hover};
use crate::persist::{self, Store};
use crate::toplevels::{self, Toplevels};
use crate::wallpaper::usable_area;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::xwayland_shell::XWaylandShellState;
use smithay::xwayland::{X11Wm, XWaylandClientData};

/// A surface under the pointer, and where its origin is.
type Under = (WlSurface, Point<f64, Logical>);

/// A toplevel navi knows about. It's unmapped until its first commit, when its app ID is known.
pub struct Managed {
    pub window: Window,
    /// `None` for dialogs, which float over their parent.
    pub tile: Option<WinId>,
    pub(crate) last_size: Size<i32, Logical>,
    /// The geometry and clip rect last logged as clipped.
    pub(crate) clipped: Option<(Size<i32, Logical>, Rect)>,
    /// When its size hints were last applied, and whether a later refit is already timed.
    pub(crate) refitted: Option<std::time::Instant>,
    pub(crate) refit_queued: bool,
    /// Asked for fullscreen before it was placed.
    pub(crate) want_fullscreen: bool,
    /// Left fullscreen by Super+Esc: its fullscreen requests are refused until the user next
    /// presses a key or button on it.
    pub(crate) escaped: bool,
    pub(crate) x11_parent: Option<u32>,
    /// A dialog's parent window, as navi knows it: a gone parent's dialogs take its own.
    pub(crate) up: Option<Window>,
    /// An X11 transient's last requested size, before any cap.
    pub(crate) asked: Option<(i32, i32)>,
    /// A shown dialog's rect: where it's drawn and hit.
    pub(crate) placed: Option<Rect>,
    /// The output a dialog was last shown on, for when it becomes an orphan.
    pub(crate) last_output: Option<String>,
    /// A new dialog not shown yet: the focus rule runs when it first is.
    pub(crate) fresh: bool,
    /// A dialog refused the keyboard and not focused since: framed in accent, its root's dock
    /// entry and tab marked.
    pub(crate) waiting: bool,
    /// Its last raise (`Navi::raises` then): any focus that put it in front, a user's or a new
    /// window's (`Raise`); 0 if never.
    pub(crate) rank: u64,
    /// Its last raise by the user alone; the only rank a closed window's dialogs inherit.
    pub(crate) user_rank: u64,
    /// An orphan family's least place: `dialog::Stacked::floor`.
    pub(crate) floor: u64,
    /// `Navi::raises` when it appeared: a dialog's place within its family until it's raised.
    pub(crate) born: u64,
    /// The bounds a dialog's cap was last logged for.
    pub(crate) capped_for: Option<Rect>,
}

pub struct Navi {
    pub preferences: crate::settings::Preferences,
    pub desktop: serde_json::Value,
    pub screen_off: bool,
    pub sleep_requested: bool,
    pub sticky: crate::input::Sticky,
    pub swipe: Option<(f64, f64)>,
    pub last_activity: std::time::Instant,
    pub wallpaper_epoch: u64,
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
    pub(crate) grid: Vec<layout::Output>,
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
    /// The running navi-bar's control stream, for app menu toggles.
    pub bar_control: Option<std::os::unix::net::UnixStream>,
    pub super_tap: crate::input::SuperTap,
    /// Layers whose last commit asked for `Exclusive` keyboard focus.
    pub(crate) exclusive: crate::focus::Exclusive<WlSurface, Window>,
    /// navi's last return of the keyboard from a layer to a window, until focus moves again.
    pub(crate) handback: Option<(WlSurface, WlSurface)>,
    /// Whether the keyboard's holder got it from the user (`Raise::User`): tokens it mints then
    /// raise as the user would.
    pub(crate) focus_by_user: bool,
    /// Counts keyboard moves: a token is the user's only while the keyboard hasn't moved since
    /// it was made.
    pub(crate) focus_moves: u64,
    /// The window that held the keyboard before the one holding it now, whether the user gave
    /// it that focus, and `focus_moves` once the keyboard left it.
    pub(crate) before_focus: Option<(Window, bool, u64)>,
    /// The raise clock, in steps of 2 so an orphan's floor fits just below a raise.
    pub(crate) raises: u64,
    /// The window that held the keyboard when a click gave it to a layer.
    before_layer: crate::focus::BeforeLayer<Window>,

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
        let preferences = crate::settings::Preferences::load();
        let desktop =
            crate::desktop_config::load().unwrap_or_else(|_| crate::desktop_config::defaults());
        crate::theme::configure(&desktop);
        if seat
            .add_keyboard(
                crate::settings::xkb_config(&desktop),
                preferences.repeat_delay,
                preferences.repeat_rate,
            )
            .is_err()
        {
            warn!("configured keyboard layout unavailable; using the default layout");
            seat.add_keyboard(
                Default::default(),
                preferences.repeat_delay,
                preferences.repeat_rate,
            )?;
        }
        seat.add_pointer();
        // Before the first window is placed.
        let mut layout = Layout::default();
        let memory = Store::open(&mut layout);
        // Only navi-bar sees the window list and layer shell; other clients don't even see them.
        dh.create_global::<Self, ZwlrForeignToplevelManagerV1, _>(3, toplevels::Filter);

        Ok(Self {
            preferences,
            desktop,
            screen_off: false,
            sleep_requested: false,
            sticky: Default::default(),
            swipe: None,
            last_activity: std::time::Instant::now(),
            wallpaper_epoch: 0,
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
            bar_control: None,
            super_tap: Default::default(),
            exclusive: Default::default(),
            handback: None,
            raises: 0,
            focus_by_user: false,
            focus_moves: 0,
            before_focus: None,
            before_layer: Default::default(),
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

    fn window_surface_under(&self, pos: Point<f64, Logical>) -> Option<Under> {
        self.window_under(pos).map(|(_, under)| under)
    }

    /// The window at `pos` and its surface there, top down: floats and their dialogs above the
    /// rest. A window's own surfaces take input only inside its clip rect; its popups anywhere.
    pub fn window_under(&self, pos: Point<f64, Logical>) -> Option<(&Window, Under)> {
        let under = |w: &Window, clip: Option<Rect>| -> Option<Under> {
            let popup = self.popups(w).into_iter().find_map(|(s, loc, clip)| {
                let all = WindowSurfaceType::ALL;
                let (s, p) = clip
                    .contains(px(pos))
                    .then(|| under_from_surface_tree(&s, pos, loc, all))??;
                Some((s, p.to_f64()))
            });
            if popup.is_some() {
                return popup;
            }
            let loc = self.space.element_location(w)? - w.geometry().loc;
            let own = WindowSurfaceType::TOPLEVEL | WindowSurfaceType::SUBSURFACE;
            let inside = clip.is_none_or(|c| c.contains(px(pos)));
            let (s, p) = inside.then(|| w.surface_under(pos - loc.to_f64(), own))??;
            Some((s, (p + loc).to_f64()))
        };
        let floats = self.float_windows().into_iter().find_map(|w| {
            let clip = self.stacking(w).1;
            Some((w, under(w, clip)?))
        });
        floats.or_else(|| {
            self.space.elements().rev().find_map(|w| {
                let (float, clip) = self.stacking(w);
                (!float).then(|| Some((w, under(w, clip)?))).flatten()
            })
        })
    }

    /// `w`'s popups, top first: each surface, where its tree is drawn (global) and the rect it's
    /// drawn and hit inside: its configured geometry cut to the rect its window is kept in
    /// (`popup_bounds`). The client picks a popup's size, so the bound is what stops a popup
    /// without a grab from covering, or taking input meant for, a neighbouring tile.
    pub fn popups(&self, w: &Window) -> Vec<(WlSurface, Point<i32, Logical>, Rect)> {
        let (Some(loc), Some(t), Some(bounds)) = (
            self.space.element_location(w),
            w.toplevel(),
            self.popup_bounds(w),
        ) else {
            return Vec::new();
        };
        let add = |a: Point<i32, Logical>, b: Point<i32, Logical>| {
            Point::from((a.x.saturating_add(b.x), a.y.saturating_add(b.y)))
        };
        let popups = PopupManager::popups_for_surface(t.wl_surface());
        popups
            .filter_map(|(popup, offset)| {
                let size = with_states(popup.wl_surface(), |s| {
                    let d = s.data_map.get::<XdgPopupSurfaceData>()?;
                    Some(d.lock().ok()?.current.geometry.size)
                })?;
                let at = add(loc, offset);
                let clip = intersect(Rect::new(at.x, at.y, size.w, size.h), bounds)?;
                let origin = add(at, Point::from((0, 0)) - popup.geometry().loc);
                Some((popup.wl_surface().clone(), origin, clip))
            })
            .collect()
    }

    /// Where `w`'s popups are kept: its clip rect (a tile's, a float's, a dialog's), else (an X11
    /// menu, a window not placed) the output it's on.
    fn popup_bounds(&self, w: &Window) -> Option<Rect> {
        if let Some(clip) = self.stacking(w).1 {
            return Some(clip);
        }
        let g = self.space.element_geometry(w)?;
        let o = self.space.output_under(g.loc.to_f64()).next()?;
        let g = self.space.output_geometry(o)?;
        Some(Rect::new(g.loc.x, g.loc.y, g.size.w, g.size.h))
    }

    /// navi's zone at `pos`, unless a float's window or dialog drawn over it is there.
    pub fn zone_at(&self, pos: Point<f64, Logical>) -> Option<layout::Hit> {
        let over = self
            .window_under(pos)
            .and_then(|(w, _)| self.float_owner(w));
        self.layout.hit_under(px(pos), over)
    }

    /// The float `w` is, or whose dialog it is.
    pub fn float_owner(&self, w: &Window) -> Option<WinId> {
        let i = self.windows.iter().position(|m| &m.window == w)?;
        self.owner(i)
            .filter(|&id| self.layout.tile(id).is_some_and(|t| t.floating))
    }

    /// The windows that stack with the floats, top first: by their float's place in
    /// `Layout::floats`, and within one float (its dialogs) by the space's order.
    pub fn float_windows(&self) -> Vec<&Window> {
        let order = self.layout.floats();
        let rank = |w: &Window| {
            let id = self.float_owner(w);
            order
                .iter()
                .position(|&f| Some(f) == id)
                .unwrap_or(order.len())
        };
        let mut floats: Vec<&Window> = self
            .space
            .elements()
            .rev()
            .filter(|w| self.stacking(w).0)
            .collect();
        floats.sort_by_key(|w| rank(w));
        floats
    }

    /// Whether `w` stacks with the floats (a float or a float's dialog), and the rect its own
    /// surfaces are drawn and hit inside: its tile's clip or a dialog's rect. X11 menus have none.
    pub fn stacking(&self, w: &Window) -> (bool, Option<Rect>) {
        let Some(i) = self.windows.iter().position(|m| &m.window == w) else {
            return (false, None);
        };
        let m = &self.windows[i];
        let owner = self.owner(i).and_then(|id| self.layout.tile(id));
        let clip = match m.tile {
            Some(id) => self.layout.clip(id),
            None => m.placed,
        };
        (owner.is_some_and(|t| t.floating), clip)
    }

    /// A shown dialog's rect, which navi frames, and whether it waits for the user; `None` for
    /// every other window.
    pub fn dialog_rect(&self, w: &Window) -> Option<(Rect, bool)> {
        let m = self.windows.iter().find(|m| &m.window == w)?;
        Some((m.placed.filter(|_| m.tile.is_none())?, m.waiting))
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
            let clear = self.zone_at(pos).is_none() && self.window_under(pos).is_none();
            clear.then(|| self.layer_under(pos, [Layer::Bottom, Layer::Background]))?
        };
        self.layer_under(pos, [Layer::Overlay, Layer::Top])
            .or_else(lower)
            .map(|(l, ..)| l)
    }

    /// Sends every visible tiled window its layout size and state and moves it to its rect;
    /// unmaps hidden ones, and their dialogs. Every layout event ends here, so floats that free
    /// cells now hold tile first, unless a drag is in progress (its end catches up).
    pub fn apply_layout(&mut self) {
        self.request_redraw();
        if self.drag.is_none() {
            self.tile_floats();
        }
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
                    put(&mut self.space, &m.window, (r.x, r.y));
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
            put(&mut self.space, &m.window, (r.x, r.y));
        }
        if !hidden.is_empty() {
            info!(?hidden, "hidden");
        }
        if !shown.is_empty() {
            info!(?shown, "shown");
        }
        self.place_dialogs();
        self.place_menus();
        self.keep_focus_mapped(&shown);
        self.restack();
        self.hide_layer_focus();
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
        if self.zone_at(pos).is_some() {
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
        // A drag on this output would put its float back here once the output is gone.
        self.cancel_drag();
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

    /// Gives a layer surface the keyboard if it takes keyboard input at all: on a click, or when
    /// it asks for `Exclusive` (only navi-bar has layer shell). Otherwise no layer surface holds
    /// the keyboard unasked.
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
        self.before_layer.took(self.focused_window());
        let focus = Some(format!("layer {}", layer.namespace()));
        info!(?focus, "keyboard focus");
        kb.set_focus(
            self,
            Some(Focus::Wayland(surface)),
            SERIAL_COUNTER.next_serial(),
        );
    }

    /// The toplevel window holding the keyboard, if one does.
    pub(crate) fn focused_window(&self) -> Option<Window> {
        let focus = self.seat.get_keyboard()?.current_focus()?.surface()?;
        self.windows
            .iter()
            .find(|m| window_surface(&m.window).as_ref() == Some(&focus))
            .map(|m| m.window.clone())
    }

    /// A fullscreen window now covers the launcher holding the keyboard: give the keyboard back,
    /// as on close. The bar closes the menu when it loses the keyboard.
    fn hide_layer_focus(&mut self) {
        let Some(focus) = self
            .seat
            .get_keyboard()
            .and_then(|kb| kb.current_focus())
            .and_then(|f| f.surface())
        else {
            return;
        };
        let Some(output) = self.layer_output(&focus) else {
            return;
        };
        if self.lock.active || !self.layout.fullscreen_on(&identity(&output)) {
            return;
        }
        let space = &self.space;
        if let Some(back) = self
            .exclusive
            .hidden(&focus, |w| space.element_location(w).is_some())
        {
            info!("launcher covered by fullscreen: keyboard returned");
            self.focus(back);
        }
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

    /// The tile of the window holding the keyboard, a dialog counting as its parent's.
    fn focused_window_tile(&self) -> Option<WinId> {
        let focus = self.seat.get_keyboard()?.current_focus()?.surface()?;
        let i = self
            .windows
            .iter()
            .position(|m| window_surface(&m.window).as_ref() == Some(&focus))?;
        self.owner(i)
    }

    /// Window `i`'s tile, or a dialog's root's.
    fn owner(&self, i: usize) -> Option<WinId> {
        match self.root(i) {
            Root::Tiled(id) => Some(id),
            Root::Orphan => None,
        }
    }

    /// Window `i`'s root: itself if tiled, else up its dialog parents.
    fn root(&self, i: usize) -> Root {
        let up = |j: usize| {
            let up = self.windows.get(j)?.up.as_ref()?;
            self.windows.iter().position(|m| &m.window == up)
        };
        dialog::root(i, up, |j| self.windows.get(j)?.tile)
    }

    /// The root of the window holding the keyboard; `None` while a layer surface or nothing does.
    fn focus_root(&self) -> Option<Root> {
        let w = self.focused_window()?;
        Some(self.root(self.windows.iter().position(|m| m.window == w)?))
    }

    /// The output Super+Esc acts on now, if a window is fullscreen there.
    pub fn escape_target(&self) -> Option<String> {
        let pointer = self
            .space
            .output_under(self.pointer_location())
            .next()
            .map(identity);
        crate::input::escape_output(&self.layout, self.focused_window_tile(), pointer.as_deref())
    }

    /// The user pressed a key or button that reached `surface`: its window may go fullscreen
    /// again.
    pub fn user_pressed(&mut self, surface: Option<WlSurface>) {
        let Some(mut root) = surface else {
            return;
        };
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        if let Some(m) = self.managed_mut(&root) {
            m.escaped = false;
        }
    }

    /// The user took `id` out of fullscreen: it can't go back until a user press reaches it.
    pub fn guard_fullscreen(&mut self, id: WinId) {
        if let Some(m) = self.windows.iter_mut().find(|m| m.tile == Some(id)) {
            m.want_fullscreen = false;
            m.escaped = true;
        }
    }

    /// navi-bar activates `id`, by a dock click or key. A fullscreen window that ends gets the
    /// re-entry guard, so its app can't cover the window the user just chose.
    pub fn bar_activate(&mut self, id: WinId) {
        info!(id, by = "bar", "activate");
        for ended in self.layout.fullscreen_ended_by(id) {
            info!(id = ended, by = "bar", "fullscreen left");
            self.guard_fullscreen(ended);
        }
        self.focus_or_restore(id);
    }

    /// Refuses a fullscreen request from a window the user took out of fullscreen.
    pub fn refuse_escaped(&self, id: Option<WinId>) -> bool {
        let escaped = self
            .windows
            .iter()
            .any(|m| m.escaped && m.tile.is_some() && m.tile == id);
        if escaped {
            info!(
                ?id,
                by = "app",
                refused = "fullscreen",
                "request refused: user left fullscreen"
            );
        }
        escaped
    }

    /// The output with navi-bar's taskbar.
    pub fn bar_on(&self) -> Option<String> {
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
        if self.lock.active || self.desktop["dock_follow"] != true {
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
            self.focus_by(self.window_of(id), Raise::User);
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
                    attention: tile.attention,
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

    /// Gives `window` keyboard focus and activates it, or nobody: navi's own refocus, which never
    /// changes the stacking order.
    pub fn focus(&mut self, window: Option<Window>) {
        self.focus_by(window, Raise::Not);
    }

    /// `focus`, raising `window` (and so its family) to the front as `by` says.
    pub fn focus_by(&mut self, window: Option<Window>, by: Raise) {
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
            Some(w) => {
                // Activates it; `restack` puts it back in the one order.
                self.space.raise_element(w, true);
                if by != Raise::Not {
                    self.raises += 2;
                }
                let clock = self.raises;
                if let Some(m) = self.windows.iter_mut().find(|m| &m.window == w) {
                    m.waiting = false;
                    if by != Raise::Not {
                        m.rank = clock;
                    }
                    if by == Raise::User {
                        m.user_rank = clock;
                    }
                }
                self.restack();
                self.sync_attention();
                // A raised float (or a float's dialog) comes to the front of the floats' order;
                // `restack` gives every float its family's rank, which leads that order.
                if by != Raise::Not
                    && let Some(id) = self.float_owner(w)
                {
                    self.layout.raise(id);
                }
            }
            None => self.space.elements().for_each(|w| {
                w.set_activated(false);
            }),
        }
        let moved = kb.current_focus() != target;
        if moved {
            // A move from nobody clears it: a hand-back never skips past a gap.
            let user = self.focus_by_user;
            self.before_focus = self.focused_window().map(|w| (w, user, 0));
        }
        // `focus_changed` counts moves to a surface; this also counts the move to nobody.
        self.focus_moves += u64::from(moved);
        // navi's own refocus of the same window keeps what the user gave it.
        if moved || by == Raise::User {
            self.focus_by_user = by == Raise::User && target.is_some();
        }
        if moved {
            let focus = surface.as_ref().map(|s| match self.tile_of(s) {
                Some(id) => id.to_string(),
                None => "dialog".into(),
            });
            info!(?focus, "keyboard focus");
            kb.set_focus(self, target, SERIAL_COUNTER.next_serial());
            if let Some(b) = self.before_focus.as_mut() {
                b.2 = self.focus_moves;
            }
        }
        self.space.elements().for_each(|w| {
            if let Some(t) = w.toplevel() {
                t.send_pending_configure();
            }
        });
    }

    /// Keyboard focus never rests on an unmapped window: it moves to the most recent of the
    /// windows just `shown`, or to nobody.
    pub(crate) fn keep_focus_mapped(&mut self, shown: &[WinId]) {
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

    /// Records a toplevel's decoration mode, as `layout::decoration` decides it; `None` is no
    /// preference.
    fn set_decoration(&mut self, toplevel: ToplevelSurface, requested: Option<Decoration>) {
        let mode = wire(decoration(
            toplevel.parent().is_some(),
            requested.map(decor),
        ));
        let changed = toplevel.with_pending_state(|s| {
            let changed = s.decoration_mode != Some(mode);
            s.decoration_mode = Some(mode);
            changed
        });
        let app = app_id(toplevel.wl_surface());
        let id = self.tile_of(toplevel.wl_surface());
        info!(app, ?id, ?mode, "decoration");
        if let Some(id) = id {
            let fit = self
                .layout
                .set_decorated(id, mode == Decoration::ServerSide);
            self.report_fit(id, &app, fit);
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
            let mode = s
                .decoration_mode
                .map(|m| decoration(parent, Some(decor(m))));
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
            let (min, max) = size_hints(surface.wl_surface());
            let hints = layout::Hints::new(min, max);
            let decorated = mode == Some(Decor::Server);
            let at = (pointer.x, pointer.y);
            if let Some((id, step)) = self.layout.place_with(&app, at, hints, decorated) {
                if hints != layout::Hints::default() {
                    info!(app, id, min = ?hints.min, max = ?hints.max, "client size hints");
                }
                self.windows[idx].tile = Some(id);
                if self.windows[idx].want_fullscreen {
                    self.layout.fullscreen(id);
                }
                info!(app, id, ?mode, "decoration");
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
                if step == layout::Step::Floating {
                    self.log_float(id, &app);
                }
                self.memory.sync(&self.layout);
            }
        }
        // Sets the size before the first configure goes out.
        self.apply_layout();
        surface.send_configure();
        match self.windows[idx].tile {
            Some(_) => self.focus_new(idx),
            // Shown, and given the focus rule, at its first buffer.
            None => self.windows[idx].fresh = true,
        }
        self.recenter_dialogs();
    }

    /// A new window `i` takes the keyboard once shown; a dialog only from its own family.
    pub(crate) fn focus_new(&mut self, i: usize) {
        let m = &self.windows[i];
        let window = m.window.clone();
        if self.space.element_location(&window).is_none() {
            // A dialog under a hidden root can't take the keyboard: it waits, marked.
            if m.tile.is_none() {
                self.refuse(i);
            }
            return;
        }
        let dialog = m.tile.is_none();
        if dialog {
            let root = self.root(i);
            if !dialog::may_focus_on_map(root, self.focus_root()) {
                self.refuse(i);
                return;
            }
        }
        // A dialog takes the keyboard only from its own family, so it carries on whatever the
        // user gave that family; a new window of its own has nothing from the user.
        let user = dialog && self.focus_by_user;
        self.focus_by(Some(window), Raise::New);
        self.focus_by_user = user;
    }

    /// Dialog `i` maps without the keyboard: it waits, marked, until the user focuses it.
    fn refuse(&mut self, i: usize) {
        let (app, root) = (window_app(&self.windows[i].window), self.root(i));
        info!(app, ?root, "dialog: focus refused");
        self.windows[i].waiting = true;
        self.sync_attention();
    }

    /// After a commit of `root`, or an X11 hints change: refits a tiled window whose min/max size
    /// changed, and logs a geometry larger than its clip rect once per change of either.
    pub(crate) fn refit(&mut self, root: &WlSurface) {
        let Some(i) = self
            .windows
            .iter()
            .position(|m| window_surface(&m.window).as_ref() == Some(root))
        else {
            return;
        };
        let m = &self.windows[i];
        let (Some(id), app) = (m.tile, window_app(&m.window)) else {
            return;
        };
        let (min, max) = match m.window.x11_surface() {
            Some(x) => crate::xwayland::x11_hints(x),
            None => size_hints(root),
        };
        let changed = self
            .layout
            .tile(id)
            .is_some_and(|t| t.hints != layout::Hints::new(min, max));
        if changed {
            let now = std::time::Instant::now();
            match refit_wait(m.refitted, now) {
                // Coalesced: the timer applies whatever hints are current then.
                Some(_) if m.refit_queued => {}
                Some(wait) => {
                    let surface = root.clone();
                    let timer = Timer::from_duration(wait);
                    let timed = self.loop_handle.insert_source(timer, move |_, _, data| {
                        let s = &mut data.state;
                        let m = s.windows.iter_mut();
                        let mut m =
                            m.filter(|m| window_surface(&m.window).as_ref() == Some(&surface));
                        if let Some(m) = m.next() {
                            m.refit_queued = false;
                        }
                        s.refit(&surface);
                        TimeoutAction::Drop
                    });
                    match timed {
                        Ok(_) => self.windows[i].refit_queued = true,
                        Err(e) => warn!(app, id, "size hints not timed: {e}"),
                    }
                }
                None => {
                    self.windows[i].refitted = Some(now);
                    let fit = self.layout.set_hints(id, min, max);
                    self.report_fit(id, &app, fit);
                }
            }
        }
        let clip = self.layout.clip(id);
        let Some(m) = self
            .windows
            .iter_mut()
            .find(|m| window_surface(&m.window).as_ref() == Some(root))
        else {
            return;
        };
        let size = m.window.geometry().size;
        let over = clip
            .filter(|c| size.w > c.w || size.h > c.h)
            .map(|c| (size, c));
        if over != m.clipped {
            m.clipped = over;
            if let Some((size, clip)) = over {
                let (w, h, clip_w, clip_h) = (size.w, size.h, clip.w, clip.h);
                info!(app, id, w, h, clip_w, clip_h, "clipped");
            }
        }
    }

    /// Logs what a refit of `id` did and applies it; a new tile's spot is saved.
    fn report_fit(&mut self, id: WinId, app: &str, fit: Fit) {
        if fit == Fit::Unchanged {
            return;
        }
        let h = self.layout.tile(id).map(|t| t.hints).unwrap_or_default();
        let (min, max) = (h.min, h.max);
        info!(app, id, ?min, ?max, ?fit, "client size hints");
        match &fit {
            Fit::Replaced(output, cells) => {
                info!(app, id, ?min, ?max, output, ?cells, "re-placed");
            }
            Fit::Tiled(output, cells) => {
                info!(app, id, ?min, ?max, output, ?cells, "tiled again");
            }
            Fit::Floated => self.log_float(id, app),
            Fit::Unchanged | Fit::Fits => {}
        }
        if matches!(fit, Fit::Replaced(..) | Fit::Tiled(..)) {
            self.memory.sync(&self.layout);
        }
        self.apply_layout();
    }

    fn tile_floats(&mut self) {
        let tiled = self.layout.tile_floats();
        for (id, output, cells) in &tiled {
            let app = self.layout.tile(*id).map(|t| t.app.as_str());
            info!(app, id, output, ?cells, by = "room freed", "tiled again");
        }
        if !tiled.is_empty() {
            self.memory.sync(&self.layout);
        }
    }

    pub(crate) fn log_float(&self, id: WinId, app: &str) {
        let h = self.layout.tile(id).map(|t| t.hints).unwrap_or_default();
        let (min, max, rect) = (h.min, h.max, self.layout.rect(id));
        info!(app, id, ?min, ?max, ?rect, "floating: no tile fits");
        if self.layout.too_small(id) {
            info!(
                app,
                id,
                ?min,
                ?rect,
                "floating: its minimum is bigger than the monitor"
            );
        }
    }

    /// Places every dialog (and X11 transient) inside its bounds over its parent, sends it its
    /// bounds and any cap, and hides it while its root is hidden. X11 menus aren't dialogs.
    pub(crate) fn recenter_dialogs(&mut self) {
        self.place_dialogs();
        // A menu of a dialog hidden here hides with it.
        self.place_menus();
        // A dialog hidden here (its parent hidden, a re-parent to a hidden window) can't keep
        // the keyboard: no key may reach a window that isn't shown.
        self.keep_focus_mapped(&[]);
    }

    /// `recenter_dialogs` without the focus check, for `apply_layout`, which does its own.
    fn place_dialogs(&mut self) {
        self.resolve_x11_parents();
        let (mut fresh, mut shown) = (Vec::new(), false);
        for i in 0..self.windows.len() {
            let m = &self.windows[i];
            let menu = m
                .window
                .x11_surface()
                .is_some_and(|x| x.is_override_redirect());
            if m.tile.is_some() || menu {
                continue;
            }
            let spot = self.dialog_spot(i);
            let window = m.window.clone();
            let toplevel = window.toplevel().cloned();
            if let Some(t) = &toplevel {
                if let Some((_, b, _)) = &spot {
                    t.with_pending_state(|s| s.bounds = Some((b.w, b.h).into()));
                }
                if !t.is_initial_configure_sent() {
                    continue;
                }
            }
            let Some((r, b, output)) = spot else {
                self.windows[i].placed = None;
                // New under a hidden root: it can't take the keyboard, so it waits, marked.
                if std::mem::take(&mut self.windows[i].fresh) {
                    self.refuse(i);
                }
                if self.space.element_location(&window).is_some() {
                    self.space.unmap_elem(&window);
                    window.set_activated(false);
                    if let Some(t) = &toplevel {
                        t.send_pending_configure();
                    }
                    info!(app = window_app(&window), "dialog hidden with its parent");
                }
                continue;
            };
            let want = want(&self.windows[i]);
            // Nothing to show before its first buffer.
            if want.0 <= 0 || want.1 <= 0 {
                continue;
            }
            let cap = dialog::needs_cap(want, b);
            let m = &mut self.windows[i];
            (m.placed, m.last_output) = (Some(r), Some(output));
            if let Some((cap_w, cap_h)) = cap
                && m.capped_for != Some(b)
            {
                m.capped_for = Some(b);
                let (app, (w, h)) = (window_app(&window), want);
                info!(app, w, h, cap_w, cap_h, rect = ?r, "dialog: capped to its monitor");
            }
            match (&toplevel, window.x11_surface()) {
                (Some(t), _) => {
                    if let Some(cap) = cap {
                        t.with_pending_state(|s| s.size = Some(cap.into()));
                    }
                    self.configure_dialog(i);
                }
                (None, Some(x)) => {
                    // Every time, as before: it answers the transient's configure requests.
                    let geo = Rectangle::new((r.x, r.y).into(), (r.w, r.h).into());
                    if let Err(e) = x.configure(geo) {
                        warn!("X11 configure: {e}");
                    }
                }
                (None, None) => {}
            }
            shown |= put(&mut self.space, &window, (r.x, r.y));
            if std::mem::take(&mut self.windows[i].fresh) {
                fresh.push(window);
            }
        }
        if shown {
            self.restack();
        }
        for w in fresh {
            if let Some(i) = self.windows.iter().position(|m| m.window == w) {
                self.focus_new(i);
            }
        }
    }

    /// Sorts the shown windows into the one stacking order (`dialog::ranks`): families by their
    /// newest raise, so no map or re-show puts a family above a window raised later.
    pub(crate) fn restack(&mut self) {
        let mut wins: Vec<dialog::Stacked> =
            (0..self.windows.len()).map(|i| self.stacked(i)).collect();
        // A new orphan with nothing inherited goes just below the window on top.
        let top = wins
            .iter()
            .filter(|w| matches!(w.kin, dialog::Kin::Tiled(_)))
            .map(|w| w.rank)
            .max()
            .unwrap_or(0);
        for (m, w) in self.windows.iter_mut().zip(wins.iter_mut()) {
            if matches!(w.kin, dialog::Kin::Orphan(_)) && m.floor == 0 {
                m.floor = dialog::fresh_floor(top);
                w.floor = m.floor;
            }
        }
        let ranks = dialog::ranks(&wins);
        // Floats are drawn and hit in `Layout::floats`' order: lead it with the same ranks.
        for (m, r) in self.windows.iter().zip(&ranks) {
            if let Some(id) = m.tile {
                self.layout.set_rank(id, r.group);
            }
        }
        let shown: Vec<Window> = self.space.elements().cloned().collect();
        let shown_ranks: Vec<dialog::Rank> = shown
            .iter()
            .map(|w| {
                let j = self.windows.iter().position(|m| &m.window == w);
                j.map_or(
                    dialog::Rank {
                        group: 0,
                        layer: 0,
                        own: 0,
                        seq: usize::MAX,
                    },
                    |j| ranks[j],
                )
            })
            .collect();
        restack_space(&mut self.space, &shown, &dialog::stack_order(&shown_ranks));
    }

    /// Window `i` as the stacking order sees it.
    fn stacked(&self, i: usize) -> dialog::Stacked {
        let m = &self.windows[i];
        let index = |w: &Window| self.windows.iter().position(|m| &m.window == w);
        let menu_parent = m
            .window
            .x11_surface()
            .filter(|x| x.is_override_redirect())
            .map(|_| {
                let parent = m.x11_parent.and_then(|id| {
                    self.windows
                        .iter()
                        .position(|p| p.window.x11_surface().is_some_and(|x| x.window_id() == id))
                });
                parent.unwrap_or(i)
            });
        let kin = match (menu_parent, self.root(i)) {
            (Some(p), _) => dialog::Kin::Menu(p),
            (None, Root::Tiled(id)) => dialog::Kin::Tiled(id),
            (None, Root::Orphan) => {
                dialog::Kin::Orphan(dialog::head(i, |j| index(self.windows[j].up.as_ref()?)))
            }
        };
        dialog::Stacked {
            kin,
            dialog: m.tile.is_none(),
            rank: m.rank,
            user_rank: m.user_rank,
            floor: m.floor,
            born: m.born,
        }
    }

    /// Marks each tile whose family has a waiting dialog, for its tab and dock entry.
    fn sync_attention(&mut self) {
        let waiting: Vec<WinId> = (0..self.windows.len())
            .filter(|&i| self.windows[i].waiting)
            .filter_map(|i| match self.root(i) {
                Root::Tiled(id) => Some(id),
                Root::Orphan => None,
            })
            .collect();
        if self.layout.set_attention(&waiting) {
            self.request_redraw();
            self.refresh_toplevels();
        }
    }

    /// Points each X11 transient's `up` at its current `WM_TRANSIENT_FOR` window, once navi
    /// knows it; the property may change, or name a window mapped later.
    fn resolve_x11_parents(&mut self) {
        let ups: Vec<Option<Window>> = self
            .windows
            .iter()
            .map(|m| {
                let x = m.window.x11_surface()?;
                let id = x.is_transient_for().filter(|_| !x.is_override_redirect())?;
                let p = self
                    .windows
                    .iter()
                    .find(|p| p.window.x11_surface().is_some_and(|p| p.window_id() == id))?;
                Some(p.window.clone())
            })
            .collect();
        for (m, up) in self.windows.iter_mut().zip(ups) {
            if m.window.x11_surface().is_some() && m.tile.is_none() {
                m.up = up;
            }
        }
    }

    /// Where dialog `i` goes now: its rect, its bounds and their output; `None` while its root is
    /// hidden. Each dialog of a chain is placed over its parent's rect, an orphan in its bounds.
    fn dialog_spot(&self, i: usize) -> Option<(Rect, Rect, String)> {
        let up = |j: usize| {
            let up = self.windows[j].up.as_ref()?;
            self.windows.iter().position(|m| &m.window == up)
        };
        match self.root(i) {
            Root::Tiled(id) => {
                self.space.element_location(&self.window_of(id)?)?;
                let t = self.layout.tile(id)?;
                let b = dialog::bounds(self.layout.output(&t.output)?.area);
                // The dialogs from `i` up to the root, which `root` found within the cap.
                let mut chain = vec![i];
                for _ in 0..dialog::DIALOG_DEPTH {
                    match up(*chain.last()?) {
                        Some(j) if self.windows[j].tile.is_none() => chain.push(j),
                        _ => break,
                    }
                }
                let mut r = self.layout.rect(id)?;
                for &j in chain.iter().rev() {
                    r = dialog::place(r, want(&self.windows[j]), b);
                }
                Some((r, b, t.output.clone()))
            }
            Root::Orphan => {
                let main = self.main_output().map(|o| identity(&o));
                let mut last = self.windows[i].last_output.iter().chain(main.iter());
                let o = last.find_map(|n| self.layout.output(n))?;
                let b = dialog::bounds(o.area);
                let r = dialog::place(b, want(&self.windows[i]), b);
                Some((r, b, o.name.clone()))
            }
        }
    }

    /// Sends dialog `i` its pending bounds and cap, at most once a second; a later change waits
    /// for a timer.
    fn configure_dialog(&mut self, i: usize) {
        let m = &self.windows[i];
        let (Some(t), app) = (m.window.toplevel().cloned(), window_app(&m.window)) else {
            return;
        };
        if !t.has_pending_changes() {
            return;
        }
        let now = std::time::Instant::now();
        match refit_wait(m.refitted, now) {
            Some(_) if m.refit_queued => {}
            Some(wait) => {
                let surface = t.wl_surface().clone();
                let timer = Timer::from_duration(wait);
                let timed = self.loop_handle.insert_source(timer, move |_, _, data| {
                    let s = &mut data.state;
                    if let Some(m) = s.managed_mut(&surface) {
                        m.refit_queued = false;
                    }
                    s.recenter_dialogs();
                    TimeoutAction::Drop
                });
                match timed {
                    Ok(_) => self.windows[i].refit_queued = true,
                    Err(e) => warn!(app, "dialog configure not timed: {e}"),
                }
            }
            None => {
                self.windows[i].refitted = Some(now);
                t.send_pending_configure();
            }
        }
    }

    /// Removes window `i`. Its dialogs now hang from its own parent, as xdg-shell says (with
    /// none they're orphans); each dialog whose root changed logs its new one.
    pub(crate) fn forget(&mut self, i: usize) -> Managed {
        let mut roots: Vec<Root> = (0..self.windows.len()).map(|j| self.root(j)).collect();
        // Its dialogs keep its family's place only as far as the user raised it.
        let family = dialog::inherited_floor(
            (0..self.windows.len())
                .filter(|&j| j == i || roots[i] != Root::Orphan && roots[j] == roots[i])
                .map(|j| &self.windows[j])
                .map(|m| (m.user_rank, m.floor)),
        );
        let had_keys = self.focused_window().as_ref() == Some(&self.windows[i].window);
        // What the hand-back needs, before the close moves anything. Another window's close
        // keeps `before_focus`, unless it is that window.
        let (user, moves) = (self.focus_by_user, self.focus_moves);
        let before = if had_keys
            || (self.before_focus.as_ref()).is_some_and(|b| b.0 == self.windows[i].window)
        {
            self.before_focus.take()
        } else {
            None
        };
        let outputs = self.space.outputs_for_element(&self.windows[i].window);
        roots.remove(i);
        let gone = self.windows.remove(i);
        self.space.unmap_elem(&gone.window);
        let moved: Vec<bool> = self
            .windows
            .iter_mut()
            .map(|m| {
                let moved = m.up.as_ref() == Some(&gone.window);
                if moved {
                    m.up = gone.up.clone();
                    m.floor = m.floor.max(family);
                }
                moved
            })
            .collect();
        for (j, (was, moved)) in roots.into_iter().zip(moved).enumerate() {
            let root = self.root(j);
            if self.windows[j].tile.is_none() && (moved || root != was) {
                let app = window_app(&self.windows[j].window);
                info!(app, ?was, ?root, "dialog: re-resolved");
            }
        }
        self.recenter_dialogs();
        self.sync_attention();
        // A closed window that had the keyboard gives it back (`may_hand_back`): a dialog to its
        // parent, else to the window that had it just before.
        let parent = gone.up.clone().filter(|_| gone.tile.is_none());
        let back = match (parent, before) {
            (Some(up), before) => {
                let held_before = before.as_ref().is_some_and(|b| b.0 == up);
                let moved_after = before.is_none_or(|b| b.2 != moves);
                let how = HandBack {
                    user_gave: user,
                    held_before,
                    moved_after,
                    ..HandBack::default()
                };
                Some((up, how))
            }
            (None, Some((w, user_gave, at))) => {
                let how = HandBack {
                    user_gave,
                    moved_since: at != moves,
                    ..HandBack::default()
                };
                Some((w, how))
            }
            (None, None) => None,
        };
        let back = back.filter(|(w, how)| {
            may_hand_back(HandBack {
                exposed: self.exposed(w),
                // (Both on none only in the headless test.)
                same_output: {
                    let on = self.space.outputs_for_element(w);
                    on.iter().any(|o| outputs.contains(o)) || on.is_empty() && outputs.is_empty()
                },
                locked: self.lock.active,
                ..*how
            })
        });
        if had_keys && let Some((w, _)) = back {
            self.focus(Some(w));
        }
        gone
    }

    /// Whether a click could reach some of `w`: shown, and not wholly under other windows.
    fn exposed(&self, w: &Window) -> bool {
        let Some(g) = self.space.element_geometry(w) else {
            return false;
        };
        let steps = 12;
        (0..=steps).any(|i| {
            (0..=steps).any(|j| {
                let x = g.loc.x as f64
                    + f64::from(g.size.w.max(1) - 1) * f64::from(i) / f64::from(steps);
                let y = g.loc.y as f64
                    + f64::from(g.size.h.max(1) - 1) * f64::from(j) / f64::from(steps);
                self.window_under((x, y).into())
                    .is_some_and(|(u, _)| u == w)
            })
        })
    }

    /// Whether `popup`'s client already has `layout::CLIENT_POPUPS` other live popups, wherever
    /// they sit: under a live or destroyed root, parentless, or dismissed but kept alive. Closing
    /// one frees its place. Each xdg_popup counts, even several sharing one surface.
    fn client_popups_full(&self, popup: &PopupSurface) -> bool {
        let client = popup.wl_surface().client();
        let others = self.xdg_shell_state.popup_surfaces().iter().filter(|p| {
            p.alive() && p.xdg_popup() != popup.xdg_popup() && p.wl_surface().client() == client
        });
        layout::popups_full(others)
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
        // The popup's own rect is the client's to pick: slide, flip or resize it into its window.
        let Some(b) = self.popup_bounds(window) else {
            return;
        };
        // Relative to the popup's parent; its offsets are the client's, so saturating.
        let up = get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        let sub = |v: i32, a: i32, b: i32| v.saturating_sub(a).saturating_sub(b);
        let (x, y) = (
            sub(b.x, up.x, window_geo.loc.x),
            sub(b.y, up.y, window_geo.loc.y),
        );
        let target = bounded_rect(Rectangle::new((x, y).into(), (b.w, b.h).into()));
        popup.with_pending_state(|s| {
            s.positioner = bounded(s.positioner);
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

/// Maps `w` at `loc`; `true` if it wasn't shown. Its place in the stacking order is
/// `Navi::restack`'s.
pub(crate) fn put<E: smithay::desktop::space::SpaceElement + Clone + PartialEq>(
    space: &mut Space<E>,
    w: &E,
    loc: (i32, i32),
) -> bool {
    let loc = Point::from(loc);
    let at = space.elements().position(|e| e == w);
    match space.element_location(w) {
        Some(was) if was == loc => {}
        // Moved in place, so a commit never restacks.
        Some(_) => {
            let above: Vec<E> = space
                .elements()
                .skip(at.unwrap_or(0) + 1)
                .cloned()
                .collect();
            space.map_element(w.clone(), loc, false);
            above.iter().for_each(|e| space.raise_element(e, false));
        }
        None => space.map_element(w.clone(), loc, false),
    }
    at.is_none()
}

/// Puts `shown` (the space's elements, back to front) in `order`, if it differs.
fn restack_space<E: smithay::desktop::space::SpaceElement + Clone + PartialEq>(
    space: &mut Space<E>,
    shown: &[E],
    order: &[usize],
) {
    if order.iter().enumerate().any(|(k, &i)| k != i) {
        order
            .iter()
            .for_each(|&i| space.raise_element(&shown[i], false));
    }
}

/// On an activation token: who asked for it, and the keyboard as it was then.
struct UserToken {
    /// navi-bar asked (the dock, the launcher).
    bar: bool,
    /// The asker held a keyboard focus the user gave it.
    user: bool,
    /// `Navi::focus_moves` when it was made.
    moves: u64,
}

/// Whether a focus raises the window to the front of the stacking order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Raise {
    /// navi's own refocus (a hand-back, a hidden window's keyboard moving on): no raise.
    Not,
    /// A new window taking the keyboard (job 014): in front while it lives, but its dialogs
    /// inherit nothing from it.
    New,
    /// A click, the dock, Super+N, an activation.
    User,
}

/// The size `m` asks for: a Wayland window's geometry, an X11 window's request. Untrusted.
fn want(m: &Managed) -> (i32, i32) {
    if let Some(asked) = m.asked {
        return asked;
    }
    let g = match m.window.x11_surface() {
        Some(x) => x.geometry().size,
        None => m.window.geometry().size,
    };
    (g.w, g.h)
}

/// A toplevel's committed min and max size, untrusted.
fn size_hints(s: &WlSurface) -> ((i32, i32), (i32, i32)) {
    with_states(s, |states| {
        let mut cached = states.cached_state.get::<SurfaceCachedState>();
        let c = cached.current();
        ((c.min_size.w, c.min_size.h), (c.max_size.w, c.max_size.h))
    })
}

/// The least time between two refits of one window: a client that changes its size hints on
/// every commit gets at most one re-place, and one memory write, a second.
const REFIT_PACE: std::time::Duration = std::time::Duration::from_secs(1);

/// How long until a window last refitted at `last` may refit again; `None` if it may now.
fn refit_wait(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<std::time::Duration> {
    let due = last? + REFIT_PACE;
    (due > now).then(|| due - now)
}

/// `r` cut to `bounds`; `None` if nothing is left. `r` is untrusted, so the sums are in i64.
fn intersect(r: Rect, bounds: Rect) -> Option<Rect> {
    let end = |a: i32, len: i32| i64::from(a) + i64::from(len);
    let (x, y) = (r.x.max(bounds.x), r.y.max(bounds.y));
    let w = end(r.x, r.w).min(end(bounds.x, bounds.w)) - i64::from(x);
    let h = end(r.y, r.h).min(end(bounds.y, bounds.h)) - i64::from(y);
    // Inside `bounds`, so these fit.
    let (w, h) = (i32::try_from(w).ok()?, i32::try_from(h).ok()?);
    (w > 0 && h > 0).then_some(Rect { x, y, w, h })
}

/// `p` with every client value bounded to `layout::POPUP_LIMIT`, so Smithay's placement can't
/// overflow.
fn bounded(mut p: PositionerState) -> PositionerState {
    p.anchor_rect = bounded_rect(p.anchor_rect);
    p.rect_size = bounded_rect(Rectangle::new((0, 0).into(), p.rect_size)).size;
    p.offset = (
        layout::popup_coord(p.offset.x),
        layout::popup_coord(p.offset.y),
    )
        .into();
    p
}

/// Whether a new popup is placed. A parentless one only from navi-bar, as only a layer surface
/// (navi-bar's alone) can adopt it later; elsewhere it would sit uncounted in Smithay's unmapped
/// list, which every loop scans. Never past the depth or count caps.
fn admits_popup(has_parent: bool, navi_bar: bool, too_deep: bool, full: bool) -> bool {
    (has_parent || navi_bar) && !too_deep && !full
}

/// Past `layout::POPUP_DEPTH`; such popups are dismissed and never tracked or placed.
fn too_deep(popup: &PopupSurface) -> bool {
    layout::popup_too_deep(popup.wl_surface().clone(), |s| {
        with_states(s, |st| {
            st.data_map
                .get::<XdgPopupSurfaceData>()?
                .lock()
                .ok()?
                .parent
                .clone()
        })
        .filter(|p| with_states(p, |st| st.data_map.get::<XdgPopupSurfaceData>().is_some()))
    })
}

fn bounded_rect(r: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    let b = layout::popup_rect(Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h));
    Rectangle::new((b.x, b.y).into(), (b.w, b.h).into())
}

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
            self.refit(&root);
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
                let focus = self
                    .seat
                    .get_keyboard()
                    .and_then(|kb| kb.current_focus())
                    .and_then(|f| f.surface());
                let focused = focus.as_ref() == Some(surface);
                let exclusive =
                    l.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive;
                // Opened by a click on the taskbar, which already took the keyboard: the window
                // before that click.
                let window = self
                    .focused_window()
                    .or_else(|| self.before_layer.window().filter(|_| focused));
                let space = &self.space;
                let handoff = self.exclusive.commit(
                    surface.clone(),
                    exclusive,
                    self.lock.active,
                    focused,
                    window,
                    |w| space.element_location(w).is_some(),
                );
                match handoff {
                    Some(Handoff::Take) => self.click_layer(&l),
                    Some(Handoff::Return(back)) => {
                        let to = back.as_ref().and_then(window_surface);
                        self.focus(back);
                        self.handback = to.map(|to| (surface.clone(), to));
                    }
                    None if focused && !l.can_receive_keyboard_focus() => self.focus(None),
                    None => {}
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
            clipped: None,
            refitted: None,
            refit_queued: false,
            want_fullscreen: false,
            escaped: false,
            x11_parent: None,
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
        let m = self.forget(idx);
        if let Some(id) = m.tile {
            self.layout.remove(id);
            info!(id, "closed; its cells stay empty");
            // A tab, or the tiles a maximized one covered, may show now.
            self.apply_layout();
        }
    }

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        let parent = surface.parent();
        let up = self
            .windows
            .iter()
            .find(|m| window_surface(&m.window) == parent);
        let up = up.filter(|_| parent.is_some()).map(|m| m.window.clone());
        if let Some(m) = self.managed_mut(surface.wl_surface()) {
            m.up = up;
        }
        self.recenter_dialogs();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        let navi_bar = privileged(
            surface
                .wl_surface()
                .client()
                .as_ref()
                .and_then(|c| c.get_data()),
        );
        let has_parent = surface.get_parent_surface().is_some();
        if !admits_popup(
            has_parent,
            navi_bar,
            too_deep(&surface),
            self.client_popups_full(&surface),
        ) {
            surface.send_popup_done();
            return;
        }
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        if too_deep(&surface) {
            return;
        }
        let positioner = bounded(positioner);
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
            id if self.refuse_escaped(id) => {
                surface.send_configure();
            }
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
        self.set_decoration(toplevel, None);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Decoration) {
        self.set_decoration(toplevel, Some(mode));
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration(toplevel, None);
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
        self.focus_moves += 1;
        let surface = focused.and_then(Focus::surface);
        self.handback = activation::after_focus_change(self.handback.take(), surface.as_ref());
        let to_layer = surface
            .as_ref()
            .is_some_and(|s| self.layer_output(s).is_some());
        self.before_layer.moved(to_layer);
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
                self.exclusive.forget(layer.wl_surface());
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
        let asker = data
            .client_id
            .clone()
            .and_then(|c| self.display_handle.backend_handle().get_client_data(c).ok());
        let navi_bar = privileged(asker.as_ref().and_then(|d| d.downcast_ref::<ClientState>()));
        let user = activation::minted_by_user(navi_bar, self.focus_by_user);
        let token = UserToken {
            bar: navi_bar,
            user,
            moves: self.focus_moves,
        };
        data.user_data.insert_if_missing(|| token);
        info!(
            app = data.app_id,
            ?serial,
            ?since,
            issued,
            user,
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
        let focus_kept = activation::focus_kept(
            focus.as_ref(),
            data.surface.as_ref(),
            &surface,
            self.handback.as_ref(),
        );
        let age = data.timestamp.elapsed();
        match id.filter(|_| may_redeem(first_use, age, focus_kept)) {
            Some(id) => {
                // Only a token from a focus the user gave, still held, raises and takes the
                // keyboard; any other only marks the window for the user.
                let user = data.user_data.get::<UserToken>().is_some_and(|t| {
                    activation::user_raise(t.bar, t.user, t.moves, self.focus_moves)
                });
                match activation::on_redeem(user) {
                    activation::Redeem::Focus => {
                        info!(id, by = "activation", user, "activate");
                        self.focus_or_restore(id);
                    }
                    activation::Redeem::Mark => {
                        info!(
                            id,
                            by = "activation",
                            user,
                            "activation: marked for the user"
                        );
                        if let Some(m) = self.managed_mut(&surface) {
                            m.waiting = true;
                        }
                        self.sync_attention();
                    }
                }
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
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_positioner::{
        Anchor, ConstraintAdjustment, Gravity,
    };

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn positioner(
        offset: (i32, i32),
        anchor: Rectangle<i32, Logical>,
        size: i32,
    ) -> PositionerState {
        PositionerState {
            rect_size: (size, size).into(),
            anchor_rect: anchor,
            offset: offset.into(),
            ..Default::default()
        }
    }

    /// A real client on an in-process navi: 66 `get_popup`s on one xdg_surface are 66 popups,
    /// so the 65th and 66th are dismissed.
    #[test]
    fn popups_sharing_a_surface_each_count() {
        use smithay_client_toolkit::reexports::client::{
            Connection, Dispatch, QueueHandle, delegate_noop,
            protocol::{wl_callback, wl_compositor, wl_registry, wl_surface},
        };
        use smithay_client_toolkit::reexports::protocols::xdg::shell::client::{
            xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base,
        };

        #[derive(Default)]
        struct Client {
            compositor: Option<wl_compositor::WlCompositor>,
            wm: Option<xdg_wm_base::XdgWmBase>,
            done: usize,
            synced: bool,
        }
        impl Dispatch<wl_registry::WlRegistry, ()> for Client {
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
                        _ => {}
                    }
                }
            }
        }
        impl Dispatch<xdg_popup::XdgPopup, ()> for Client {
            fn event(
                c: &mut Self,
                _: &xdg_popup::XdgPopup,
                e: xdg_popup::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let xdg_popup::Event::PopupDone = e {
                    c.done += 1;
                }
            }
        }
        impl Dispatch<wl_callback::WlCallback, ()> for Client {
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
        delegate_noop!(Client: ignore wl_compositor::WlCompositor);
        delegate_noop!(Client: ignore wl_surface::WlSurface);
        delegate_noop!(Client: ignore xdg_wm_base::XdgWmBase);
        delegate_noop!(Client: ignore xdg_surface::XdgSurface);
        delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
        delegate_noop!(Client: ignore xdg_positioner::XdgPositioner);

        let (mut event_loop, mut data) = crate::xwayland::tests::harness();
        let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        data.state
            .display_handle
            .insert_client(server, std::sync::Arc::new(ClientState::default()))
            .unwrap();
        let conn = Connection::from_socket(client).unwrap();
        let mut queue = conn.new_event_queue::<Client>();
        let qh = queue.handle();
        let mut c = Client::default();
        // Until the server has answered everything sent so far.
        let mut sync = |c: &mut Client,
                        queue: &mut smithay_client_toolkit::reexports::client::EventQueue<
            Client,
        >| {
            c.synced = false;
            conn.display().sync(&qh, ());
            queue.flush().unwrap();
            crate::xwayland::tests::pump(&mut event_loop, &mut data, |_| {
                if let Some(read) = queue.prepare_read() {
                    let _ = read.read();
                }
                queue.dispatch_pending(c).unwrap();
                c.synced
            });
        };
        conn.display().get_registry(&qh, ());
        sync(&mut c, &mut queue);
        let (comp, wm) = (c.compositor.clone().unwrap(), c.wm.clone().unwrap());
        let top = wm.get_xdg_surface(&comp.create_surface(&qh, ()), &qh, ());
        top.get_toplevel(&qh, ());
        let pos = wm.create_positioner(&qh, ());
        pos.set_size(8, 8);
        pos.set_anchor_rect(0, 0, 1, 1);
        let shared = wm.get_xdg_surface(&comp.create_surface(&qh, ()), &qh, ());
        let popups: Vec<_> = (0..layout::CLIENT_POPUPS + 2)
            .map(|_| shared.get_popup(Some(&top), &pos, &qh, ()))
            .collect();
        sync(&mut c, &mut queue);
        assert_eq!(popups.len() - c.done, layout::CLIENT_POPUPS);
    }

    #[test]
    fn only_navi_bar_may_open_a_parentless_popup() {
        assert!(admits_popup(true, false, false, false));
        assert!(admits_popup(true, true, false, false));
        assert!(admits_popup(false, true, false, false));
        assert!(!admits_popup(false, false, false, false));
        for (deep, full) in [(true, false), (false, true)] {
            assert!(!admits_popup(true, false, deep, full));
            assert!(!admits_popup(false, true, deep, full));
        }
    }

    #[test]
    fn extreme_positioners_place_without_overflow() {
        let (max, min) = (i32::MAX, i32::MIN);
        let targets = [
            rect(min, min, max, max),
            rect(max, max, max, max),
            rect(0, 0, 1, 1),
        ]
        .map(bounded_rect);
        let mut raw = Vec::new();
        for offset in [(max, max), (min, min), (max, min), (0, 0)] {
            for (loc, len) in [(max, max), (min, max), (max, 1), (min, 1), (0, 1)] {
                for size in [max, 1] {
                    raw.push(positioner(offset, rect(loc, loc, len, len), size));
                }
            }
        }
        for p in raw {
            let mut p = bounded(p);
            for anchor in 0..=8 {
                p.anchor_edges = Anchor::try_from(anchor).unwrap();
                for gravity in 0..=8 {
                    p.gravity = Gravity::try_from(gravity).unwrap();
                    let _ = p.get_geometry();
                    for adjust in 0..64 {
                        p.constraint_adjustment = ConstraintAdjustment::from_bits_truncate(adjust);
                        for t in targets {
                            let _ = p.get_unconstrained_geometry(t);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn in_range_popups_place_as_before() {
        let menu = PositionerState {
            anchor_edges: Anchor::BottomLeft,
            gravity: Gravity::BottomRight,
            constraint_adjustment: ConstraintAdjustment::all(),
            ..positioner((0, 4), rect(100, 20, 80, 24), 300)
        };
        for t in [
            rect(-500, -300, 1280, 800),
            rect(0, 0, 400, 300),
            rect(-90, -10, 200, 200),
        ] {
            assert_eq!(bounded(menu), menu);
            assert_eq!(
                bounded(menu).get_unconstrained_geometry(bounded_rect(t)),
                menu.get_unconstrained_geometry(t)
            );
        }
    }

    #[test]
    fn reposition_to_extreme_offset_places_without_overflow() {
        let p = PositionerState {
            anchor_edges: Anchor::BottomRight,
            gravity: Gravity::BottomRight,
            ..positioner(
                (i32::MAX, i32::MAX),
                rect(i32::MAX, i32::MAX, 1, 1),
                i32::MAX,
            )
        };
        // As in `reposition_request`: anchor point L + 1, plus offset L.
        let l = layout::POPUP_LIMIT;
        assert_eq!(bounded(p).get_geometry(), rect(2 * l + 1, 2 * l + 1, l, l));
    }

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

    /// A stand-in window for `Space`, by name.
    #[derive(Clone, Debug, PartialEq)]
    struct Elem(&'static str);

    impl smithay::utils::IsAlive for Elem {
        fn alive(&self) -> bool {
            true
        }
    }

    impl smithay::desktop::space::SpaceElement for Elem {
        fn bbox(&self) -> Rectangle<i32, Logical> {
            Rectangle::new((0, 0).into(), (10, 10).into())
        }
        fn is_in_input_region(&self, _: &Point<f64, Logical>) -> bool {
            true
        }
        fn set_activate(&self, _: bool) {}
        fn output_enter(&self, _: &Output, _: Rectangle<i32, Logical>) {}
        fn output_leave(&self, _: &Output) {}
    }

    /// Round 1 security: re-placing a shown window (a dialog on every commit) never raises it,
    /// so a neighbour the user raised stays in front of it.
    #[test]
    fn re_placing_keeps_the_stacking_order() {
        let mut space = Space::<Elem>::default();
        let order = |s: &Space<Elem>| s.elements().map(|e| e.0).collect::<Vec<_>>();
        for (e, x) in [("dialog", 0), ("neighbour", 50)] {
            put(&mut space, &Elem(e), (x, 0));
        }
        assert_eq!(
            order(&space),
            ["dialog", "neighbour"],
            "new windows go on top"
        );
        put(&mut space, &Elem("dialog"), (0, 0));
        put(&mut space, &Elem("dialog"), (7, 3));
        assert_eq!(order(&space), ["dialog", "neighbour"]);
        assert_eq!(space.element_location(&Elem("dialog")), Some((7, 3).into()));
        put(&mut space, &Elem("new"), (1, 1));
        put(&mut space, &Elem("neighbour"), (9, 9));
        assert_eq!(order(&space), ["dialog", "neighbour", "new"]);
    }

    /// Round 3/4 security: parent and neighbour hidden (neighbour raised last), shown again in
    /// open order (parent last): after the restack a dialog of parent stays under neighbour.
    #[test]
    fn re_shown_windows_keep_the_user_raise_order() {
        use dialog::{Rank, stack_order};
        let mut space = Space::<Elem>::default();
        let rank = |e: &str| match e {
            "parent" => Rank {
                group: 3,
                layer: 0,
                own: 3,
                seq: 0,
            },
            "dialog" => Rank {
                group: 3,
                layer: 1,
                own: 9,
                seq: 2,
            },
            _ => Rank {
                group: 5,
                layer: 0,
                own: 5,
                seq: 1,
            },
        };
        for e in ["neighbour", "parent", "dialog"] {
            assert!(put(&mut space, &Elem(e), (0, 0)), "newly shown");
        }
        let shown: Vec<Elem> = space.elements().cloned().collect();
        let ranks: Vec<Rank> = shown.iter().map(|e| rank(e.0)).collect();
        restack_space(&mut space, &shown, &stack_order(&ranks));
        let order: Vec<&str> = space.elements().map(|e| e.0).collect();
        assert_eq!(order, ["parent", "dialog", "neighbour"]);
    }

    #[test]
    fn refits_are_paced_to_one_a_second() {
        let t = std::time::Instant::now();
        let ms = std::time::Duration::from_millis;
        assert_eq!(refit_wait(None, t), None);
        assert_eq!(refit_wait(Some(t), t), Some(REFIT_PACE));
        assert_eq!(refit_wait(Some(t), t + ms(400)), Some(ms(600)));
        assert_eq!(refit_wait(Some(t), t + REFIT_PACE), None);
        assert_eq!(refit_wait(Some(t), t + ms(5000)), None);
    }

    #[test]
    fn popups_are_cut_to_their_bounds() {
        let full = Rect::new(0, 0, 1280, 800);
        let r = Rect::new;
        assert_eq!(
            intersect(r(100, 100, 200, 50), full),
            Some(r(100, 100, 200, 50))
        );
        assert_eq!(
            intersect(r(-50, 700, 5000, 5000), full),
            Some(r(0, 700, 1280, 100))
        );
        assert_eq!(intersect(r(1280, 0, 10, 10), full), None);
        assert_eq!(intersect(r(10, 10, 0, 10), full), None);
        // Client-sized extremes: no overflow.
        let max = i32::MAX;
        assert_eq!(
            intersect(r(12, 0, max, max), full),
            Some(r(12, 0, 1268, 800))
        );
        assert_eq!(intersect(r(max, max, max, max), full), None);
        assert_eq!(intersect(r(i32::MIN, i32::MIN, max, max), full), None);
        assert_eq!(intersect(r(-5, -5, max, max), full), Some(full));
        assert_eq!(intersect(r(0, 0, 10, 10), r(max - 5, 0, max, max)), None);
    }
}
