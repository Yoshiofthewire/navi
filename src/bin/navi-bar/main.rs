#![forbid(unsafe_code)]
//! navi-bar: the taskbar and app menu. navi starts it over a socket only it holds, which is the
//! one connection that sees layer shell and the window list. It launches the apps the user picks,
//! without a shell, each with an xdg-activation token.

#[path = "../../files.rs"]
mod files;
#[path = "../../image.rs"]
mod image;
#[path = "../../theme.rs"]
mod theme;
#[path = "../../utility_theme.rs"]
mod utility_theme;

mod cards;
#[path = "../../control.rs"]
mod control;
#[allow(dead_code)]
#[path = "../../cursor.rs"]
mod cursor;
mod dbus;
mod desktop;
#[path = "../../desktop_config.rs"]
mod desktop_config;
mod dock;
mod exec;
mod font;
mod icons;
mod layout;
mod menu;
mod notify;
mod runtime;
mod session;
mod status;
#[cfg(test)]
mod testutil;

use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::{net::UnixStream, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use resvg::tiny_skia::{self, Color, Paint, Pixmap, PixmapPaint, PremultipliedColorU8, Transform};
use smithay_client_toolkit::{
    activation::{ActivationHandler, ActivationState, RequestData, RequestDataExt},
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_activation, delegate_compositor, delegate_keyboard, delegate_layer, delegate_output,
    delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            channel::{self, Channel},
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Dispatch, QueueHandle, event_created_child,
            globals::registry_queue_init,
            protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
        },
        protocols_wlr::foreign_toplevel::v1::client::{
            zwlr_foreign_toplevel_handle_v1::{self as handle, ZwlrForeignToplevelHandleV1},
            zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use tracing::{debug, error, info, warn};

use cards::Card;
use dbus::Call;
use desktop::Entry;
use icons::{Icons, Themes};
use layout::{BAR_H, IconOf, Item, Layout, Rect, Slot};
use menu::{Menu, Row};
use notify::{Closed, Notes};
use theme::{DIM, INK};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
fn text_px() -> f32 {
    TEXT_SIZE.load(std::sync::atomic::Ordering::Relaxed) as f32
}
static TEXT_SIZE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(14);
/// How long a launch or a card's action waits for its activation token.
const TOKEN_WAIT: Duration = Duration::from_secs(1);
/// The status band across the top of the main output.
const BAND_H: i32 = session::BAND_H;
const POWER: &str = "/sys/class/power_supply";

fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    if let Err(e) = run() {
        error!("navi-bar: {e}");
        std::process::exit(1);
    }
}

/// navi's socket from `WAYLAND_SOCKET=0`, moved off fd 0 to a close-on-exec fd, with
/// `/dev/null` on fd 0; else `WAYLAND_DISPLAY`.
fn connect() -> Result<Connection, String> {
    match std::env::var("WAYLAND_SOCKET").as_deref() {
        Ok("0") => {
            let sock = std::io::stdin()
                .as_fd()
                .try_clone_to_owned()
                .map_err(|e| format!("WAYLAND_SOCKET: {e}"))?;
            let null = File::open("/dev/null").map_err(|e| format!("/dev/null: {e}"))?;
            rustix::stdio::dup2_stdin(&null).map_err(|e| format!("stdin: {e}"))?;
            Connection::from_socket(UnixStream::from(sock)).map_err(|e| e.to_string())
        }
        Ok(v) => Err(format!("WAYLAND_SOCKET={v}: only 0, from navi, is used")),
        Err(_) => Connection::connect_to_env().map_err(|e| e.to_string()),
    }
}

fn run() -> Result<(), String> {
    let control = if std::env::var("NAVI_CONTROL_FD").as_deref() == Ok("1") {
        let fd = std::io::stdout()
            .as_fd()
            .try_clone_to_owned()
            .map_err(|e| e.to_string())?;
        let null = File::options()
            .write(true)
            .open("/dev/null")
            .map_err(|e| e.to_string())?;
        rustix::stdio::dup2_stdout(&null).map_err(|e| e.to_string())?;
        Some(UnixStream::from(fd))
    } else {
        None
    };
    let conn = connect()?;
    let (globals, queue) = registry_queue_init::<Bar>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let need = |what: &str| {
        format!(
            "the compositor offers no {what}. navi gives it only to the navi-bar it starts \
             itself; run navi, not navi-bar by hand"
        )
    };
    let compositor = CompositorState::bind(&globals, &qh).map_err(|_| need("wl_compositor"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|_| need("zwlr_layer_shell_v1"))?;
    let _manager: ZwlrForeignToplevelManagerV1 = globals
        .bind(&qh, 1..=3, ())
        .map_err(|_| need("zwlr_foreign_toplevel_manager_v1"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|_| need("wl_shm"))?;
    let activation = ActivationState::bind(&globals, &qh).map_err(|_| need("xdg_activation_v1"))?;

    if dbus::wants_export(std::env::var_os("NAVI_EXPORT_ENV").as_deref()) {
        // Off the main thread: a stuck bus mustn't hold up the taskbar.
        std::thread::spawn(dbus::export_env);
    }
    let l = Layout::new(0, false);
    let layer = bar_layer(&compositor, &layer_shell, &qh, &l);
    let band = band_layer(&compositor, &layer_shell, &qh)?;

    let mut event_loop: EventLoop<Bar> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let var = std::env::var_os;
    let data_dirs = desktop::data_dirs(
        var("XDG_DATA_HOME").as_deref(),
        var("HOME").as_deref(),
        var("XDG_DATA_DIRS").as_deref(),
    );
    let config = files::xdg_dir(
        var("XDG_CONFIG_HOME").as_deref(),
        var("HOME").as_deref(),
        ".config",
    );
    let desktop_settings = desktop_config::load().unwrap_or_else(|_| desktop_config::defaults());
    theme::configure(&desktop_settings);
    let preferred = desktop_settings["icon_theme"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| icons::preferred(config.as_deref()));
    let themes = Themes::new(
        icons::bases(&data_dirs, var("HOME").as_deref()),
        Some(PathBuf::from("/usr/share/pixmaps")),
        &preferred,
    );
    info!(themes = ?themes.chain(), "icon themes");
    let font = font::load();
    if font.is_none() {
        warn!("no font found: menu rows show icons only");
    }
    let (tx, rx): (_, Channel<Call>) = channel::channel();
    event_loop
        .handle()
        .insert_source(rx, |event, _, bar| {
            if let channel::Event::Msg(call) = event {
                bar.call(call);
            }
        })
        .map_err(|e| e.to_string())?;
    let (command_tx, command_rx) = channel::channel();
    event_loop
        .handle()
        .insert_source(command_rx, |event, _, bar| match event {
            channel::Event::Msg(control::Command::ToggleMenu) => bar.toggle_menu(),
            channel::Event::Msg(control::Command::Dock(key)) => bar.dock_key(key),
            channel::Event::Msg(control::Command::WindowLimit) => bar.window_limit(),
            channel::Event::Closed => {}
        })
        .map_err(|e| e.to_string())?;
    let control = match control {
        Some(stream) => Some(std::sync::Arc::new(std::sync::Mutex::new(
            control::reader(stream, move |c| {
                let _ = command_tx.send(c);
            })
            .map_err(|e| e.to_string())?,
        ))),
        None => None,
    };
    let (session_tx, session_rx) = channel::channel();
    event_loop
        .handle()
        .insert_source(session_rx, |event, _, bar| {
            if let channel::Event::Msg(reply) = event {
                bar.session_reply(reply);
            }
        })
        .map_err(|e| e.to_string())?;
    let tick = Timer::from_duration(status::next_tick(&jiff::Zoned::now()));
    event_loop
        .handle()
        .insert_source(tick, |_, _, bar| {
            if bar.full_session {
                runtime::play(&bar.desktop_settings, "login");
            }
            bar.refresh_status();
            TimeoutAction::ToDuration(status::next_tick(&jiff::Zoned::now()))
        })
        .map_err(|e| e.to_string())?;
    let runtime = runtime::watch(
        tx.clone(),
        dbus::wants_export(var("NAVI_EXPORT_ENV").as_deref()),
    );
    let mut bar = Bar {
        desktop_settings,
        runtime,
        dbus: dbus::serve(tx.clone()),
        _settings_dbus: dbus::serve_settings(tx.clone()),
        notes: Notes::default(),
        start: Instant::now(),
        band,
        band_configured: false,
        band_w: 0,
        status: String::new(),
        battery: None,
        session: session::Menu {
            full: dbus::wants_export(var("NAVI_EXPORT_ENV").as_deref()),
            ..Default::default()
        },
        session_since: 0,
        control,
        band_hover: false,
        session_tx,
        full_session: dbus::wants_export(var("NAVI_EXPORT_ENV").as_deref()),
        logout: false,
        cards_layer: None,
        cards_size: (0, 0),
        cards_configured: false,
        cards: Vec::new(),
        cards_changed_ms: 0,
        compositor,
        layer_shell,
        qh: qh.clone(),
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(256 * 1024, &shm).map_err(|e| e.to_string())?,
        shm,
        activation,
        layer,
        loop_handle: event_loop.handle(),
        layout: l,
        configured: false,
        scale: 1,
        windows: Vec::new(),
        seat: None,
        keyboard: None,
        pointer: None,
        pointer_at: (0.0, 0.0),
        entries: desktop::scan(&data_dirs),
        data_dirs,
        menu: None,
        pins: dock::Pins::load(),
        hover: None,
        pin_message: String::new(),
        icons: Icons::new(themes),
        font,
        asks: 0,
        pending: Vec::new(),
        closed: false,
    };
    bar.refresh_status();
    bar.relayout();
    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .map_err(|e| e.to_string())?;
    while !bar.closed {
        event_loop
            .dispatch(None, &mut bar)
            .map_err(|e| format!("connection lost: {e}"))?;
    }
    // A successful shell exit is the private logout request to Navi. Failures respawn the bar.
    if bar.logout {
        runtime::play(&bar.desktop_settings, "logout");
        Ok(())
    } else {
        Err("Shell surfaces were closed unexpectedly".into())
    }
}

/// The taskbar's layer surface, on whatever output navi picks.
fn bar_layer(
    compositor: &CompositorState,
    layer_shell: &LayerShell,
    qh: &QueueHandle<Bar>,
    l: &Layout,
) -> LayerSurface {
    let surface = compositor.create_surface(qh);
    let layer = layer_shell.create_layer_surface(qh, surface, Layer::Top, Some("navi-bar"), None);
    layer.set_anchor(Anchor::BOTTOM);
    layer.set_exclusive_zone(BAR_H);
    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    layer.set_size(l.w as u32, l.h as u32);
    layer.commit();
    layer
}

/// The status band on the main output. Only the Navi button and open menu accept clicks.
fn band_layer(
    compositor: &CompositorState,
    layer_shell: &LayerShell,
    qh: &QueueHandle<Bar>,
) -> Result<LayerSurface, String> {
    let surface = compositor.create_surface(qh);
    let band = layer_shell.create_layer_surface(qh, surface, Layer::Top, Some("navi-status"), None);
    band.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
    let [top, right, bottom, left] = session::BAND_MARGIN;
    band.set_margin(top, right, bottom, left);
    band.set_size(0, BAND_H as u32);
    band.set_exclusive_zone(BAND_H);
    band.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    let none = Region::new(compositor).map_err(|e| e.to_string())?;
    band.wl_surface().set_input_region(Some(none.wl_region()));
    band.commit();
    Ok(band)
}

/// A window from the window list.
struct Win {
    handle: ZwlrForeignToplevelHandleV1,
    app_id: String,
    minimized: bool,
    activated: bool,
    /// A dialog of it waits for the user (navi's private state).
    attention: bool,
    /// Shown after its first `done`.
    ready: bool,
}

struct Bar {
    desktop_settings: serde_json::Value,
    runtime: runtime::Runtime,
    qh: QueueHandle<Bar>,
    compositor: CompositorState,
    layer_shell: LayerShell,
    /// The status band and its width, 0 until configured.
    band: LayerSurface,
    band_configured: bool,
    band_w: i32,
    status: String,
    battery: Option<status::Battery>,
    session: session::Menu,
    /// When the session menu opened, in `now_ms`: cards hidden behind it don't expire meanwhile.
    session_since: u64,
    control: Option<std::sync::Arc<std::sync::Mutex<control::Control>>>,
    band_hover: bool,
    session_tx: channel::Sender<session::Reply>,
    full_session: bool,
    logout: bool,
    /// The notification cards' surface, while any are shown.
    cards_layer: Option<LayerSurface>,
    cards_size: (i32, i32),
    cards_configured: bool,
    cards: Vec<Card>,
    /// `now_ms` when `cards` last changed, for `cards::armed`.
    cards_changed_ms: u64,
    notes: Notes,
    /// The bus connection, while serving notifications.
    dbus: Option<zbus::blocking::Connection>,
    /// Independent owner for trusted Settings requests, even with another notification server.
    _settings_dbus: Option<zbus::blocking::Connection>,
    /// The notifications' clock.
    start: Instant,
    registry: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    activation: ActivationState,
    layer: LayerSurface,
    loop_handle: LoopHandle<'static, Bar>,
    /// What the surface was last sized for.
    layout: Layout,
    configured: bool,
    scale: i32,
    windows: Vec<Win>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    pointer_at: (f64, f64),
    entries: Vec<Entry>,
    data_dirs: Vec<PathBuf>,
    menu: Option<Menu>,
    pins: dock::Pins,
    hover: Option<layout::Hit>,
    pin_message: String,
    icons: Icons,
    font: Option<fontdue::Font>,
    asks: u64,
    /// What waits for a token, by number.
    pending: Vec<(u64, Pending)>,
    closed: bool,
}

/// What waits for a token.
enum Pending {
    Launch(Entry),
    /// A card's `default` action: the note's id and sender, and its `NotificationClosed`, sent
    /// after the action.
    Action(u32, String, Option<Closed>),
}

/// A token request for pending number `seq`.
struct TokenRequest {
    seq: u64,
    data: RequestData,
}

impl RequestDataExt for TokenRequest {
    fn app_id(&self) -> Option<&str> {
        self.data.app_id()
    }

    fn seat_and_serial(&self) -> Option<(&wl_seat::WlSeat, u32)> {
        self.data.seat_and_serial()
    }

    fn surface(&self) -> Option<&wl_surface::WlSurface> {
        self.data.surface()
    }
}

impl Bar {
    fn shown(&self) -> impl Iterator<Item = &Win> {
        self.windows.iter().filter(|w| w.ready)
    }

    /// Resizes the surface to what it now holds, or redraws it at its size.
    fn relayout(&mut self) {
        // navi gives the open launcher the keyboard, and hands it back when it closes. Closed,
        // a click still focuses the taskbar, so a dock launch gets an activation token.
        self.layer.set_keyboard_interactivity(match self.menu {
            Some(_) => KeyboardInteractivity::Exclusive,
            None => KeyboardInteractivity::OnDemand,
        });
        let rows = self.menu.as_ref().map_or(menu::ROWS, |m| m.rows().count());
        let l = Layout::new(self.dock_apps().len(), self.menu.is_some()).with_rows(rows);
        if l == self.layout {
            return self.draw();
        }
        self.layout = l;
        self.layer.set_size(l.w as u32, l.h as u32);
        // A client-requested layer size need not produce another configure. Commit the new
        // buffer and input region with that size, so an old image never moves into its place.
        if self.configured {
            self.draw();
        } else {
            self.layer.commit();
        }
    }

    fn toggle_menu(&mut self) {
        self.close_session();
        self.pin_message.clear();
        self.menu = match self.menu {
            Some(_) => None,
            None => {
                self.entries = desktop::scan(&self.data_dirs);
                Some(Menu::new(&self.entries))
            }
        };
        debug!(open = self.menu.is_some(), "launcher toggled");
        self.relayout();
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            self.relayout();
        }
    }

    /// Asks for an activation token for the click or key with `serial`, then launches.
    fn launch(&mut self, entry: usize, serial: u32, qh: &QueueHandle<Self>) {
        let Some(e) = self.entries.get(entry).cloned() else {
            return;
        };
        let (app_id, surface) = (Some(e.id.clone()), self.layer.wl_surface().clone());
        self.ask_token(Pending::Launch(e), app_id, serial, surface, qh);
        self.close_menu();
    }

    /// Asks for an activation token for the press or key with `serial` on `surface`; `p` goes
    /// on with it, or without one after `TOKEN_WAIT`.
    fn ask_token(
        &mut self,
        p: Pending,
        app_id: Option<String>,
        serial: u32,
        surface: wl_surface::WlSurface,
        qh: &QueueHandle<Self>,
    ) {
        self.asks += 1;
        let seq = self.asks;
        let data = RequestData {
            app_id,
            seat_and_serial: self.seat.clone().map(|s| (s, serial)),
            surface: Some(surface),
        };
        self.activation
            .request_token_with_data(qh, TokenRequest { seq, data });
        self.pending.push((seq, p));
        let timer = Timer::from_duration(TOKEN_WAIT);
        let wait = self.loop_handle.insert_source(timer, move |_, _, bar| {
            if let Some(p) = bar.take_pending(seq) {
                warn!("no activation token within 1 s: going on without one");
                bar.go_on(p, None);
            }
            TimeoutAction::Drop
        });
        if let Err(e) = wait {
            warn!("token timer: {e}");
        }
    }

    fn take_pending(&mut self, seq: u64) -> Option<Pending> {
        let at = self.pending.iter().position(|p| p.0 == seq)?;
        Some(self.pending.remove(at).1)
    }

    fn go_on(&mut self, p: Pending, token: Option<&str>) {
        match p {
            Pending::Launch(e) => spawn(&e, token),
            Pending::Action(id, sender, closed) => {
                if let Some(conn) = &self.dbus {
                    dbus::action(conn, &sender, id, "default", token);
                }
                info!(id, token = token.is_some(), "card action");
                self.emit(closed.into_iter().collect());
            }
        }
    }

    fn running(&self) -> Vec<dock::Running<'_>> {
        let windows = self.shown().map(|w| dock::Running {
            app_id: &w.app_id,
            focused: w.activated,
            minimized: w.minimized,
            attention: w.attention,
        });
        windows.collect()
    }

    fn dock_apps(&self) -> Vec<dock::App> {
        dock::apps(&self.pins.ids, &self.entries, &self.running())
    }

    /// Super+1..9 or Super+Tab, sent by navi: activates as a dock click does.
    fn dock_key(&mut self, key: control::DockKey) {
        let windows = self.running();
        let apps = dock::apps(&self.pins.ids, &self.entries, &windows);
        match dock::key(&apps, &windows, key) {
            Ok((app, win)) => {
                info!(?key, app_id = apps[app].app_id, "dock key");
                let h = self.shown().nth(win).map(|w| w.handle.clone());
                if let (Some(h), Some(seat)) = (h, &self.seat) {
                    h.activate(seat);
                }
                self.close_menu();
            }
            Err(why) => debug!(?key, why, "dock key does nothing"),
        }
    }

    fn toggle_pin(&mut self, entry: usize) {
        let Some(e) = self.entries.get(entry) else {
            return;
        };
        match self.pins.toggle(&e.id) {
            Ok(pinned) => {
                self.pin_message = format!(
                    "{} {} the dock",
                    e.name,
                    if pinned { "pinned to" } else { "removed from" }
                );
                info!(id = e.id, pinned, "dock pin changed");
            }
            Err(err) => {
                warn!("{err}");
                self.pin_message = err;
                if self.menu.is_none() {
                    let mut menu = Menu::new(&self.entries);
                    menu.set_query(&e.name, &self.entries);
                    self.menu = Some(menu);
                }
            }
        }
        self.relayout();
    }

    fn open_row(&mut self, row: Row, serial: u32, qh: &QueueHandle<Self>) {
        match row {
            Row::Folder(folder) => {
                if let Some(menu) = &mut self.menu {
                    menu.open(folder, &self.entries);
                }
                self.relayout();
            }
            Row::App(entry) => self.launch(entry, serial, qh),
        }
    }

    fn click(&mut self, serial: u32, qh: &QueueHandle<Self>) {
        let (x, y) = (self.pointer_at.0 as i32, self.pointer_at.1 as i32);
        debug!(x, y, hit = ?self.layout.hit(x, y), "dock click");
        match self.layout.hit(x, y) {
            Some(layout::Hit::Button) => self.toggle_menu(),
            Some(layout::Hit::Window(i)) => {
                let apps = self.dock_apps();
                let Some(app) = apps.get(i) else {
                    return;
                };
                if app.windows.is_empty() {
                    if let Some(e) = app.entry {
                        self.launch(e, serial, qh);
                    }
                } else {
                    let focused = app
                        .windows
                        .iter()
                        .position(|&i| self.shown().nth(i).is_some_and(|w| w.activated));
                    let next = focused.map_or(0, |i| (i + 1) % app.windows.len());
                    let win = self
                        .shown()
                        .nth(app.windows[next])
                        .map(|w| w.handle.clone());
                    if let (Some(h), Some(seat)) = (win, &self.seat) {
                        h.activate(seat);
                    }
                    self.close_menu();
                }
            }
            Some(layout::Hit::Row(k) | layout::Hit::Pin(k)) => {
                if let Some(row) = self.menu.as_ref().and_then(|m| m.at_row(k)) {
                    match (self.layout.hit(x, y), row) {
                        (Some(layout::Hit::Pin(_)), Row::App(e)) => self.toggle_pin(e),
                        _ => self.open_row(row, serial, qh),
                    }
                }
            }
            Some(layout::Hit::Back) => {
                if let Some(menu) = &mut self.menu {
                    menu.back(&self.entries);
                }
                self.relayout();
            }
            None => {}
        }
    }

    fn right_click(&mut self) {
        let (x, y) = (self.pointer_at.0 as i32, self.pointer_at.1 as i32);
        let entry = match self.layout.hit(x, y) {
            Some(layout::Hit::Window(i)) => self.dock_apps().get(i).and_then(|app| app.entry),
            Some(layout::Hit::Row(k) | layout::Hit::Pin(k)) => {
                self.menu.as_ref().and_then(|m| match m.at_row(k) {
                    Some(Row::App(e)) => Some(e),
                    _ => None,
                })
            }
            _ => None,
        };
        if let Some(e) = entry {
            self.toggle_pin(e);
        }
    }

    fn key(&mut self, event: KeyEvent, serial: u32, qh: &QueueHandle<Self>) {
        if self.session.open {
            match event.keysym {
                Keysym::Escape => {
                    if self.session.confirm.is_some() {
                        self.session.activate(session::Hit::Cancel);
                        self.draw_band();
                    } else {
                        self.close_session();
                    }
                }
                Keysym::Return | Keysym::KP_Enter => {
                    self.session_click(self.session.selected_hit())
                }
                _ => self.edit(event),
            }
            return;
        }
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        match event.keysym {
            Keysym::Escape => {
                if !self.menu.as_mut().is_some_and(|m| m.back(&self.entries)) {
                    self.close_menu();
                } else {
                    self.relayout();
                }
            }
            Keysym::Insert => {
                if let Some(Row::App(e)) = menu.selected() {
                    self.toggle_pin(e);
                }
            }
            Keysym::Return | Keysym::KP_Enter => {
                if let Some(e) = menu.selected() {
                    self.open_row(e, serial, qh);
                }
            }
            _ => self.edit(event),
        }
    }

    /// Typing and moving in the menu; the keys that repeat while held.
    fn edit(&mut self, event: KeyEvent) {
        if self.session.open {
            match event.keysym {
                Keysym::Up | Keysym::Left => self.session.step(-1),
                Keysym::Down | Keysym::Right | Keysym::Tab => self.session.step(1),
                _ => return,
            }
            self.draw_band();
            return;
        }
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        match event.keysym {
            Keysym::BackSpace => menu.pop(&self.entries),
            Keysym::Left => {
                menu.back(&self.entries);
            }
            Keysym::Up => menu.step(-1),
            Keysym::Down => menu.step(1),
            Keysym::Page_Up | Keysym::KP_Page_Up => menu.page(-1),
            Keysym::Page_Down | Keysym::KP_Page_Down => menu.page(1),
            Keysym::Home | Keysym::KP_Home => menu.home(),
            Keysym::End | Keysym::KP_End => menu.end(),
            Keysym::Escape | Keysym::Return | Keysym::KP_Enter => return,
            _ => {
                let text = event.utf8.unwrap_or_default();
                for c in text.chars().filter(|c| !c.is_control()) {
                    menu.push(c, &self.entries);
                }
            }
        }
        self.relayout();
    }

    fn draw(&mut self) {
        if !self.configured {
            return;
        }
        let l = self.layout;
        let s = self.scale.max(1);
        let Some(mut canvas) = Pixmap::new((l.w * s) as u32, (l.h * s) as u32) else {
            return;
        };
        let apps = self.dock_apps();
        let slots: Vec<Slot> = apps
            .iter()
            .map(|app| Slot {
                focused: app.focused,
                minimized: app.minimized,
                attention: app.attention,
                running: app.windows.len(),
                pinned: app.pinned,
            })
            .collect();
        let items = l.draw(
            &slots,
            self.menu.as_ref(),
            &self.entries,
            &self.pins.ids,
            self.hover,
            &self.pin_message,
        );
        if let Ok(region) = Region::new(&self.compositor) {
            for r in [Some(l.bar()), l.panel()].into_iter().flatten() {
                region.add(r.x, r.y, r.w, r.h);
            }
            self.layer
                .wl_surface()
                .set_input_region(Some(region.wl_region()));
        }
        for item in items {
            match item {
                Item::Fill(..) | Item::Round(..) | Item::Panel(..) | Item::Text(..) => {
                    paint(&mut canvas, self.font.as_ref(), item, s)
                }
                Item::Icon(of, r, alpha) => {
                    let r = scaled(r, s);
                    let size = r.w as u32;
                    let px = match of {
                        IconOf::Button => self.icons.navi(size),
                        IconOf::Window(i) => {
                            let app = &apps[i];
                            let names = match app.entry {
                                Some(e) => self.entries[e]
                                    .icon
                                    .iter()
                                    .cloned()
                                    .chain(["application-x-executable".into()])
                                    .collect(),
                                None => icons::app_icon_names(&app.app_id, &self.entries),
                            };
                            self.icons.get(&format!("app:{}", app.app_id), &names, size)
                        }
                        IconOf::Folder(category) => self.icons.category(category, size),
                        IconOf::Session(name) => self.icons.get(name, &[name.into()], size),
                        IconOf::Entry(e) => {
                            let entry = &self.entries[e];
                            let names: Vec<String> = entry.icon.iter().cloned().collect();
                            self.icons
                                .get(&format!("icon:{}", names.join("")), &names, size)
                        }
                    };
                    match px {
                        Some(px) => draw_icon(&mut canvas, px, r, alpha, of.tint()),
                        None => placeholder(&mut canvas, r),
                    }
                }
                Item::Field(query, r) => {
                    let r = scaled(r, s);
                    let px = text_px() * s as f32;
                    let end = match &self.font {
                        Some(font) => draw_text(&mut canvas, font, &query, r, px, INK),
                        None => r.x,
                    };
                    // The caret: after the query, or before the placeholder.
                    let h = (px * 1.3) as i32;
                    let caret = Rect {
                        x: end + s,
                        y: r.y + (r.h - h) / 2,
                        w: s,
                        h,
                    };
                    fill(&mut canvas, caret, INK, 1.0);
                    if let (Some(font), true) = (&self.font, query.is_empty()) {
                        let rest = Rect {
                            x: caret.x + 4 * s,
                            w: r.w - 4 * s,
                            ..r
                        };
                        draw_text(&mut canvas, font, "Search all apps…", rest, px, DIM);
                    }
                }
            }
        }
        present(&mut self.pool, &self.layer, &canvas, s);
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn close_session(&mut self) {
        if self.session.open {
            self.session.close();
            self.draw_band();
            self.resume_cards();
        }
    }

    fn toggle_session(&mut self) {
        if self.session.open {
            return self.close_session();
        }
        self.close_menu();
        self.session.open = true;
        self.session_since = self.now_ms();
        self.session.selected = 0;
        if !self.session.busy {
            self.session.busy = true;
            let (tx, full, seat) = (
                self.session_tx.clone(),
                self.full_session,
                std::env::var("XDG_SEAT_PATH").ok(),
            );
            let control = self.control.clone();
            std::thread::spawn(move || {
                let can_lock = control
                    .as_ref()
                    .is_some_and(|c| c.lock().is_ok_and(|s| control::request(&s, false).is_ok()));
                let _ = tx.send(session::Reply::Capabilities(session::capabilities(
                    full, seat, can_lock,
                )));
            });
        }
        self.draw_band();
        self.show_cards();
    }

    fn session_click(&mut self, hit: session::Hit) {
        if let Some(action) = self.session.activate(hit) {
            if action == session::Action::Logout {
                self.logout = true;
                self.closed = true;
                return;
            }
            self.session.busy = true;
            self.session.message = format!("Requesting {}…", action.label());
            let (tx, full, caps) = (
                self.session_tx.clone(),
                self.full_session,
                self.session.capabilities.clone(),
            );
            let control = self.control.clone();
            std::thread::spawn(move || {
                let result = session::perform_locked(action, full, &caps, || {
                    let c = control.as_ref().ok_or("Lock channel unavailable")?;
                    let s = c.lock().map_err(|_| "Lock channel unavailable")?;
                    control::request(&s, true)
                });
                let _ = tx.send(session::Reply::Done(result));
            });
        }
        self.draw_band();
    }

    fn session_reply(&mut self, reply: session::Reply) {
        self.session.busy = false;
        match reply {
            session::Reply::Capabilities(caps) => self.session.capabilities = caps,
            session::Reply::Done(Ok(())) => self.close_session(),
            session::Reply::Done(Err(e)) => {
                runtime::play(&self.desktop_settings, "warning");
                warn!(error = %e, "session action failed");
                self.session.message = if e.contains("lock") || e.contains("Lock") {
                    "Could not lock. Install swaylock;\ncheck the Navi log and try again.".into()
                } else {
                    "Request failed. See the session log,\nthen try again.".into()
                };
            }
        }
        self.draw_band();
    }

    fn refresh_status(&mut self) {
        let battery = status::battery(Path::new(POWER));
        if battery.is_some_and(|b| !b.charging && b.percent <= 15)
            && self.battery.is_some_and(|b| b.percent > 15)
        {
            runtime::play(&self.desktop_settings, "battery");
        }
        let status = status::format(&jiff::Zoned::now(), battery);
        if status != self.status {
            self.status = status;
            self.battery = battery;
            self.draw_band();
        }
    }

    /// A deliberate status bar: identity left, clock centered, power right.
    fn draw_band(&mut self) {
        if !self.band_configured {
            return;
        }
        let s = self.scale.max(1);
        let h = self.session.height();
        self.band.set_size(0, h as u32);
        if let Ok(region) = Region::new(&self.compositor) {
            for r in self.session.input() {
                region.add(r.x, r.y, r.w, r.h);
            }
            self.band
                .wl_surface()
                .set_input_region(Some(region.wl_region()));
        }
        let Some(mut canvas) = Pixmap::new((self.band_w * s) as u32, (h * s) as u32) else {
            return;
        };
        let measured = |text: String| {
            let w = self
                .font
                .as_ref()
                .map_or(0.0, |f| text_w(f, &text, text_px()));
            (text, w.ceil() as i32)
        };
        let now = jiff::Zoned::now();
        let clock = measured(
            now.strftime(if self.band_w < 520 {
                "%H:%M"
            } else {
                "%a %d %b   %H:%M"
            })
            .to_string(),
        );
        let battery = self.battery.map(|b| {
            let (text, w) = measured(format!(
                "{}{}%",
                if b.charging { "Charging  " } else { "" },
                b.percent
            ));
            (text, w, b)
        });
        let lit = self.session.open || self.band_hover;
        let band = session::band(self.band_w, lit, clock, battery);
        for item in band.into_iter().chain(self.session.draw()) {
            if let Item::Icon(IconOf::Button, r, _) = item {
                let r = scaled(r, s);
                if let Some(icon) = self.icons.navi(r.w as u32) {
                    let (paint, t) = (PixmapPaint::default(), Transform::identity());
                    canvas.draw_pixmap(r.x, r.y, icon.as_ref(), &paint, t, None);
                }
            } else if let Item::Icon(of @ IconOf::Session(name), r, alpha) = item {
                let r = scaled(r, s);
                if let Some(icon) =
                    self.icons
                        .get(name, &[name.into(), format!("{name}-symbolic")], r.w as u32)
                {
                    draw_icon(&mut canvas, icon, r, alpha, of.tint());
                } else {
                    placeholder(&mut canvas, r);
                }
            } else {
                paint(&mut canvas, self.font.as_ref(), item, s);
            }
        }
        present(&mut self.pool, &self.band, &canvas, s);
    }

    fn call(&mut self, call: Call) {
        let now = self.now_ms();
        match call {
            Call::Configure(settings) => {
                theme::configure(&settings);
                TEXT_SIZE.store(
                    settings["font_size"].as_u64().unwrap_or(14) as u32,
                    std::sync::atomic::Ordering::Relaxed,
                );
                if settings["icon_theme"] != self.desktop_settings["icon_theme"] {
                    let config = files::xdg_dir(
                        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
                        std::env::var_os("HOME").as_deref(),
                        ".config",
                    );
                    let preferred = settings["icon_theme"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| icons::preferred(config.as_deref()));
                    self.icons = Icons::new(Themes::new(
                        icons::bases(&self.data_dirs, std::env::var_os("HOME").as_deref()),
                        Some(PathBuf::from("/usr/share/pixmaps")),
                        &preferred,
                    ));
                }
                if settings["dnd"] == true || settings["notifications"] != true {
                    self.notes.clear_shown();
                }
                let blocked: Vec<_> = self
                    .notes
                    .shown()
                    .iter()
                    .filter(|n| {
                        settings["blocked_apps"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|v| v == &n.app_id))
                    })
                    .map(|n| n.id)
                    .collect();
                for id in blocked {
                    self.notes.hide(id);
                }
                if settings["history"] != true {
                    self.notes.history.clear();
                }
                self.desktop_settings = settings;
                self.draw();
                self.draw_band();
                self.show_cards();
            }
            Call::Settings(value, reply) => {
                let result = match value["op"].as_str() {
                    Some("get") => {
                        serde_json::json!({"ok":true,"settings":self.desktop_settings,"history":self.notes.history.iter().map(|n| serde_json::json!({
                        "id":n.id,"app":n.app,"app_id":n.app_id,"summary":n.summary,"body":n.body,
                        "actions":if n.expires_ms > now && self.notes.shown().iter().any(|shown| shown.id == n.id) { n.actions.clone() } else { Vec::new() }
                    })).collect::<Vec<_>>(),"push":self.runtime.status.lock().map(|s| s.clone()).unwrap_or_default()})
                    }
                    Some("clear") => {
                        self.notes.history.clear();
                        serde_json::json!({"ok":true})
                    }
                    Some("action") => {
                        let id = value["id"].as_u64().and_then(|v| u32::try_from(v).ok());
                        let key = value["key"].as_str().unwrap_or("");
                        let note = self.notes.shown().iter().find(|n| {
                            Some(n.id) == id
                                && n.expires_ms > now
                                && n.actions.iter().any(|(k, _)| k == key)
                        });
                        if let (Some(note), Some(conn)) = (note, self.dbus.as_ref()) {
                            // Settings' click lands in another process: no token to vouch for it.
                            dbus::action(conn, &note.sender, note.id, key, None);
                            serde_json::json!({"ok":true})
                        } else {
                            serde_json::json!({"ok":false,"error":"Notification action expired"})
                        }
                    }
                    Some("sound") => {
                        runtime::play(
                            &self.desktop_settings,
                            value["event"].as_str().unwrap_or(""),
                        );
                        serde_json::json!({"ok":true})
                    }
                    _ => serde_json::json!({"ok":false,"error":"Unknown shell setting"}),
                };
                let _ = reply.send(result);
            }
            Call::Notify(sender, r, reply) => {
                let (id, closed) = self.notes.notify(now, &sender, &r);
                let _ = reply.send(id);
                let Some(closed) = closed else {
                    return debug!(id, sender, "notification rate limited");
                };
                let lens = (r.app_name.len(), r.summary.len(), r.body.len());
                info!(id, sender, ?lens, "notification");
                self.posted(now, id, closed, &r.app_id, true);
            }
            Call::Close(sender, id) => {
                let closed = self.notes.close(&sender, id);
                self.emit(closed.into_iter().collect());
            }
        }
    }

    /// navi closed a window past its client's limit.
    fn window_limit(&mut self) {
        let now = self.now_ms();
        // A flood replaces the card each second; only the first one sounds.
        let repeat = self.notes.shown().iter().any(|n| n.sender == notify::LOCAL);
        let (id, closed) = self.notes.window_limit(now);
        info!(id, repeat, "window limit card");
        self.posted(now, id, closed, "", !repeat);
    }

    /// Applies the notification settings to note `id`, just posted, then shows it.
    fn posted(&mut self, now: u64, id: u32, mut closed: Vec<Closed>, app_id: &str, sound: bool) {
        let blocked = self.desktop_settings["blocked_apps"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == app_id));
        let suppressed = blocked
            || self.desktop_settings["dnd"] == true
            || self.desktop_settings["notifications"] != true;
        if suppressed {
            self.notes.hide(id);
        }
        if blocked || self.desktop_settings["history"] != true {
            self.notes.history.retain(|n| n.id != id);
        }
        if suppressed {
            closed.push((id, notify::EXPIRED));
        } else if sound {
            runtime::play(&self.desktop_settings, "notification");
        }
        self.emit(closed);
        let at = self.notes.shown().iter().find(|n| n.id == id);
        self.expire_at(at.map_or(now, |n| n.expires_ms));
    }

    /// Sends `NotificationClosed` for each, and shows what is left.
    fn emit(&mut self, closed: Vec<Closed>) {
        if let Some(conn) = &self.dbus {
            for c in closed {
                dbus::emit(conn, c);
            }
        }
        self.show_cards();
    }

    /// Expires shown notes at `at` ms, or later while the session menu hides them.
    fn expire_at(&mut self, at: u64) {
        let wait = Duration::from_millis(at.saturating_sub(self.now_ms()));
        let timer = self
            .loop_handle
            .insert_source(Timer::from_duration(wait), |_, _, bar| {
                if bar.session.open {
                    return TimeoutAction::ToDuration(Duration::from_millis(250));
                }
                let closed = bar.notes.expire(bar.now_ms());
                if !closed.is_empty() {
                    bar.emit(closed);
                }
                TimeoutAction::Drop
            });
        if let Err(e) = timer {
            warn!("notification timer: {e}");
        }
    }

    /// Shows the cards the session menu hid, giving back the time they were hidden.
    fn resume_cards(&mut self) {
        self.notes
            .delay(self.now_ms().saturating_sub(self.session_since));
        let expiries: Vec<u64> = self.notes.shown().iter().map(|n| n.expires_ms).collect();
        for at in expiries {
            self.expire_at(at);
        }
        self.show_cards();
    }

    /// Lays out the shown notifications, creating, resizing or destroying their surface.
    fn show_cards(&mut self) {
        let font = self.font.as_ref();
        let advance = |c: char| match font {
            Some(f) => {
                (f.lookup_glyph_index(c) != 0).then(|| f.metrics(c, text_px()).advance_width)
            }
            None => Some(text_px() / 2.0),
        };
        // Cards share the session menu's corner; they wait while it's open.
        let shown = if self.session.open {
            &[][..]
        } else {
            self.notes.shown()
        };
        let cards = cards::layout(shown, &advance);
        if cards == self.cards && self.cards_layer.is_some() {
            return;
        }
        self.cards = cards;
        self.cards_changed_ms = self.now_ms();
        if self.cards.is_empty() {
            // Dropping it destroys the surface.
            self.cards_layer = None;
            return;
        }
        let size = cards::size(&self.cards);
        let layer = match &self.cards_layer {
            Some(l) if size == self.cards_size => return self.draw_cards(),
            Some(l) => l.clone(),
            None => {
                let qh = &self.qh;
                let surface = self.compositor.create_surface(qh);
                let ns = Some("navi-notifications");
                let l = self
                    .layer_shell
                    .create_layer_surface(qh, surface, Layer::Top, ns, None);
                l.set_anchor(Anchor::TOP | Anchor::LEFT);
                let [top, right, bottom, left] = cards::MARGIN;
                l.set_margin(top, right, bottom, left);
                l.set_exclusive_zone(0);
                l.set_keyboard_interactivity(KeyboardInteractivity::None);
                self.cards_configured = false;
                self.cards_layer = Some(l.clone());
                l
            }
        };
        self.cards_size = size;
        layer.set_size(size.0 as u32, size.1 as u32);
        // Drawn on the configure that follows.
        layer.commit();
    }

    fn draw_cards(&mut self) {
        let (Some(layer), true) = (&self.cards_layer, self.cards_configured) else {
            return;
        };
        let s = self.scale.max(1);
        let (w, h) = self.cards_size;
        let Some(mut canvas) = Pixmap::new((w * s) as u32, (h * s) as u32) else {
            return;
        };
        for item in cards::draw(&self.cards, self.desktop_settings["visual_alerts"] == true) {
            paint(&mut canvas, self.font.as_ref(), item, s);
        }
        present(&mut self.pool, layer, &canvas, s);
    }
}

