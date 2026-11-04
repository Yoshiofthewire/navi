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

mod cards;
mod dbus;
mod desktop;
mod exec;
mod font;
mod icons;
mod layout;
mod menu;
mod notify;
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
use menu::Menu;
use notify::{Closed, Notes};
use theme::{BG, DIM, INK};

const BTN_LEFT: u32 = 0x110;
const TEXT_PX: f32 = 15.0;
/// How long a launch waits for its activation token.
const TOKEN_WAIT: Duration = Duration::from_secs(1);
/// The status band across the top of the main output.
const BAND_H: i32 = 24;
/// Between the status text and the band's right edge, and the cards and the output's edges.
const EDGE: i32 = 8;
const BAND_ALPHA: f32 = 0.85;
const POWER: &str = "/sys/class/power_supply";

fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
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
    let themes = Themes::new(
        icons::bases(&data_dirs, var("HOME").as_deref()),
        Some(PathBuf::from("/usr/share/pixmaps")),
        icons::PREFERRED,
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
    let tick = Timer::from_duration(status::next_tick(&jiff::Zoned::now()));
    event_loop
        .handle()
        .insert_source(tick, |_, _, bar| {
            bar.refresh_status();
            TimeoutAction::ToDuration(status::next_tick(&jiff::Zoned::now()))
        })
        .map_err(|e| e.to_string())?;
    let mut bar = Bar {
        dbus: dbus::serve(tx),
        notes: Notes::default(),
        start: Instant::now(),
        band,
        band_configured: false,
        band_w: 0,
        status: String::new(),
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
        icons: Icons::new(themes),
        font,
        launches: 0,
        pending: Vec::new(),
        closed: false,
    };
    bar.refresh_status();
    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .map_err(|e| e.to_string())?;
    while !bar.closed {
        event_loop
            .dispatch(None, &mut bar)
            .map_err(|e| format!("connection lost: {e}"))?;
    }
    Ok(())
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

/// The status band across the top of the main output. Takes no clicks: its input region is
/// empty.
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
    band.set_keyboard_interactivity(KeyboardInteractivity::None);
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
    qh: QueueHandle<Bar>,
    compositor: CompositorState,
    layer_shell: LayerShell,
    /// The status band and its width, 0 until configured.
    band: LayerSurface,
    band_configured: bool,
    band_w: i32,
    status: String,
    /// The notification cards' surface, while any are shown.
    cards_layer: Option<LayerSurface>,
    cards_size: (i32, i32),
    cards_configured: bool,
    cards: Vec<Card>,
    notes: Notes,
    /// The bus connection, while serving notifications.
    dbus: Option<zbus::blocking::Connection>,
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
        let l = Layout::new(self.shown().count(), self.menu.is_some());
        if l == self.layout {
            return self.draw();
        }
        self.layout = l;
        self.layer.set_size(l.w as u32, l.h as u32);
        // Drawn on the configure that follows.
        self.layer.commit();
    }

    fn toggle_menu(&mut self) {
        self.menu = match self.menu {
            Some(_) => None,
            None => {
                self.entries = desktop::scan(&self.data_dirs);
                Some(Menu::new(&self.entries))
            }
        };
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

    fn click(&mut self, serial: u32, qh: &QueueHandle<Self>) {
        let (x, y) = (self.pointer_at.0 as i32, self.pointer_at.1 as i32);
        match self.layout.hit(x, y) {
            Some(layout::Hit::Button) => self.toggle_menu(),
            Some(layout::Hit::Window(i)) => {
                let win = self.shown().nth(i).map(|w| w.handle.clone());
                if let (Some(h), Some(seat)) = (win, &self.seat) {
                    h.activate(seat);
                }
            }
            Some(layout::Hit::Row(k)) => {
                if let Some(e) = self.menu.as_ref().and_then(|m| m.at_row(k)) {
                    self.launch(e, serial, qh);
                }
            }
            None => {}
        }
    }

    fn key(&mut self, event: KeyEvent, serial: u32, qh: &QueueHandle<Self>) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        match event.keysym {
            Keysym::Escape => self.close_menu(),
            Keysym::Return | Keysym::KP_Enter => {
                if let Some(e) = menu.selected() {
                    self.launch(e, serial, qh);
                }
            }
            _ => self.edit(event),
        }
    }

    /// Typing and moving in the menu; the keys that repeat while held.
    fn edit(&mut self, event: KeyEvent) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        match event.keysym {
            Keysym::BackSpace => menu.pop(&self.entries),
            Keysym::Up => menu.step(-1),
            Keysym::Down => menu.step(1),
            Keysym::Escape | Keysym::Return | Keysym::KP_Enter => return,
            _ => {
                let text = event.utf8.unwrap_or_default();
                for c in text.chars().filter(|c| !c.is_control()) {
                    menu.push(c, &self.entries);
                }
            }
        }
        self.draw();
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
        let slots: Vec<Slot> = self
            .shown()
            .map(|w| Slot {
                focused: w.activated,
                minimized: w.minimized,
            })
            .collect();
        let app_ids: Vec<String> = self.shown().map(|w| w.app_id.clone()).collect();
        let (query, rows, scroll) = match &self.menu {
            Some(m) => (m.query(), m.rows().collect(), m.scroll_pos()),
            None => ("", Vec::new(), (0, 0)),
        };
        let rows: Vec<(usize, bool, &str)> = rows
            .into_iter()
            .map(|(e, sel)| (e, sel, self.entries[e].name.as_str()))
            .collect();
        let items = l.draw(&slots, query, &rows, scroll);
        for item in items {
            match item {
                Item::Fill(..) | Item::Text(..) => paint(&mut canvas, self.font.as_ref(), item, s),
                Item::Icon(of, r, alpha) => {
                    let r = scaled(r, s);
                    let size = r.w as u32;
                    let px = match of {
                        IconOf::Button => self.icons.navi(size),
                        IconOf::Window(i) => {
                            let id = &app_ids[i];
                            let names = icons::app_icon_names(id, &self.entries);
                            self.icons.get(&format!("app:{id}"), &names, size)
                        }
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
                    let px = TEXT_PX * s as f32;
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
                        draw_text(&mut canvas, font, "Search", rest, px, DIM);
                    }
                }
            }
        }
        present(&mut self.pool, &self.layer, &canvas, s);
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn refresh_status(&mut self) {
        let status = status::format(&jiff::Zoned::now(), status::battery(Path::new(POWER)));
        if status != self.status {
            self.status = status;
            self.draw_band();
        }
    }

    /// The status text flush right on a dim box just big enough for it; the rest transparent.
    fn draw_band(&mut self) {
        let s = self.scale.max(1);
        let Some(mut canvas) = Pixmap::new((self.band_w * s) as u32, (BAND_H * s) as u32) else {
            return;
        };
        if let Some(font) = &self.font {
            let w = text_w(font, &self.status, TEXT_PX).ceil() as i32 + 2 * EDGE;
            let r = Rect {
                x: self.band_w - w,
                y: 0,
                w,
                h: BAND_H,
            };
            fill(&mut canvas, scaled(r, s), BG, BAND_ALPHA);
            let text = Rect {
                x: r.x + EDGE,
                w: r.w - EDGE,
                ..r
            };
            let item = Item::Text(self.status.clone(), text, INK);
            paint(&mut canvas, Some(font), item, s);
        }
        present(&mut self.pool, &self.band, &canvas, s);
    }

    fn call(&mut self, call: Call) {
        let now = self.now_ms();
        match call {
            Call::Notify(sender, r, reply) => {
                let (id, closed) = self.notes.notify(now, &sender, &r);
                let _ = reply.send(id);
                let Some(closed) = closed else {
                    return debug!(id, sender, "notification rate limited");
                };
                let lens = (r.app_name.len(), r.summary.len(), r.body.len());
                info!(id, sender, ?lens, "notification");
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
            Some(f) => (f.lookup_glyph_index(c) != 0).then(|| f.metrics(c, TEXT_PX).advance_width),
            None => Some(TEXT_PX / 2.0),
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
        for item in cards::draw(&self.cards) {
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
        Item::Text(text, r, rgb) => {
            if let Some(font) = font {
                draw_text(canvas, font, &text, scaled(r, s), TEXT_PX * s as f32, rgb);
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
    let [_, r, g, b] = rgb.to_be_bytes();
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
    let [_, cr, cg, cb] = rgb.to_be_bytes();
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
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
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.close_menu();
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
            let on_cards = self.cards_layer.as_ref().map(|l| l.wl_surface()) == Some(&e.surface);
            if on_cards {
                if let PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } = e.kind
                {
                    let (x, y) = (e.position.0 as i32, e.position.1 as i32);
                    let dismissed =
                        cards::hit(&self.cards, x, y).and_then(|id| self.notes.dismiss(id));
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
                PointerEventKind::Axis { vertical, .. } => {
                    let steps = match vertical.discrete {
                        0 => (vertical.absolute / 15.0).round() as isize,
                        d => d as isize,
                    };
                    if let Some(m) = self.menu.as_mut()
                        && steps != 0
                    {
                        m.scroll(steps);
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
