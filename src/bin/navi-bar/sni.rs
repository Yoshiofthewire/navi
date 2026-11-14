//! The StatusNotifierItem watcher and host on the session bus, only while no other process
//! holds the watcher name. Its own connections, never the notification server's. One worker
//! thread makes every call into items, each bounded by a method timeout, behind a bounded
//! queue: the UI thread never waits on the bus.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use tracing::{debug, info, warn};
use zbus::{
    MatchRule,
    blocking::{Connection, MessageIterator, connection::Builder, fdo::DBusProxy},
    fdo::{self, RequestNameFlags, RequestNameReply},
    interface,
    message::{Header, Type},
    names::BusName,
    object_server::SignalEmitter,
    zvariant::{DynamicTuple, OwnedValue, Structure, Value},
};

use crate::tray::{self, Coalesce, Key, Node, Prop, Props, Raw, Registry, Tree};

pub const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
/// `GetAll`'s bound, as `session.rs` bounds system calls; menus and clicks wait less.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(1);
/// Calls waiting for the worker; more are dropped.
const QUEUE: usize = 32;
/// Signals queued per subscription before older ones are dropped.
const SIGNALS: usize = 256;

/// What the UI hears.
#[derive(Debug)]
pub enum Update {
    /// A new item, last in order; its properties follow.
    Added(Key),
    Props(Key, Props),
    Removed(Key),
    /// An item's menu layout, for `Tray::menu` or a re-fetch of the open one.
    Menu(Key, Node),
}

/// A call into an item, at output-local logical px where it takes a position.
#[derive(Debug)]
pub enum Job {
    /// `ProvideXdgActivationToken` first when there is a token, then `Activate`, or
    /// `SecondaryActivate` when `secondary`.
    Activate {
        key: Key,
        token: Option<String>,
        at: (i32, i32),
        secondary: bool,
    },
    Scroll(Key, i32, &'static str),
    ContextMenu(Key, (i32, i32)),
    /// `AboutToShow(0)` then `GetLayout`, answered with `Update::Menu`.
    Menu(Key),
    /// A dbusmenu `clicked` on item `id`.
    Event(Key, i32),
    /// Look at the reads now due.
    Wake,
}

/// A re-read waiting in `Coalesce`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Read {
    Props(Key),
    Menu(Key),
}

struct Shared {
    registry: Mutex<Registry>,
    reads: Mutex<Coalesce<Read>>,
    /// Each item's menu path, once read.
    menus: Mutex<HashMap<Key, String>>,
    /// The item whose menu is open: its `LayoutUpdated` re-fetches.
    open: Mutex<Option<Key>>,
    /// The icon size wanted, in px: 20 × the band's scale.
    want: AtomicU32,
    jobs: mpsc::SyncSender<Job>,
    ui: Sender<Update>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn wake(&self) {
        let _ = self.jobs.try_send(Job::Wake);
    }

    /// Removes `gone` items: the watcher's signal, and the UI's.
    fn removed(&self, conn: &Connection, gone: Vec<tray::Entry>) {
        for e in gone {
            info!(
                sender = e.key.sender,
                path = e.key.path,
                "tray: item removed"
            );
            let mut reads = lock(&self.reads);
            reads.forget(&Read::Props(e.key.clone()));
            reads.forget(&Read::Menu(e.key.clone()));
            drop(reads);
            lock(&self.menus).remove(&e.key);
            let _ = conn.emit_signal(
                None::<&str>,
                WATCHER_PATH,
                WATCHER,
                "StatusNotifierItemUnregistered",
                &(e.listed.as_str(),),
            );
            let _ = self.ui.send(Update::Removed(e.key));
        }
    }
}

struct Watcher(Arc<Shared>);

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        service: &str,
    ) -> fdo::Result<()> {
        let sender = h.sender().map(|s| s.to_string()).unwrap_or_default();
        // Only a name's owner may register it.
        let owner = match BusName::try_from(service) {
            Ok(name) if !service.starts_with('/') => fdo::DBusProxy::new(conn)
                .await?
                .get_name_owner(name)
                .await
                .ok()
                .map(|o| o.to_string()),
            _ => None,
        };
        let added = lock(&self.0.registry).register(&sender, service, owner.as_deref());
        match added {
            Err(why) => {
                info!(sender, ?why, "tray: registration refused");
                Err(fdo::Error::AccessDenied(format!("{why:?}")))
            }
            Ok(None) => Ok(()),
            Ok(Some(e)) => {
                info!(sender, path = e.key.path, "tray: item registered");
                let _ = self.0.ui.send(Update::Added(e.key.clone()));
                lock(&self.0.reads).mark(Read::Props(e.key));
                self.0.wake();
                Self::status_notifier_item_registered(&emitter, &e.listed).await?;
                Ok(())
            }
        }
    }

    /// navi-bar is the only host it serves; another host's registration changes nothing.
    fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        lock(&self.0.registry).listed()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// The serving tray; its threads hold its connections.