/// Draws `canvas` on `layer`'s surface at buffer scale `s`.
fn present(pool: &mut SlotPool, layer: &LayerSurface, canvas: &Pixmap, s: i32) {
    let (w, h) = (canvas.width() as i32, canvas.height() as i32);
    let (buffer, data) = match pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) {
        Ok(b) => b,
        Err(e) => return warn!("no buffer: {e}"),
    };
    // Premultiplied RGBA to little-endian ARGB, which is BGRA in memory.
    for (d, p) in data.as_chunks_mut::<4>().0.iter_mut().zip(canvas.pixels()) {
        *d = [p.blue(), p.green(), p.red(), p.alpha()];
    }
    let surface = layer.wl_surface();
    surface.set_buffer_scale(s);
    surface.damage_buffer(0, 0, w, h);
    if let Err(e) = buffer.attach_to(surface) {
        return warn!("attach: {e}");
    }
    layer.commit();
}

/// A fill or a line of text; other items are the taskbar's.
fn paint(canvas: &mut Pixmap, font: Option<&fontdue::Font>, item: Item, s: i32) {
    match item {
        Item::Fill(r, rgb) => fill(canvas, scaled(r, s), rgb, 1.0),
        Item::Round(r, radius, rgb) => rounded(canvas, scaled(r, s), radius * s, rgb),
        Item::Panel(r, kind, edge, border) => {
            panel(canvas, scaled(r, s), kind, edge, border * s, s)
        }
        Item::Text(text, r, rgb) => {
            if let Some(font) = font {
                draw_text(canvas, font, &text, scaled(r, s), text_px() * s as f32, rgb);
            }
        }
        _ => {}
    }
}

