//! The StatusNotifierItem watcher and host on the session bus, only while no other process
//! holds the watcher name. Its own connections, never the notification server's. One worker
//! thread makes every call into items, each bounded by a method timeout, behind a bounded
//! queue: the UI thread never waits on the bus.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
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
    /// A `Wake` is queued: signal floods add no more, so clicks always find room.
    woken: AtomicBool,
    ui: Sender<Update>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn wake(&self) {
        if !self.woken.swap(true, Relaxed) && self.jobs.try_send(Job::Wake).is_err() {
            self.woken.store(false, Relaxed);
        }
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
        let bus = fdo::DBusProxy::new(conn).await?;
        // Only the name owner's own process may register it.
        let owner = match BusName::try_from(service) {
            Ok(name) if !service.starts_with('/') => {
                bus.get_name_owner(name).await.ok().map(|o| o.to_string())
            }
            _ => None,
        };
        let mut pids = [None, None];
        if let Some(o) = owner.as_deref().filter(|o| *o != sender) {
            for (pid, name) in pids.iter_mut().zip([sender.as_str(), o]) {
                if let Ok(name) = BusName::try_from(name) {
                    *pid = bus.get_connection_unix_process_id(name).await.ok();
                }
            }
        }
        let me = tray::Conn {
            name: &sender,
            pid: pids[0],
        };
        let owner = owner
            .as_deref()
            .map(|name| tray::Conn { name, pid: pids[1] });
        let added = lock(&self.0.registry).register(me, service, owner);
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
        woken: AtomicBool::new(false),
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
    loop {
        while let Ok(job) = rx.try_recv() {
            run(calls, shared, job);
        }
        let due = lock(&shared.reads).take(Instant::now());
        let job = match due {
            Ok(Read::Props(key)) => {
                get_all(read, shared, key);
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

/// Reads `key`'s properties. An item whose read fails or times out is removed: a hung item
/// holds the worker for one read, never again.
fn get_all(conn: &Connection, shared: &Shared, key: Key) {
    if !lock(&shared.registry).contains(&key) {
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
            let e = brief(&e);
            warn!(
                sender = key.sender,
                path = key.path,
                e,
                "tray: GetAll failed"
            );
            let gone = lock(&shared.registry).remove(&key);
            return shared.removed(conn, gone.into_iter().collect());
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
            let e = brief(e);
            debug!(
                sender = $key.sender,
                method = $method,
                e,
                "tray: call failed"
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
        Err(e) => warn!(
            sender = key.sender,
            path,
            e = brief(&e),
            "tray: GetLayout failed"
        ),
    }
}

/// An error without the text an item wrote: a D-Bus error by its name alone.
fn brief(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, ..) => name.to_string(),
        zbus::Error::FDO(e) => zbus::DBusError::name(&**e).to_string(),
        // The rest are zbus's own texts (I/O, timeouts, signatures), never an item's message.
        e => e.to_string(),
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
        Job::Wake => shared.woken.store(false, Relaxed),
    }
}

#[cfg(test)]
pub mod tests {
    //! `tray::tests::private_bus_tray`: the production watcher and worker on a private bus,
    //! with `scripts/check-tray.py` items. No display.
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use serde_json::Value as Json;
    use smithay_client_toolkit::reexports::calloop::channel::{self, Channel};
    use zbus::blocking::{Connection, MessageIterator, Proxy, fdo::DBusProxy};
    use zbus::{MatchRule, message::Type, names::BusName};

    use super::*;
    use crate::testutil::Tmp;

    struct Ui {
        rx: Channel<Update>,
        all: Vec<Update>,
    }

    impl Ui {
        /// Waits up to `secs` for `done` to hold over every update so far.
        fn until(&mut self, secs: u64, done: impl Fn(&[Update]) -> bool) -> bool {
            let end = Instant::now() + Duration::from_secs(secs);
            loop {
                while let Ok(u) = self.rx.try_recv() {
                    self.all.push(u);
                }
                if done(&self.all) {
                    return true;
                }
                if Instant::now() > end {
                    return false;
                }
                sleep(Duration::from_millis(10));
            }
        }

        fn props(&self, key: &Key) -> Option<&Props> {
            self.all.iter().rev().find_map(|u| match u {
                Update::Props(k, p) if k == key => Some(p),
                _ => None,
            })
        }

        fn menus(&self, key: &Key) -> Vec<&Node> {
            let menus = self.all.iter().filter_map(|u| match u {
                Update::Menu(k, n) if k == key => Some(n),
                _ => None,
            });
            menus.collect()
        }

        fn removed(&self, key: &Key) -> bool {
            self.all
                .iter()
                .any(|u| matches!(u, Update::Removed(k) if k == key))
        }
    }

    /// A test item and its call log.
    struct Py {
        child: Child,
        log: PathBuf,
    }

    impl Py {
        fn new(dir: &Tmp, name: &str, args: &[&str]) -> Py {
            let log = dir.0.join(format!("{name}.jsonl"));
            let child = Command::new("python3")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/scripts/check-tray.py"
                ))
                .arg("--log")
                .arg(&log)
                .args(args)
                .stdin(Stdio::piped())
                .spawn()
                .expect("start check-tray.py");
            Py { child, log }
        }

        fn calls(&self) -> Vec<Json> {
            let text = std::fs::read_to_string(&self.log).unwrap_or_default();
            text.lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }

        /// Waits for `n` log lines that `f` picks.
        fn wait(&self, secs: u64, n: usize, f: impl Fn(&Json) -> bool) -> Vec<Json> {
            let end = Instant::now() + Duration::from_secs(secs);
            loop {
                let picked: Vec<Json> = self.calls().into_iter().filter(|c| f(c)).collect();
                if picked.len() >= n || Instant::now() > end {
                    return picked;
                }
                sleep(Duration::from_millis(20));
            }
        }

        /// Its first registration's key, once logged.
        fn key(&self, path: &str) -> Key {
            let r = self.wait(10, 1, |c| c.get("register").is_some());
            Key {
                sender: r[0]["conn"].as_str().unwrap().into(),
                path: path.into(),
            }
        }

        fn method(&self, c: &Json) -> String {
            c["call"].as_str().unwrap_or("").into()
        }

        fn cmd(&mut self, line: &str) {
            let stdin = self.child.stdin.as_mut().unwrap();
            writeln!(stdin, "{line}").unwrap();
        }

        fn exit(mut self) {
            self.cmd("exit");
            let _ = self.child.wait();
        }
    }

    fn methods(py: &Py) -> Vec<String> {
        let calls = py.calls();
        let m = calls.iter().map(|c| py.method(c)).filter(|m| !m.is_empty());
        m.filter(|m| m != "GetAll").collect()
    }

    pub fn private_bus() {
        let dir = Tmp::new("tray-bus");
        let client = Connection::session().unwrap();
        let dbus = DBusProxy::new(&client).unwrap();
        let owner = |n: &str| {
            let name = BusName::try_from(n).unwrap();
            dbus.get_name_owner(name).ok().map(|o| o.to_string())
        };
        let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());

        // AC1: someone else's watcher: no name requested, nothing exported.
        let (tx, rx) = channel::channel();
        client.request_name(WATCHER).unwrap();
        let mine = client.unique_name().unwrap().to_string();
        assert!(serve(tx.clone()).is_none());
        assert_eq!(owner(WATCHER).as_deref(), Some(mine.as_str()));
        assert_eq!(owner(&host), None);
        client.release_name(WATCHER).unwrap();

        // Watcher signals, from here on.
        let signals = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(WATCHER)
            .unwrap()
            .build();
        let it = MessageIterator::for_match_rule(rule, &client, Some(1024)).unwrap();
        let seen = signals.clone();
        std::thread::spawn(move || {
            for m in it.flatten() {
                let member = m
                    .header()
                    .member()
                    .map(|m| m.to_string())
                    .unwrap_or_default();
                let arg = m.body().deserialize::<(String,)>().map(|a| a.0);
                seen.lock().unwrap().push((member, arg.unwrap_or_default()));
            }
        });
        // `member`'s arguments once `n` arrived, or after 5 s.
        let signalled = |member: &str, n: usize| {
            let end = Instant::now() + Duration::from_secs(5);
            loop {
                let s = signals.lock().unwrap();
                let args: Vec<String> = s
                    .iter()
                    .filter(|(m, _)| m == member)
                    .map(|s| s.1.clone())
                    .collect();
                if args.len() >= n || Instant::now() > end {
                    return args;
                }
                drop(s);
                sleep(Duration::from_millis(20));
            }
        };

        let tray = serve(tx.clone()).expect("the watcher name is free");
        let ours = owner(WATCHER).unwrap();
        assert_ne!(ours, mine);
        assert_eq!(owner(&host).as_deref(), Some(ours.as_str()), "host name");
        assert!(serve(tx).is_none(), "a second serve never takes over");
        assert_eq!(owner(WATCHER).as_deref(), Some(ours.as_str()));
        let watcher: Proxy = zbus::blocking::proxy::Builder::new(&client)
            .destination(WATCHER)
            .and_then(|b| b.path(WATCHER_PATH))
            .and_then(|b| b.interface(WATCHER))
            .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No))
            .and_then(|b| b.build())
            .unwrap();
        let prop = |name: &str| watcher.get_property::<OwnedValue>(name).unwrap();
        assert_eq!(
            bool::try_from(prop("IsStatusNotifierHostRegistered")),
            Ok(true)
        );
        assert_eq!(i32::try_from(prop("ProtocolVersion")), Ok(0));
        let listed = || -> Vec<String> {
            watcher
                .get_property("RegisteredStatusNotifierItems")
                .unwrap()
        };
        let mut ui = Ui {
            rx,
            all: Vec::new(),
        };
        println!("AC1 ok: watcher and host owned by {ours}");

        // AC3: by bus name (twice: one entry) and by object path.
        let mut a = Py::new(&dir, "a", &["--twice", "--title", "Secret Title A"]);
        let ka = a.key("/StatusNotifierItem");
        let b_args = [
            "--register",
            "path",
            "--no-menu",
            "--pixmap",
            "none",
            "--icon-name",
            "nm-device-wireless",
        ];
        let b = Py::new(&dir, "b", &b_args);
        let kb = b.key("/org/navi/TestItem");
        assert!(ui.until(10, |all| has_props(all, &ka) && has_props(all, &kb)));
        let name_a = a.wait(5, 1, |c| c.get("name").is_some())[0]["name"]
            .as_str()
            .unwrap()
            .to_string();
        let want = [
            format!("{name_a}/StatusNotifierItem"),
            format!("{}/org/navi/TestItem", kb.sender),
        ];
        assert_eq!(listed(), want);
        assert_eq!(a.wait(5, 2, |c| c.get("register").is_some()).len(), 2);
        assert_eq!(signalled("StatusNotifierItemRegistered", 2), want);
        let pa = ui.props(&ka).unwrap().clone();
        assert_eq!(pa.title, "Secret Title A");
        let px = pa.pixmap.as_ref().expect("a valid pixmap");
        assert_eq!((px.w, px.h), (22, 22), "the smallest at least 20 px");
        assert_eq!(&px.rgba[..4], [255, 0, 0, 255]);
        assert_eq!(pa.menu.as_deref(), Some("/MenuBar"));
        let pb = ui.props(&kb).unwrap();
        assert_eq!(
            (pb.menu.as_deref(), pb.icon_name.as_deref()),
            (None, Some("nm-device-wireless"))
        );
        println!("AC3 ok: {want:?}");

        // AC4: a name owned by another connection, then the caps.
        let spoof = Py::new(&dir, "spoof", &["--spoof", &name_a]);
        let r = spoof.wait(10, 1, |c| c.get("register").is_some());
        assert_eq!(r[0]["ok"], false, "{r:?}");
        assert!(r[0]["error"].as_str().unwrap().contains("Spoofed"), "{r:?}");
        assert_eq!(listed().len(), 2);
        // KStatusNotifierItem's way: the name on a second connection of the same process.
        let split = Py::new(&dir, "split", &["--split", "--no-menu"]);
        let ks = split.key("/StatusNotifierItem");
        assert!(
            ui.until(10, |all| has_props(all, &ks)),
            "served by the name's owner"
        );
        assert_eq!(split.wait(5, 1, |c| c["call"] == "GetAll").len(), 1);
        assert_eq!(listed().len(), 3);
        split.exit();
        assert!(ui.until(10, |all| {
            all.iter()
                .any(|u| matches!(u, Update::Removed(k) if *k == ks))
        }));
        let fill = Py::new(&dir, "fill", &["--fill", "--no-menu"]);
        // One connection with 5 items, then 8 with 4 each.
        let r = fill.wait(30, 37, |c| c.get("register").is_some());
        assert_eq!(r.len(), 37);
        let ok = r.iter().filter(|c| c["ok"] == true).count();
        assert_eq!(ok, tray::TOTAL - 2, "the 33rd item overall is refused");
        assert!(
            r[4]["error"].as_str().unwrap().contains("SenderFull"),
            "{:?}",
            r[4]
        );
        assert!(r[36]["error"].as_str().unwrap().contains("Full"));
        assert_eq!(listed().len(), tray::TOTAL);
        println!("AC4 ok: spoof refused, {ok} filled, sender and total caps hold");
        fill.exit();
        spoof.exit();
        let back = |n| {
            move |all: &[Update]| {
                all.iter()
                    .filter(|u| matches!(u, Update::Removed(_)))
                    .count()
                    >= n
            }
        };
        assert!(ui.until(10, back(30)));
        assert_eq!(listed().len(), 2, "the fill's items left with it");

        // AC5: a registration with no object: its first GetAll fails.
        let nothing = Py::new(&dir, "nothing", &["--no-object"]);
        let kn = nothing.key("/Nothing");
        assert!(ui.until(10, |all| {
            all.iter()
                .any(|u| matches!(u, Update::Removed(k) if *k == kn))
        }));
        assert_eq!(listed().len(), 2);
        nothing.exit();

        // AC7: hostile pixmaps fall back to no pixmap; the tray goes on.
        for mode in ["huge", "negative", "mismatch", "zero", "many"] {
            let p = Py::new(&dir, mode, &["--register", "path", "--pixmap", mode]);
            let k = p.key("/org/navi/TestItem");
            assert!(ui.until(10, |all| has_props(all, &k)), "{mode}");
            assert_eq!(ui.props(&k).unwrap().pixmap, None, "{mode}");
            p.exit();
            assert!(ui.until(10, |all| {
                all.iter()
                    .any(|u| matches!(u, Update::Removed(r) if *r == k))
            }));
        }
        println!("AC7 ok: hostile pixmaps dropped");

        // AC12: the token before Activate, at the given spot; Activate alone without one.
        let activate = |key: &Key, token: Option<&str>, secondary| Job::Activate {
            key: key.clone(),
            token: token.map(String::from),
            at: (100, 20),
            secondary,
        };
        tray.send(activate(&ka, Some("tok-a"), false));
        a.wait(5, 1, |c| c["call"] == "Activate");
        assert_eq!(methods(&a), ["ProvideXdgActivationToken", "Activate"]);
        let calls = a.calls();
        let call = |m: &str| calls.iter().find(|c| c["call"] == m).unwrap()["args"].clone();
        assert_eq!(
            call("ProvideXdgActivationToken"),
            serde_json::json!(["tok-a"])
        );
        assert_eq!(call("Activate"), serde_json::json!([100, 20]));
        let f = Py::new(
            &dir,
            "f",
            &["--register", "path", "--no-menu", "--no-token-method"],
        );
        let kf = f.key("/org/navi/TestItem");
        tray.send(activate(&kf, Some("tok-f"), false));
        f.wait(5, 1, |c| c["call"] == "Activate");
        assert_eq!(
            methods(&f),
            ["ProvideXdgActivationToken", "Activate"],
            "UnknownMethod, then the click"
        );
        f.exit();
        // AC13: middle with and without a token, the wheel, and ContextMenu without a Menu.
        tray.send(activate(&kb, None, false));
        tray.send(activate(&kb, Some("tok-b"), true));
        tray.send(activate(&kb, None, true));
        tray.send(Job::Scroll(kb.clone(), 120, "vertical"));
        tray.send(Job::Scroll(kb.clone(), -120, "horizontal"));
        tray.send(Job::ContextMenu(kb.clone(), (5, 6)));
        b.wait(5, 1, |c| c["call"] == "ContextMenu");
        assert_eq!(
            methods(&b),
            [
                "Activate",
                "ProvideXdgActivationToken",
                "SecondaryActivate",
                "SecondaryActivate",
                "Scroll",
                "Scroll",
                "ContextMenu"
            ]
        );
        let calls = b.calls();
        let args: Vec<Json> = calls
            .iter()
            .filter(|c| c["call"] == "Scroll")
            .map(|c| c["args"].clone())
            .collect();
        assert_eq!(
            args,
            [
                serde_json::json!([120, "vertical"]),
                serde_json::json!([-120, "horizontal"])
            ]
        );
        println!("AC12/AC13 ok: {:?}", methods(&b));

        // AC14: AboutToShow, then GetLayout; the chosen item's Event.
        tray.send(Job::Menu(ka.clone()));
        assert!(ui.until(5, |all| {
            all.iter()
                .any(|u| matches!(u, Update::Menu(k, _) if *k == ka))
        }));
        let m = ui.menus(&ka)[0].clone();
        let labels: Vec<&str> = m.children.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels[0], "Open Secret Title A label");
        assert_eq!(m.children[4].mark, Some('✓'));
        assert_eq!(
            m.children[6].children[0].label,
            "Secret Title A label inner"
        );
        assert_eq!(
            (m.children[7].label.as_str(), m.children[7].enabled),
            ("", true),
            "wrong types ignored"
        );
        tray.send(Job::Event(ka.clone(), 9));
        let ev = a.wait(5, 1, |c| c["call"] == "Event");
        assert_eq!(
            ev[0]["args"].as_array().unwrap()[..3],
            [serde_json::json!(9), "clicked".into(), 0.into()]
        );
        let menu_calls: Vec<String> = methods(&a).into_iter().skip(2).collect();
        assert_eq!(menu_calls, ["AboutToShow", "GetLayout", "Event"]);
        let layout = a
            .calls()
            .into_iter()
            .find(|c| c["call"] == "GetLayout")
            .unwrap();
        assert_eq!(layout["args"], serde_json::json!([0, -1, []]));
        println!("AC14 ok: {menu_calls:?}");

        // AC15: LayoutUpdated while open re-fetches, coalesced.
        tray.set_open(Some(ka.clone()));
        a.cmd("layout 100");
        a.wait(5, 1, |c| c["done"] == "layout");
        assert!(ui.until(5, |all| {
            all.iter()
                .filter(|u| matches!(u, Update::Menu(k, _) if *k == ka))
                .count()
                >= 2
        }));
        sleep(Duration::from_millis(600));
        let fetches = a
            .calls()
            .iter()
            .filter(|c| c["call"] == "GetLayout")
            .count();
        assert!(
            (2..=3).contains(&fetches),
            "{fetches} GetLayout for 100 LayoutUpdated"
        );
        tray.set_open(None);
        // Hostile menus: wide and deep are capped, a hung one opens nothing.
        let wide = Py::new(&dir, "wide", &["--register", "path", "--menu", "wide"]);
        let deep = Py::new(&dir, "deep", &["--register", "path", "--menu", "deep"]);
        let hang = Py::new(&dir, "hang", &["--register", "path", "--menu", "hang"]);
        let (kw, kd, kh) = (
            wide.key("/org/navi/TestItem"),
            deep.key("/org/navi/TestItem"),
            hang.key("/org/navi/TestItem"),
        );
        assert!(ui.until(10, |all| [&kw, &kd, &kh].iter().all(|k| has_props(all, k))));
        for k in [&kw, &kd, &kh] {
            tray.send(Job::Menu(k.clone()));
        }
        assert!(ui.until(10, |all| {
            all.iter()
                .any(|u| matches!(u, Update::Menu(k, _) if *k == kw))
        }));
        assert_eq!(ui.menus(&kw)[0].children.len(), tray::MENU_WIDTH);
        assert_eq!(deep.wait(5, 1, |c| c["call"] == "GetLayout").len(), 1);
        let deep_sent = deep.wait(5, 1, |c| c.get("depth").is_some())[0]["depth"].clone();
        // GLib may refuse to send 100 levels; either way nothing over the cap arrives.
        ui.until(3, |_| false);
        if let Some(d) = ui.menus(&kd).first() {
            fn depth(n: &Node) -> usize {
                n.children.first().map_or(0, |c| 1 + depth(c))
            }
            assert!(depth(d) <= tray::MENU_DEPTH);
        }
        assert!(
            ui.menus(&kh).is_empty(),
            "a GetLayout that times out opens nothing"
        );
        println!(
            "AC15 ok: wide capped, deep sent={deep_sent} menus={}, hang opened nothing",
            ui.menus(&kd).len()
        );
        for p in [wide, deep, hang] {
            p.exit();
        }

        // AC10: a NewIcon flood: few reads, and clicks still go through.
        let before = a.calls().iter().filter(|c| c["call"] == "GetAll").count();
        a.cmd("flood 10000");
        a.wait(5, 1, |c| c.get("flood").is_some());
        let sent = Instant::now();
        tray.send(activate(&kb, None, false));
        let clicks = b.wait(2, 2, |c| c["call"] == "Activate").len();
        let click_ms = sent.elapsed().as_millis();
        assert!(clicks >= 2, "a click during the flood");
        a.wait(10, 1, |c| c.get("flooded").is_some());
        let flood = a.calls();
        let start = flood.iter().find(|c| c.get("flood").is_some()).unwrap()["t"]
            .as_f64()
            .unwrap();
        let end = flood.iter().find(|c| c.get("flooded").is_some()).unwrap()["t"]
            .as_f64()
            .unwrap();
        sleep(Duration::from_millis(600));
        let reads = a.calls().iter().filter(|c| c["call"] == "GetAll").count() - before;
        println!(
            "AC10: 10000 NewIcon in {:.2} s gave {reads} GetAll; a click took {click_ms} ms",
            end - start
        );
        assert!(end - start <= 1.1, "the flood took {} s", end - start);
        assert!(reads <= 6, "{reads} GetAll");
        assert!(click_ms < 1000);

        // A hung item flooding signals holds the worker for one read, then is dropped; other
        // items' clicks wait for that read and none is lost.
        let h = Py::new(&dir, "hung", &["--register", "path", "--no-menu"]);
        let kh = h.key("/org/navi/TestItem");
        assert!(ui.until(10, |all| has_props(all, &kh)));
        let mut h = h;
        h.cmd("hang");
        h.cmd("flood 2000");
        h.wait(5, 2, |c| c["call"] == "GetAll");
        let before = b.calls().iter().filter(|c| c["call"] == "Activate").count();
        // Fewer than the queue holds: the flood takes at most one slot.
        for _ in 0..20 {
            tray.send(activate(&kb, None, false));
        }
        let clicks = b.wait(10, before + 20, |c| c["call"] == "Activate").len() - before;
        assert_eq!(clicks, 20, "every click during the hang arrives");
        assert!(ui.until(10, |all| {
            all.iter()
                .any(|u| matches!(u, Update::Removed(k) if *k == kh))
        }));
        assert_eq!(
            h.calls().iter().filter(|c| c["call"] == "GetAll").count(),
            2
        );
        println!("hung item: dropped after one timed-out read, {clicks} clicks kept");
        h.exit();

        // AC8: an item's error text never reaches navi-bar's log, only the error's name.
        let f = Py::new(
            &dir,
            "fail",
            &[
                "--register",
                "path",
                "--no-menu",
                "--fail-calls",
                "--title",
                "Secret Error",
            ],
        );
        let kf = f.key("/org/navi/TestItem");
        let r = Connection::session().unwrap().call_method(
            Some(kf.sender.as_str()),
            "/org/navi/TestItem",
            Some(ITEM),
            "Activate",
            &(1i32, 2i32),
        );
        let e = r.unwrap_err();
        assert!(e.to_string().contains("Secret Error"), "{e}");
        assert_eq!(brief(&e), "org.navi.Test.Failed");
        f.exit();

        // AC5: the owner leaves.
        let gone = want[0].clone();
        a.exit();
        assert!(ui.until(10, |all| {
            all.iter()
                .any(|u| matches!(u, Update::Removed(k) if *k == ka))
        }));
        assert!(ui.removed(&ka));
        assert_eq!(listed(), [want[1].clone()]);
        let unregistered = signalled("StatusNotifierItemUnregistered", 37);
        assert!(unregistered.contains(&gone), "{unregistered:?}");
        println!("AC5 ok: {gone} unregistered");
        b.exit();
    }

    #[test]
    fn ac8_errors_are_logged_by_name_only() {
        let e = zbus::Error::FDO(Box::new(fdo::Error::Failed("Secret Title".into())));
        assert_eq!(brief(&e), "org.freedesktop.DBus.Error.Failed");
    }

    fn has_props(all: &[Update], key: &Key) -> bool {
        all.iter()
            .any(|u| matches!(u, Update::Props(k, _) if k == key))
    }
}