pub struct Tray {
    shared: Arc<Shared>,
    dropped: Mutex<(u64, Option<Instant>)>,
}

impl Tray {
    /// Queues `job` for the worker; a full queue drops it, logged at most once a second.
    pub fn send(&self, job: Job) {
        if let Err(mpsc::TrySendError::Full(_)) = self.shared.jobs.try_send(job) {
            let mut d = lock(&self.dropped);
            d.0 += 1;
            if d.1.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
                warn!(dropped = d.0, "tray: worker queue full, call dropped");
                *d = (0, Some(Instant::now()));
            }
        }
    }

    /// The item whose menu is open, if any.
    pub fn set_open(&self, key: Option<Key>) {
        *lock(&self.shared.open) = key;
    }

    /// Icon pixmaps are picked for `px`.
    pub fn set_px(&self, px: u32) {
        self.shared.want.store(px, Relaxed);
    }
}

/// Serves the watcher and the host if nobody holds the watcher name, never queueing for it or
/// letting it be taken: then the tray, else `None`, having requested no name and exported
/// nothing.
pub fn serve(ui: Sender<Update>) -> Option<Tray> {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        info!("tray: DBUS_SESSION_BUS_ADDRESS unset, not serving");
        return None;
    }
    let conn = |timeout| {
        Builder::session()
            .and_then(|b| b.method_timeout(timeout).build())
            .inspect_err(|e| warn!("tray: no session bus, not serving: {e}"))
            .ok()
    };
    let read = conn(READ_TIMEOUT)?;
    // An empty object server first, or zbus warns; the watcher is added once the name is ours.
    let _ = read.object_server();
    match read.request_name_with_flags(WATCHER, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner) => {}
        Err(zbus::Error::NameTaken) => {
            info!("tray: watcher owned by another process, not serving");
            return None;
        }
        r => {
            warn!(?r, "tray: watcher name not requested, not serving");
            return None;
        }
    }
    let calls = conn(CALL_TIMEOUT)?;
    let (jobs, rx) = mpsc::sync_channel(QUEUE);
    let shared = Arc::new(Shared {
        registry: Mutex::default(),
        reads: Mutex::default(),
        menus: Mutex::default(),
        open: Mutex::default(),
        want: AtomicU32::new(tray::ICON as u32),
        jobs,
        ui,
    });
    if let Err(e) = read
        .object_server()
        .at(WATCHER_PATH, Watcher(shared.clone()))
    {
        warn!("tray: watcher not exported, not serving: {e}");
        return None;
    }
    let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    match read.request_name_with_flags(host.as_str(), RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner) => {}
        r => {
            warn!(?r, "tray: host name not requested, not serving");
            return None;
        }
    }
    let _ = read.emit_signal(
        None::<&str>,
        WATCHER_PATH,
        WATCHER,
        "StatusNotifierHostRegistered",
        &(),
    );
    let threads = [
        spawn("tray-owners", owners(read.clone(), shared.clone())),
        spawn("tray-signals", signals(read.clone(), shared.clone(), ITEM)),
        spawn("tray-menus", signals(read.clone(), shared.clone(), MENU)),
        spawn("tray-worker", {
            let (read, shared) = (read.clone(), shared.clone());
            move || worker(&read, &calls, &shared, rx)
        }),
    ];
    if threads.iter().any(Option::is_none) {
        return None;
    }
    info!("tray: serving");
    Some(Tray {
        shared,
        dropped: Mutex::default(),
    })
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Option<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .inspect_err(|e| warn!("tray: {name} not started: {e}"))
        .ok()
        .map(drop)
}

/// Items leave with their connection.
fn owners(conn: Connection, shared: Arc<Shared>) -> impl FnOnce() + Send {
    move || {
        let changes = match DBusProxy::new(&conn).and_then(|p| p.receive_name_owner_changed()) {
            Ok(c) => c,
            Err(e) => return warn!("tray: owner changes unwatched: {e}"),
        };
        for c in changes {
            let Ok(args) = c.args() else { continue };
            if args.new_owner.is_none() && args.name.starts_with(':') {
                let gone = lock(&shared.registry).gone(args.name.as_str());
                shared.removed(&conn, gone);
            }
        }
    }
}