fn text_w(font: &fontdue::Font, text: &str, px: f32) -> f32 {
    text.chars()
        .filter(|&c| font.lookup_glyph_index(c) != 0)
        .map(|c| font.metrics(c, px).advance_width)
        .sum()
}

fn scaled(r: Rect, s: i32) -> Rect {
    Rect {
        x: r.x * s,
        y: r.y * s,
        w: r.w * s,
        h: r.h * s,
    }
}

fn color(rgb: u32, alpha: f32) -> Color {
    let [_, r, g, b] = theme::resolve(rgb).to_be_bytes();
    let mut c = Color::from_rgba8(r, g, b, 255);
    c.apply_opacity(alpha);
    c
}

fn fill(canvas: &mut Pixmap, r: Rect, rgb: u32, alpha: f32) {
    let Some(rect) = tiny_skia::Rect::from_xywh(r.x as f32, r.y as f32, r.w as f32, r.h as f32)
    else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color(color(rgb, alpha));
    canvas.fill_rect(rect, &paint, Transform::identity(), None);
}

/// Rounded shell surfaces, rendered with the rasterizer already used for icons.
fn rounded(canvas: &mut Pixmap, r: Rect, radius: i32, rgb: u32) {
    let mut path = tiny_skia::PathBuilder::new();
    round_rect(&mut path, r, radius);
    fill_path(canvas, path, rgb, 1.0);
}

