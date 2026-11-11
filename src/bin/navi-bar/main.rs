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
/// How long a launch waits for its activation token.
const TOKEN_WAIT: Duration = Duration::from_secs(1);
/// The status band across the top of the main output.
const BAND_H: i32 = session::BAND_H;
/// Between the status text and the band's right edge, and the cards and the output's edges.
const EDGE: i32 = 8;
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
        session: session::Menu::default(),
        control,
        band_hover: false,
        session_tx,
        full_session: dbus::wants_export(var("NAVI_EXPORT_ENV").as_deref()),
        logout: false,
        cards_layer: None,
        cards_size: (0, 0),
        cards_configured: false,
        cards: Vec::new(),
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
        launches: 0,
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
    launches: u64,
    /// Launches waiting for a token, by number.
    pending: Vec<(u64, Entry)>,
    closed: bool,
}

/// A token request for launch number `seq`.
struct Launch {
    seq: u64,
    data: RequestData,
}

impl RequestDataExt for Launch {
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
        self.launches += 1;
        let seq = self.launches;
        let data = RequestData {
            app_id: Some(e.id.clone()),
            seat_and_serial: self.seat.clone().map(|s| (s, serial)),
            surface: Some(self.layer.wl_surface().clone()),
        };
        self.activation
            .request_token_with_data(qh, Launch { seq, data });
        self.pending.push((seq, e));
        let timer = Timer::from_duration(TOKEN_WAIT);
        let wait = self.loop_handle.insert_source(timer, move |_, _, bar| {
            if let Some(e) = bar.take_pending(seq) {
                warn!(
                    id = e.id,
                    "no activation token within 1 s: launching without one"
                );
                spawn(&e, None);
            }
            TimeoutAction::Drop
        });
        if let Err(e) = wait {
            warn!("token timer: {e}");
        }
        self.close_menu();
    }

    fn take_pending(&mut self, seq: u64) -> Option<Entry> {
        let at = self.pending.iter().position(|p| p.0 == seq)?;
        Some(self.pending.remove(at).1)
    }

    fn running(&self) -> Vec<dock::Running<'_>> {
        let windows = self.shown().map(|w| dock::Running {
            app_id: &w.app_id,
            focused: w.activated,
            minimized: w.minimized,
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
                Item::Fill(..) | Item::Round(..) | Item::Text(..) => {
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
                        Some(px) => {
                            let paint = PixmapPaint {
                                opacity: alpha,
                                ..Default::default()
                            };
                            let t = Transform::identity();
                            canvas.draw_pixmap(r.x, r.y, px.as_ref(), &paint, t, None);
                        }
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
        }
    }

    fn toggle_session(&mut self) {
        if self.session.open {
            return self.close_session();
        }
        self.close_menu();
        self.session.open = true;
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
            let r = session::BUTTON;
            region.add(r.x, r.y, r.w, r.h);
            if self.session.open {
                let r = self.session.panel();
                region.add(r.x, r.y, r.w, r.h);
            }
            self.band
                .wl_surface()
                .set_input_region(Some(region.wl_region()));
        }
        let Some(mut canvas) = Pixmap::new((self.band_w * s) as u32, (h * s) as u32) else {
            return;
        };
        fill(
            &mut canvas,
            Rect {
                x: 0,
                y: 0,
                w: self.band_w * s,
                h: BAND_H * s,
            },
            theme::BAR_BG,
            1.0,
        );
        if self.session.open || self.band_hover {
            paint(
                &mut canvas,
                self.font.as_ref(),
                Item::Round(session::BUTTON, 8, theme::SELECTED),
                s,
            );
        }
        fill(
            &mut canvas,
            Rect {
                x: 0,
                y: (BAND_H - 1) * s,
                w: self.band_w * s,
                h: s,
            },
            theme::SHELL_EDGE,
            1.0,
        );
        if let Some(icon) = self.icons.navi((16 * s) as u32) {
            canvas.draw_pixmap(
                12 * s,
                8 * s,
                icon.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
        if let Some(font) = &self.font {
            let now = jiff::Zoned::now();
            let clock = now
                .strftime(if self.band_w < 520 {
                    "%H:%M"
                } else {
                    "%a %d %b   %H:%M"
                })
                .to_string();
            let w = text_w(font, &clock, text_px()).ceil() as i32;
            for item in [
                Item::Text(
                    "Navi".into(),
                    Rect {
                        x: 36,
                        y: 0,
                        w: 64,
                        h: BAND_H,
                    },
                    INK,
                ),
                Item::Text(
                    clock,
                    Rect {
                        x: (self.band_w - w) / 2,
                        y: 0,
                        w: w + 1,
                        h: BAND_H,
                    },
                    INK,
                ),
            ] {
                paint(&mut canvas, Some(font), item, s);
            }
            if let Some(b) = self.battery {
                let text = format!(
                    "{}{}%",
                    if b.charging { "Charging  " } else { "" },
                    b.percent
                );
                let w = text_w(font, &text, text_px()).ceil() as i32;
                let x = self.band_w - 12 - w;
                paint(
                    &mut canvas,
                    Some(font),
                    Item::Text(
                        text,
                        Rect {
                            x,
                            y: 0,
                            w: w + 1,
                            h: BAND_H,
                        },
                        INK,
                    ),
                    s,
                );
                let r = Rect {
                    x: x - 27,
                    y: 11,
                    w: 18,
                    h: 10,
                };
                for edge in [
                    Rect { h: 1, ..r },
                    Rect {
                        y: r.y + r.h - 1,
                        h: 1,
                        ..r
                    },
                    Rect { w: 1, ..r },
                    Rect {
                        x: r.x + r.w - 1,
                        w: 1,
                        ..r
                    },
                    Rect {
                        x: r.x + r.w,
                        y: r.y + 3,
                        w: 2,
                        h: 4,
                    },
                ] {
                    fill(&mut canvas, scaled(edge, s), DIM, 1.0);
                }
                let charge = Rect {
                    x: r.x + 2,
                    y: r.y + 2,
                    w: (14 * b.percent as i32 / 100).max(1),
                    h: 6,
                };
                fill(
                    &mut canvas,
                    scaled(charge, s),
                    if b.charging { theme::ACCENT } else { INK },
                    1.0,
                );
            }
        }
        for item in self.session.draw(self.full_session) {
            if let Item::Icon(IconOf::Session(name), r, alpha) = item {
                let r = scaled(r, s);
                if let Some(icon) =
                    self.icons
                        .get(name, &[name.into(), format!("{name}-symbolic")], r.w as u32)
                {
                    canvas.draw_pixmap(
                        r.x,
                        r.y,
                        icon.as_ref(),
                        &PixmapPaint {
                            opacity: alpha,
                            ..Default::default()
                        },
                        Transform::identity(),
                        None,
                    );
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
                            dbus::action(conn, &note.sender, note.id, key);
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
                let blocked = self.desktop_settings["blocked_apps"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == &r.app_id));
                let suppressed = blocked
                    || self.desktop_settings["dnd"] == true
                    || self.desktop_settings["notifications"] != true;
                if suppressed {
                    self.notes.hide(id);
                }
                if blocked || self.desktop_settings["history"] != true {
                    self.notes.history.retain(|n| n.id != id);
                }
                if !suppressed {
                    runtime::play(&self.desktop_settings, "notification");
                }
                let mut closed = closed;
                if suppressed {
                    closed.push((id, notify::EXPIRED));
                }
                self.emit(closed);
                let at = self.notes.shown().iter().find(|n| n.id == id);
                let wait = Duration::from_millis(at.map_or(0, |n| n.expires_ms - now));
                let timer =
                    self.loop_handle
                        .insert_source(Timer::from_duration(wait), |_, _, bar| {
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
            Call::Close(sender, id) => {
                let closed = self.notes.close(&sender, id);
                self.emit(closed.into_iter().collect());
            }
        }
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

    /// Lays out the shown notifications, creating, resizing or destroying their surface.
    fn show_cards(&mut self) {
        let font = self.font.as_ref();
        let advance = |c: char| match font {
            Some(f) => {
                (f.lookup_glyph_index(c) != 0).then(|| f.metrics(c, text_px()).advance_width)
            }
            None => Some(text_px() / 2.0),
        };
        let cards = cards::layout(self.notes.shown(), &advance);
        if cards == self.cards && self.cards_layer.is_some() {
            return;
        }
        self.cards = cards;
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
                l.set_margin(EDGE, 0, 0, EDGE);
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
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    let radius = (radius as f32).min(w / 2.0).min(h / 2.0).max(0.0);
    let k = radius * 0.5522848;
    let (right, bottom) = (x + w, y + h);
    let mut path = tiny_skia::PathBuilder::new();
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
    if let Some(path) = path.finish() {
        let mut paint = Paint::default();
        paint.set_color(color(rgb, 1.0));
        canvas.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
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
    type RequestData = Launch;

    fn new_token(&mut self, token: String, data: &Launch) {
        if let Some(e) = self.take_pending(data.seq) {
            spawn(&e, Some(&token));
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
            self.session.close();
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
                    button: BTN_LEFT, ..
                } = e.kind
                {
                    let (x, y) = (e.position.0 as i32, e.position.1 as i32);
                    let clicked = cards::hit(&self.cards, x, y);
                    if let Some(note) = self.notes.shown().iter().find(|n| Some(n.id) == clicked)
                        && note.actions.iter().any(|(k, _)| k == "default")
                        && let Some(conn) = &self.dbus
                    {
                        dbus::action(conn, &note.sender, note.id, "default");
                    }
                    let dismissed = clicked.and_then(|id| self.notes.dismiss(id));
                    self.emit(dismissed.into_iter().collect());
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
delegate_activation!(Bar, Launch);