/// `interface`'s signals from registered items mark them for a re-read: `New*` the item's
/// properties, a menu's `LayoutUpdated` the open menu.
fn signals(conn: Connection, shared: Arc<Shared>, interface: &'static str) -> impl FnOnce() + Send {
    move || {
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(interface)
            .map(|b| b.build());
        let it = rule.and_then(|r| MessageIterator::for_match_rule(r, &conn, Some(SIGNALS)));
        let it = match it {
            Ok(it) => it,
            Err(e) => return warn!(interface, "tray: signals unwatched: {e}"),
        };
        for m in it.flatten() {
            let h = m.header();
            let (Some(sender), Some(path), Some(member)) = (h.sender(), h.path(), h.member())
            else {
                continue;
            };
            let key = Key {
                sender: sender.to_string(),
                path: path.to_string(),
            };
            let read = match member.as_str() {
                "NewIcon" | "NewStatus" | "NewTitle" | "NewToolTip" | "NewMenu"
                    if lock(&shared.registry).contains(&key) =>
                {
                    Read::Props(key)
                }
                "LayoutUpdated" | "ItemsPropertiesUpdated" => {
                    let open = lock(&shared.open).clone();
                    let Some(item) = open.filter(|k| k.sender == key.sender) else {
                        continue;
                    };
                    if lock(&shared.menus).get(&item) != Some(&key.path) {
                        continue;
                    }
                    Read::Menu(item)
                }
                _ => continue,
            };
            lock(&shared.reads).mark(read);
            shared.wake();
        }
    }
}