/// A floating panel: translucent body, opaque `border` px rim, both following the corners.
fn panel(canvas: &mut Pixmap, r: Rect, kind: theme::Panel, edge: u32, border: i32, s: i32) {
    let radius = theme::RADIUS * s;
    let mut body = tiny_skia::PathBuilder::new();
    round_rect(&mut body, r, radius);
    fill_path(canvas, body, theme::BAR_BG, theme::panel_alpha(kind));
    let mut rim = tiny_skia::PathBuilder::new();
    round_rect(&mut rim, r, radius);
    let inner = Rect {
        x: r.x + border,
        y: r.y + border,
        w: r.w - 2 * border,
        h: r.h - 2 * border,
    };
    round_rect(&mut rim, inner, radius - border);
    fill_path(canvas, rim, edge, 1.0);
}

/// Even-odd, so a second contour inside the first cuts it out.
fn fill_path(canvas: &mut Pixmap, path: tiny_skia::PathBuilder, rgb: u32, alpha: f32) {
    if let Some(path) = path.finish() {
        let mut paint = Paint::default();
        paint.set_color(color(rgb, alpha));
        let rule = tiny_skia::FillRule::EvenOdd;
        canvas.fill_path(&path, &paint, rule, Transform::identity(), None);
    }
}

/// Adds `r` with rounded corners to `path` as one closed contour.
fn round_rect(path: &mut tiny_skia::PathBuilder, r: Rect, radius: i32) {
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let radius = (radius as f32).min(w / 2.0).min(h / 2.0).max(0.0);
    let k = radius * 0.5522848;
    let (right, bottom) = (x + w, y + h);
    path.move_to(x + radius, y);
    path.line_to(right - radius, y);
    path.cubic_to(
        right - radius + k,
        y,
        right,
        y + radius - k,
        right,
        y + radius,
    );
    path.line_to(right, bottom - radius);
    path.cubic_to(
        right,
        bottom - radius + k,
        right - radius + k,
        bottom,
        right - radius,
        bottom,
    );
    path.line_to(x + radius, bottom);
    path.cubic_to(
        x + radius - k,
        bottom,
        x,
        bottom - radius + k,
        x,
        bottom - radius,
    );
    path.line_to(x, y + radius);
    path.cubic_to(x, y + radius - k, x + radius - k, y, x + radius, y);
    path.close();
}