/// Runs queued calls first, then one due read, then waits for either.
fn worker(read: &Connection, calls: &Connection, shared: &Shared, rx: mpsc::Receiver<Job>) {
    let mut loaded: HashSet<Key> = HashSet::new();
    loop {
        while let Ok(job) = rx.try_recv() {
            run(calls, shared, job);
        }
        let due = lock(&shared.reads).take(Instant::now());
        let job = match due {
            Ok(Read::Props(key)) => {
                get_all(read, shared, &mut loaded, key);
                continue;
            }
            Ok(Read::Menu(key)) => {
                layout(calls, shared, key, false);
                continue;
            }
            Err(Some(wait)) => rx.recv_timeout(wait),
            Err(None) => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match job {
            Ok(job) => run(calls, shared, job),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Reads `key`'s properties. An item whose first read fails is removed.
fn get_all(conn: &Connection, shared: &Shared, loaded: &mut HashSet<Key>, key: Key) {
    if !lock(&shared.registry).contains(&key) {
        loaded.remove(&key);
        return;
    }
    let reply = conn
        .call_method(
            Some(key.sender.as_str()),
            key.path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &(ITEM,),
        )
        .and_then(|m| m.body().deserialize::<HashMap<String, OwnedValue>>());
    let map = match reply {
        Ok(map) => map,
        Err(e) => {
            warn!(
                sender = key.sender,
                path = key.path,
                "tray: GetAll failed: {e}"
            );
            if !loaded.contains(&key) {
                let gone = lock(&shared.registry).remove(&key);
                shared.removed(conn, gone.into_iter().collect());
            }
            return;
        }
    };
    let props = Props::new(raw(&map, shared.want.load(Relaxed)));
    let mut menus = lock(&shared.menus);
    match &props.menu {
        Some(m) => menus.insert(key.clone(), m.clone()),
        None => menus.remove(&key),
    };
    drop(menus);
    // Gone while it was read: its removal already reached the UI.
    if lock(&shared.registry).contains(&key) {
        loaded.insert(key.clone());
        let _ = shared.ui.send(Update::Props(key, props));
    }
}

/// The properties navi-bar uses, wrong types as `None`; of IconPixmap, only the entry `pick`
/// chooses is copied.
fn raw(map: &HashMap<String, OwnedValue>, want: u32) -> Raw {
    let get = |k: &str| map.get(k).map(|v| &**v);
    let s = |k| {
        get(k)
            .and_then(|v| <&str>::try_from(v).ok())
            .map(String::from)
    };
    Raw {
        id: s("Id"),
        title: s("Title"),
        status: s("Status"),
        icon_name: s("IconName"),
        theme_path: s("IconThemePath"),
        pixmap: get("IconPixmap").and_then(|v| pixmap(v, want)),
        menu: match get("Menu") {
            Some(Value::ObjectPath(p)) => Some(p.to_string()),
            _ => None,
        },
        item_is_menu: get("ItemIsMenu").and_then(|v| bool::try_from(v).ok()),
    }
}

fn pixmap(v: &Value, want: u32) -> Option<(u32, u32, Vec<u8>)> {
    let Value::Array(entries) = v else {
        return None;
    };
    let entries: Vec<(i32, i32, &[Value])> = entries
        .iter()
        .take(16)
        .filter_map(|e| match e {
            Value::Structure(s) => match s.fields() {
                [Value::I32(w), Value::I32(h), Value::Array(b)] => Some((*w, *h, b.inner())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let sizes: Vec<_> = entries.iter().map(|(w, h, b)| (*w, *h, b.len())).collect();
    let (w, h, bytes) = entries[tray::pick(&sizes, want)?];
    let bytes = bytes.iter().map(|b| match b {
        Value::U8(b) => Some(*b),
        _ => None,
    });
    Some((w as u32, h as u32, bytes.collect::<Option<_>>()?))
}

impl Tree for Value<'_> {
    fn id(&self) -> Option<i32> {
        match fields(self)?.first()? {
            Value::I32(id) => Some(*id),
            _ => None,
        }
    }

    fn prop(&self, key: &str) -> Option<Prop<'_>> {
        let Value::Dict(d) = fields(self)?.get(1)? else {
            return None;
        };
        let (_, v) = d
            .iter()
            .find(|(k, _)| matches!(k, Value::Str(k) if k.as_str() == key))?;
        Some(match v {
            Value::Value(v) => return Some(prop(v)),
            v => prop(v),
        })
    }

    fn children(&self) -> Box<dyn Iterator<Item = &Self> + '_> {
        match fields(self).and_then(|f| f.get(2)) {
            Some(Value::Array(a)) => Box::new(a.iter().map(|c| match c {
                Value::Value(c) => &**c,
                c => c,
            })),
            _ => Box::new(std::iter::empty()),
        }
    }
}

fn prop<'a>(v: &'a Value<'_>) -> Prop<'a> {
    match v {
        Value::Str(s) => Prop::Str(s.as_str()),
        Value::Bool(b) => Prop::Bool(*b),
        Value::I32(i) => Prop::Int(*i),
        _ => Prop::Other,
    }
}

/// A layout node's `(ia{sv}av)` fields.
fn fields<'s, 'a>(v: &'s Value<'a>) -> Option<&'s [Value<'a>]> {
    match v {
        Value::Structure(s) => Some(s.fields()),
        Value::Value(v) => fields(v),
        _ => None,
    }
}

/// A call to `key`'s object at `path` (its item path if none), logged only by method on error.
macro_rules! call {
    ($conn:expr, $key:expr, $path:expr, $iface:expr, $method:expr, $body:expr) => {{
        let r = $conn.call_method(
            Some($key.sender.as_str()),
            $path,
            Some($iface),
            $method,
            $body,
        );
        if let Err(e) = &r {
            debug!(
                sender = $key.sender,
                method = $method,
                "tray: call failed: {e}"
            );
        }
        r
    }};
    ($conn:expr, $key:expr, $method:expr, $body:expr) => {
        call!($conn, $key, $key.path.as_str(), ITEM, $method, $body)
    };
}

/// Fetches `key`'s menu for the UI; `about` asks the app to prepare it first.
fn layout(conn: &Connection, shared: &Shared, key: Key, about: bool) {
    let Some(path) = lock(&shared.menus).get(&key).cloned() else {
        return;
    };
    if about {
        let _ = call!(conn, key, path.as_str(), MENU, "AboutToShow", &(0i32,));
    }
    let body = (0i32, -1i32, Vec::<&str>::new());
    let reply = call!(conn, key, path.as_str(), MENU, "GetLayout", &body);
    let node = reply.and_then(|m| {
        let body = m.body();
        let DynamicTuple((_, root)) = body.deserialize::<DynamicTuple<(u32, Structure)>>()?;
        Ok(tray::parse(&Value::Structure(root)))
    });
    match node {
        Ok(node) => {
            let _ = shared.ui.send(Update::Menu(key, node));
        }
        Err(e) => warn!(sender = key.sender, path, "tray: GetLayout failed: {e}"),
    }
}

fn run(conn: &Connection, shared: &Shared, job: Job) {
    match job {
        Job::Activate {
            key,
            token,
            at,
            secondary,
        } => {
            if let Some(t) = &token {
                // Apps without it (an UnknownMethod error) still get the click.
                let _ = call!(conn, key, "ProvideXdgActivationToken", &(t.as_str(),));
            }
            let method = if secondary {
                "SecondaryActivate"
            } else {
                "Activate"
            };
            let _ = call!(conn, key, method, &at);
            info!(
                sender = key.sender,
                method,
                token = token.is_some(),
                "tray: click"
            );
        }
        Job::Scroll(key, delta, orientation) => {
            let _ = call!(conn, key, "Scroll", &(delta, orientation));
        }
        Job::ContextMenu(key, at) => {
            let _ = call!(conn, key, "ContextMenu", &at);
        }
        Job::Menu(key) => layout(conn, shared, key, true),
        Job::Event(key, id) => {
            let Some(path) = lock(&shared.menus).get(&key).cloned() else {
                return;
            };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as u32);
            let body = (id, "clicked", Value::from(0i32), now);
            let ok = call!(conn, key, path.as_str(), MENU, "Event", &body).is_ok();
            info!(sender = key.sender, id, ok, "tray: menu event");
        }
        Job::Wake => {}
    }
}