/// `icon` at `r`, its colour replaced by `tint` (its shape kept) when given.
fn draw_icon(canvas: &mut Pixmap, icon: &Pixmap, r: Rect, alpha: f32, tint: Option<u32>) {
    let paint = PixmapPaint {
        opacity: alpha,
        ..Default::default()
    };
    let t = Transform::identity();
    match tint {
        Some(rgb) => {
            let tinted = tinted(icon, rgb);
            canvas.draw_pixmap(r.x, r.y, tinted.as_ref(), &paint, t, None)
        }
        None => canvas.draw_pixmap(r.x, r.y, icon.as_ref(), &paint, t, None),
    }
}

/// `icon`'s coverage in `rgb`.
fn tinted(icon: &Pixmap, rgb: u32) -> Pixmap {
    let mut out = icon.clone();
    let [_, r, g, b] = theme::resolve(rgb).to_be_bytes();
    for p in out.pixels_mut() {
        let a = p.alpha();
        let k = |c: u8| (u16::from(c) * u16::from(a) / 255) as u8;
        if let Some(c) = PremultipliedColorU8::from_rgba(k(r), k(g), k(b), a) {
            *p = c;
        }
    }
    out
}

/// A dim outlined square where an icon couldn't be found.
fn placeholder(canvas: &mut Pixmap, r: Rect) {
    let i = r.w / 8;
    for (x, y, w, h) in [
        (r.x + i, r.y + i, r.w - 2 * i, 2),
        (r.x + i, r.y + r.h - i - 2, r.w - 2 * i, 2),
        (r.x + i, r.y + i, 2, r.h - 2 * i),
        (r.x + r.w - i - 2, r.y + i, 2, r.h - 2 * i),
    ] {
        fill(canvas, Rect { x, y, w, h }, DIM, 1.0);
    }
}

/// `text` in `font` at `px`, vertically centred in `r` and cut to its width. Returns where the
/// text ends.
fn draw_text(
    canvas: &mut Pixmap,
    font: &fontdue::Font,
    text: &str,
    r: Rect,
    px: f32,
    rgb: u32,
) -> i32 {
    let advance =
        |c: char| (font.lookup_glyph_index(c) != 0).then(|| font.metrics(c, px).advance_width);
    let Some(line) = font.horizontal_line_metrics(px) else {
        return r.x;
    };
    let baseline = r.y as f32 + (r.h as f32 - (line.ascent - line.descent)) / 2.0 + line.ascent;
    let [_, cr, cg, cb] = theme::resolve(rgb).to_be_bytes();
    let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);
    let mut pen = r.x as f32;
    for c in font::fit(text, r.w as f32, advance) {
        let (m, coverage) = font.rasterize(c, px);
        let x0 = (pen + m.xmin as f32).round() as i32;
        let y0 = (baseline - m.height as f32 - m.ymin as f32).round() as i32;
        let pixels = canvas.pixels_mut();
        for (k, &a) in coverage.iter().enumerate() {
            let (x, y) = (
                x0 + (k % m.width.max(1)) as i32,
                y0 + (k / m.width.max(1)) as i32,
            );
            if a == 0 || x < 0 || y < 0 || x >= cw || y >= ch {
                continue;
            }
            let d = &mut pixels[(y * cw + x) as usize];
            let mix = |src: u8, dst: u8| {
                ((u16::from(src) * u16::from(a) + u16::from(dst) * (255 - u16::from(a))) / 255)
                    as u8
            };
            let alpha = mix(255, d.alpha());
            let (r, g, b) = (mix(cr, d.red()), mix(cg, d.green()), mix(cb, d.blue()));
            if let Some(p) =
                PremultipliedColorU8::from_rgba(r.min(alpha), g.min(alpha), b.min(alpha), alpha)
            {
                *d = p;
            }
        }
        pen += m.advance_width;
    }
    pen.round() as i32
}

/// Runs `e`'s argv with no shell, detached in its own process group, with nothing of navi-bar's
/// but its environment minus `WAYLAND_SOCKET`. A thread reaps it.
fn spawn(e: &Entry, token: Option<&str>) {
    let Some((program, args)) = e.argv.split_first() else {
        return;
    };
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_remove("WAYLAND_SOCKET")
        .env_remove("NAVI_CONTROL_FD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Ok(settings) = desktop_config::load() {
        cmd.env("XCURSOR_SIZE", settings["cursor_size"].to_string());
        if let Some(theme) = settings["cursor_theme"].as_str().filter(|s| !s.is_empty()) {
            cmd.env("XCURSOR_THEME", theme);
        } else {
            let dirs = cursor::dirs(
                std::env::var_os("XDG_DATA_HOME").as_deref(),
                std::env::var_os("HOME").as_deref(),
                std::env::var_os("XDG_DATA_DIRS").as_deref(),
            );
            cmd.env("XCURSOR_THEME", cursor::theme(&dirs).unwrap_or("default"));
        }
        cmd.env(
            "GTK_THEME",
            if settings["scheme"] == "light" {
                "Adwaita"
            } else {
                "Adwaita:dark"
            },
        );
    }
    match token {
        Some(t) => cmd
            .env("XDG_ACTIVATION_TOKEN", t)
            .env("DESKTOP_STARTUP_ID", t),
        None => cmd
            .env_remove("XDG_ACTIVATION_TOKEN")
            .env_remove("DESKTOP_STARTUP_ID"),
    };
    match cmd.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            info!(id = e.id, argv = ?e.argv, pid, token = token.is_some(), "launch");
            std::thread::spawn(move || child.wait());
        }
        Err(err) => warn!(id = e.id, argv = ?e.argv, "launch failed: {err}"),
    }
}

impl ActivationHandler for Bar {
    type RequestData = TokenRequest;

    fn new_token(&mut self, token: String, data: &TokenRequest) {
        if let Some(p) = self.take_pending(data.seq) {
            self.go_on(p, Some(&token));
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Bar {
    fn event(
        bar: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            manager::Event::Toplevel { toplevel } => bar.windows.push(Win {
                handle: toplevel,
                app_id: String::new(),
                minimized: false,
                activated: false,
                attention: false,
                ready: false,
            }),
            manager::Event::Finished => warn!("window list finished"),
            _ => {}
        }
    }

    event_created_child!(Bar, ZwlrForeignToplevelManagerV1, [
        manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Bar {
    fn event(
        bar: &mut Self,
        h: &ZwlrForeignToplevelHandleV1,
        event: handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(at) = bar.windows.iter().position(|w| &w.handle == h) else {
            return;
        };
        let w = &mut bar.windows[at];
        match event {
            handle::Event::AppId { app_id } => w.app_id = app_id,
            handle::Event::State { state } => {
                let states: Vec<u32> = state
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&b| u32::from_ne_bytes(b))
                    .collect();
                w.minimized = states.contains(&(handle::State::Minimized as u32));
                w.activated = states.contains(&(handle::State::Activated as u32));
                w.attention = states.contains(&control::ATTENTION);
            }
            handle::Event::Done => {
                w.ready = true;
                bar.relayout();
            }
            handle::Event::Closed => {
                h.destroy();
                bar.windows.remove(at);
                bar.relayout();
            }
            _ => {}
        }
    }
}

impl LayerShellHandler for Bar {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        // Closed before its first configure, navi refused it: making another would loop.
        let refused = (layer == &self.layer && !self.configured)
            || (layer == &self.band && !self.band_configured);
        if refused {
            warn!("layer surface closed by navi");
            self.closed = true;
        } else if layer == &self.layer {
            // Moved to another output, or its output went away: navi places the new one.
            info!("taskbar closed by navi, making a new one");
            self.layer = bar_layer(&self.compositor, &self.layer_shell, &self.qh, &self.layout);
            self.configured = false;
        } else if layer == &self.band {
            info!("status band closed by navi, making a new one");
            self.band_configured = false; // Nothing draws on the closed band.
            self.close_session();
            match band_layer(&self.compositor, &self.layer_shell, &self.qh) {
                Ok(b) => {
                    self.band = b;
                    self.band_configured = false;
                }
                Err(e) => {
                    warn!("status band not remade: {e}");
                    self.closed = true;
                }
            }
        } else if self.cards_layer.as_ref() == Some(layer) {
            // The next notification makes a new one.
            self.cards_layer = None;
            self.cards.clear();
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if layer == &self.layer {
            self.configured = true;
            self.draw();
        } else if layer == &self.band {
            self.band_configured = true;
            self.band_w = configure.new_size.0 as i32;
            self.draw_band();
        } else if self.cards_layer.as_ref() == Some(layer) {
            self.cards_configured = true;
            self.draw_cards();
        }
    }
}

impl CompositorHandler for Bar {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if factor != self.scale {
            self.scale = factor;
            self.draw();
            self.draw_band();
            self.draw_cards();
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl SeatHandler for Bar {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.seat.get_or_insert(seat);
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        self.seat.get_or_insert(seat.clone());
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self
                .seat_state
                .get_keyboard_with_repeat::<Bar, Bar>(
                    qh,
                    &seat,
                    None,
                    self.loop_handle.clone(),
                    Box::new(|bar, _, event| bar.edit(event)),
                )
                .ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(k) = self.keyboard.take()
        {
            k.release();
        }
        if capability == Capability::Pointer
            && let Some(p) = self.pointer.take()
        {
            p.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Bar {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    /// Clicking anything else closes the menu.
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if surface == self.band.wl_surface() {
            self.close_session();
        }
        if surface == self.layer.wl_surface() {
            self.close_menu();
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.key(event, serial, qh);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: u32,
    ) {
    }
}

impl PointerHandler for Bar {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for e in events {
            if &e.surface == self.band.wl_surface() {
                let (x, y) = (e.position.0 as i32, e.position.1 as i32);
                match e.kind {
                    PointerEventKind::Press {
                        button: BTN_LEFT, ..
                    } => {
                        if session::BUTTON.contains(x, y) {
                            self.toggle_session();
                        } else if let Some(hit) = self.session.hit(x, y) {
                            self.session_click(hit);
                        }
                    }
                    PointerEventKind::Motion { .. }
                    | PointerEventKind::Enter { .. }
                    | PointerEventKind::Leave { .. } => {
                        let hover = !matches!(e.kind, PointerEventKind::Leave { .. })
                            && session::BUTTON.contains(x, y);
                        if self.band_hover != hover {
                            self.band_hover = hover;
                            self.draw_band();
                        }
                        if let Some(hit) = self.session.hit(x, y) {
                            let selected = match hit {
                                session::Hit::Row(i) => i,
                                session::Hit::Cancel => 0,
                                session::Hit::Confirm => 1,
                            };
                            if selected != self.session.selected {
                                self.session.selected = selected;
                                self.draw_band();
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
            let on_cards = self.cards_layer.as_ref().map(|l| l.wl_surface()) == Some(&e.surface);
            if on_cards {
                if let PointerEventKind::Press {
                    button: BTN_LEFT,
                    serial,
                    ..
                } = e.kind
                {
                    let (x, y) = (e.position.0 as i32, e.position.1 as i32);
                    let clicked = cards::hit(&self.cards, x, y);
                    let action = clicked.and_then(|id| self.notes.default_action(id));
                    let dismissed = clicked.and_then(|id| self.notes.dismiss(id));
                    let armed = cards::armed(self.cards_changed_ms, self.now_ms());
                    match action.filter(|_| self.dbus.is_some()) {
                        // The token lets the app raise itself. Its card hides now; its
                        // `NotificationClosed` follows the action, or clients waiting on the
                        // card would quit before it.
                        Some((id, sender)) if armed => {
                            let action = Pending::Action(id, sender, dismissed);
                            self.ask_token(action, None, serial, e.surface.clone(), qh);
                            self.show_cards();
                        }
                        // Cards that just appeared or moved: the action alone, as from Settings.
                        Some((id, sender)) => {
                            self.go_on(Pending::Action(id, sender, dismissed), None)
                        }
                        None => self.emit(dismissed.into_iter().collect()),
                    }
                }
                continue;
            }
            if &e.surface != self.layer.wl_surface() {
                continue;
            }
            self.pointer_at = e.position;
            match e.kind {
                PointerEventKind::Press {
                    button: BTN_LEFT,
                    serial,
                    ..
                } => self.click(serial, qh),
                PointerEventKind::Press {
                    button: BTN_RIGHT, ..
                } => self.right_click(),
                PointerEventKind::Enter { .. }
                | PointerEventKind::Motion { .. }
                | PointerEventKind::Leave { .. } => {
                    if matches!(e.kind, PointerEventKind::Leave { .. })
                        && let Some(menu) = self.menu.as_mut()
                    {
                        menu.stop_scroll();
                    }
                    let hover = if matches!(e.kind, PointerEventKind::Leave { .. }) {
                        None
                    } else {
                        self.layout.hit(e.position.0 as i32, e.position.1 as i32)
                    };
                    let selected = match (self.menu.as_mut(), hover) {
                        (Some(menu), Some(layout::Hit::Row(k) | layout::Hit::Pin(k))) => {
                            menu.select_row(k)
                        }
                        _ => false,
                    };
                    if hover != self.hover || selected {
                        self.hover = hover;
                        self.draw();
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    if let Some(m) = self.menu.as_mut()
                        && m.scroll_axis(vertical.absolute, vertical.discrete, vertical.stop)
                    {
                        self.draw();
                    }
                }
                _ => {}
            }
        }
    }
}

impl OutputHandler for Bar {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Bar {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Bar {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Bar);
delegate_output!(Bar);
delegate_shm!(Bar);
delegate_seat!(Bar);
delegate_keyboard!(Bar);
delegate_pointer!(Bar);
delegate_layer!(Bar);
delegate_registry!(Bar);
delegate_activation!(Bar, TokenRequest);

#[cfg(test)]
mod tests {
    use super::*;
    use desktop::{Category, Entry};
    use layout::Hit;
    use theme::{ACCENT, INSET, ON_FILL, PANEL_EDGE, PANEL_TEXT, Panel};

    fn note(id: u32) -> notify::Note {
        notify::Note {
            id,
            sender: ":1.1".into(),
            app: "App".into(),
            summary: "Hi".into(),
            body: "body".into(),
            expires_ms: 0,
            app_id: String::new(),
            actions: Vec::new(),
        }
    }

    /// Every panel in each state that changes its text: dock, launcher, session menu, band, cards.
    fn every_state() -> Vec<Vec<Item>> {
        let entries = [Entry {
            id: "t.desktop".into(),
            name: "Terminal".into(),
            icon: None,
            argv: vec!["t".into()],
            wm_class: None,
            category: Category::System,
        }];
        let slots = [
            Slot {
                focused: true,
                running: 1,
                ..Slot::default()
            },
            Slot {
                pinned: true,
                ..Slot::default()
            },
            Slot {
                minimized: true,
                running: 2,
                ..Slot::default()
            },
        ];
        let mut out = Vec::new();
        for hover in [None, Some(Hit::Button), Some(Hit::Window(0))] {
            out.push(Layout::new(3, false).draw(&slots, None, &entries, &[], hover, ""));
        }
        let open = Layout::new(3, true);
        let pinned = ["t.desktop".to_string()];
        let launch = |m: &Menu, hover, pins: &[String], message| {
            open.draw(&slots, Some(m), &entries, pins, hover, message)
        };
        let hovers = [None, Some(Hit::Row(0)), Some(Hit::Pin(0)), Some(Hit::Back)];
        let mut m = Menu::new(&entries);
        for hover in hovers {
            out.push(launch(&m, hover, &[], ""));
        }
        m.open(Category::System, &entries);
        for pins in [&[][..], &pinned[..]] {
            for hover in hovers {
                out.push(launch(&m, hover, pins, "Pinned Terminal"));
            }
        }
        m.set_query("zzz", &entries);
        out.push(launch(&m, None, &[], ""));
        out.push(open.draw(&slots, Some(&Menu::new(&[])), &[], &[], None, ""));

        let mut s = session::Menu {
            open: true,
            ..Default::default()
        };
        for available in [[false; 6], [true; 6]] {
            s.capabilities.available = available;
            for selected in 0..6 {
                s.selected = selected;
                for full in [false, true] {
                    s.full = full;
                    out.push(s.draw());
                }
            }
        }
        s.busy = true;
        out.push(s.draw());
        s.busy = false;
        s.activate(session::Hit::Row(5));
        for selected in 0..2 {
            s.selected = selected;
            for full in [false, true] {
                s.full = full;
                out.push(s.draw());
            }
        }

        for lit in [false, true] {
            for charging in [None, Some(false), Some(true)] {
                let battery = charging.map(|charging| {
                    let b = status::Battery {
                        percent: 50,
                        charging,
                    };
                    ("50%".to_string(), 30, b)
                });
                out.push(session::band(800, lit, ("12:00".into(), 40), battery));
            }
        }

        let cards = cards::layout(&[note(2), note(1)], &|_| Some(8.0));
        out.extend([cards::draw(&cards, false), cards::draw(&cards, true)]);
        out
    }

    #[test]
    fn ac3_every_text_is_one_proven_on_its_backdrop() {
        let mut seen = 0;
        for items in every_state() {
            for (k, item) in items.iter().enumerate() {
                let (r, colors) = match item {
                    Item::Text(_, r, c) => (*r, vec![*c]),
                    // The query in INK, the placeholder in DIM.
                    Item::Field(_, r) => (*r, vec![INK, DIM]),
                    _ => continue,
                };
                let (x, y) = (r.x + r.w / 2, r.y + r.h / 2);
                // The nearest item behind the text's centre: an opaque fill, or the panel.
                let under = items[..k].iter().rev().find_map(|i| match i {
                    Item::Round(f, _, c) | Item::Fill(f, c) if f.contains(x, y) => Some(Some(*c)),
                    Item::Panel(f, ..) if f.contains(x, y) => Some(None),
                    _ => None,
                });
                for c in colors {
                    seen += 1;
                    let ok = match under.expect("every text is on a panel") {
                        None => PANEL_TEXT.contains(&c),
                        Some(fill) => ON_FILL
                            .iter()
                            .any(|&(f, texts)| f == fill && texts.contains(&c)),
                    };
                    assert!(ok, "{item:?} on {under:x?}");
                }
            }
        }
        assert!(seen > 200, "{seen} texts");
    }

    #[test]
    fn folder_and_session_glyphs_are_ink() {
        let mut seen = 0;
        for items in every_state() {
            for item in &items {
                if let Item::Icon(of @ (IconOf::Folder(_) | IconOf::Session(_)), ..) = item {
                    seen += 1;
                    // INK is proven 4.5:1 on panels and their fills, so well over 3:1.
                    assert!(
                        of.tint().is_some_and(|c| PANEL_TEXT.contains(&c)),
                        "{item:?}"
                    );
                }
            }
        }
        assert!(seen > 50, "{seen}");
        // The tint keeps the shape and replaces the colour.
        let mut icon = Pixmap::new(2, 1).unwrap();
        icon.pixels_mut()[0] = PremultipliedColorU8::from_rgba(40, 0, 80, 128).unwrap();
        let t = tinted(&icon, theme::INK);
        let [_, r, g, b] = theme::resolve(theme::INK).to_be_bytes();
        let k = |c: u8| (u16::from(c) * 128 / 255) as u8;
        let p = t.pixels()[0];
        assert_eq!(
            (p.red(), p.green(), p.blue(), p.alpha()),
            (k(r), k(g), k(b), 128)
        );
        assert_eq!(t.pixels()[1].alpha(), 0);
    }

    fn rendered(items: &[Item], w: i32, h: i32) -> Pixmap {
        let mut canvas = Pixmap::new(w as u32, h as u32).unwrap();
        for item in items {
            paint(&mut canvas, None, item.clone(), 1);
        }
        canvas
    }

    /// Corner clear, body at the panel alpha, rim in `edge` at the middle of the top edge.
    fn assert_panel(canvas: &Pixmap, r: Rect, body: (i32, i32), edge: u32, what: &str) {
        let px = |(x, y): (i32, i32)| canvas.pixel(x as u32, y as u32).unwrap();
        assert_eq!(px((r.x, r.y)).alpha(), 0, "{what}: corner");
        let kind = match what {
            "dock" | "band" => Panel::Bar,
            _ => Panel::Popup,
        };
        let want = (255.0 * theme::panel_alpha(kind)).round() as i32;
        let got = i32::from(px(body).alpha());
        assert!(
            (got - want).abs() <= 1,
            "{what}: body alpha {got}, want {want}"
        );
        let top = px((r.x + r.w / 2, r.y));
        let [_, er, eg, eb] = theme::resolve(edge).to_be_bytes();
        assert_eq!(
            (top.red(), top.green(), top.blue(), top.alpha()),
            (er, eg, eb, 255),
            "{what}: rim"
        );
    }

    #[test]
    fn ac6_panels_are_rounded_translucent_and_rimmed() {
        let check = || {
            let dock = Layout::new(1, false);
            let items = dock.draw(&[Slot::default()], None, &[], &[], None, "");
            let b = dock.bar();
            let canvas = rendered(&items, dock.w, dock.h);
            assert_panel(
                &canvas,
                b,
                (b.x + b.w / 2, b.y + b.h / 2),
                PANEL_EDGE,
                "dock",
            );
            // Below the dock the surface is clear.
            assert_eq!(canvas.pixel(0, (dock.h - 1) as u32).unwrap().alpha(), 0);

            let open = Layout::new(1, true);
            let items = open.draw(
                &[Slot::default()],
                Some(&Menu::new(&[])),
                &[],
                &[],
                None,
                "",
            );
            let p = open.panel().unwrap();
            let canvas = rendered(&items, open.w, open.h);
            assert_panel(
                &canvas,
                p,
                (p.x + p.w / 2, p.y + p.h / 2),
                PANEL_EDGE,
                "launcher",
            );

            let items = session::band(400, false, ("12:00".into(), 40), None);
            let r = Rect {
                x: 0,
                y: 0,
                w: 400,
                h: BAND_H,
            };
            let canvas = rendered(&items, 400, BAND_H);
            assert_panel(&canvas, r, (300, BAND_H / 2), PANEL_EDGE, "band");

            let cards = cards::layout(&[note(1)], &|_| Some(8.0));
            let (w, h) = cards::size(&cards);
            let canvas = rendered(&cards::draw(&cards, false), w, h);
            let c = cards[0].rect;
            assert_panel(&canvas, c, (c.x + c.w / 2, c.y + c.h / 2), ACCENT, "card");

            let menu = session::Menu {
                open: true,
                ..Default::default()
            };
            let p = menu.panel();
            let canvas = rendered(&menu.draw(), p.x + p.w, p.y + p.h);
            assert_panel(
                &canvas,
                p,
                (p.x + p.w - 20, p.y + 20),
                PANEL_EDGE,
                "session",
            );
        };
        check();
        theme::configure(&serde_json::json!({"high_contrast": true}));
        assert_eq!(theme::panel_alpha(Panel::Popup), 1.0);
        check();
        theme::configure(&serde_json::json!({}));
        assert_eq!(theme::panel_alpha(Panel::Bar), 0.85);
        assert_eq!(theme::panel_alpha(Panel::Popup), 0.98);
        assert_eq!(INSET, 12);
    }
}
