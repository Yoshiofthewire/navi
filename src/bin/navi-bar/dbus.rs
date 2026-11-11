//! The `org.freedesktop.Notifications` server on the session bus, only while no other server
//! holds the name. zbus answers on its own thread; each call is handed to navi-bar's loop.
//!
//! On real hardware only, navi-bar also makes the one call out: it tells the bus and systemd's
//! user manager the session's display variables, so portals and D-Bus-activated apps find navi.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::mpsc;
use std::time::Duration;

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use tracing::{info, warn};
use zbus::{
    blocking::Connection,
    fdo::{self, RequestNameFlags, RequestNameReply},
    interface,
    message::Header,
    zvariant::OwnedValue,
};

use crate::notify::{Closed, Request};

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// How long a `Notify` waits for navi-bar's loop before it fails.
const REPLY_WAIT: Duration = Duration::from_secs(2);

pub enum Call {
    /// Answered with the id.
    Notify(String, Request, mpsc::SyncSender<u32>),
    Close(String, u32),
    Configure(serde_json::Value),
    Settings(serde_json::Value, mpsc::SyncSender<serde_json::Value>),
}

struct Server(Sender<Call>);

fn sender(h: &Header<'_>) -> String {
    h.sender().map(|s| s.to_string()).unwrap_or_default()
}

#[interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<&str> {
        vec!["body", "actions"]
    }

    fn get_server_information(&self) -> (&str, &str, &str, &str) {
        ("navi", "navi", env!("CARGO_PKG_VERSION"), "1.2")
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        #[zbus(header)] h: Header<'_>,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> fdo::Result<u32> {
        let r = Request {
            app_name: app_name.clone(),
            replaces_id,
            summary,
            body,
            expire_timeout,
            actions: actions
                .as_chunks::<2>()
                .0
                .iter()
                .take(8)
                .map(|p| {
                    (
                        crate::notify::sanitize(&p[0], 64),
                        crate::notify::sanitize(&p[1], 64),
                    )
                })
                .collect(),
            app_id: hints
                .get("desktop-entry")
                .and_then(|v| <&str>::try_from(v).ok())
                .map(|s| crate::notify::sanitize(s, 128))
                .unwrap_or_else(|| crate::notify::sanitize(&app_name, 64)),
        };
        let (tx, rx) = mpsc::sync_channel(1);
        self.0
            .send(Call::Notify(sender(&h), r, tx))
            .map_err(|_| fdo::Error::Failed("navi-bar is closing".into()))?;
        rx.recv_timeout(REPLY_WAIT)
            .map_err(|_| fdo::Error::Failed("navi-bar did not answer".into()))
    }

    fn close_notification(&self, #[zbus(header)] h: Header<'_>, id: u32) {
        let _ = self.0.send(Call::Close(sender(&h), id));
    }
}

/// Serves the name if nobody holds it, never queueing for it or letting it be taken: then the
/// connection, else `None`.
pub fn serve(tx: Sender<Call>) -> Option<Connection> {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        info!("notifications: DBUS_SESSION_BUS_ADDRESS unset, not serving");
        return None;
    }
    let conn = Connection::session()
        .inspect_err(|e| warn!("notifications: no session bus, not serving: {e}"))
        .ok()?;
    // An empty object server first, or zbus warns; the interface is added once the name is ours.
    let _ = conn.object_server();
    match conn.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner) => {}
        // zbus turns the `Exists` reply into this error.
        Err(zbus::Error::NameTaken) => {
            info!("notifications: name owned by another server, not serving");
            return None;
        }
        Ok(r) => {
            warn!(?r, "notifications: unexpected reply, not serving");
            return None;
        }
        Err(e) => {
            warn!("notifications: name not requested, not serving: {e}");
            return None;
        }
    }
    if let Err(e) = conn.object_server().at(PATH, Server(tx)) {
        warn!("notifications: not serving: {e}");
        return None;
    }
    info!("notifications: serving");
    Some(conn)
}

/// Keep this connection separate: notification-only sandbox proxies may route calls to
/// any interface on the notification name's unique owner.
pub fn serve_settings(tx: Sender<Call>) -> Option<Connection> {
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS")?;
    let conn = Connection::session()
        .inspect_err(|e| warn!("shell settings: no session bus: {e}"))
        .ok()?;
    if let Err(e) = conn
        .object_server()
        .at("/io/github/Yoshiofthewire/Navi/Shell", SettingsServer(tx))
    {
        warn!("shell settings unavailable: {e}");
        return None;
    }
    match conn.request_name_with_flags(shell_name(), RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner) => Some(conn),
        result => {
            warn!(?result, "shell settings name unavailable");
            None
        }
    }
}

/// The variables exported, and nothing else.
const EXPORTED: [&str; 4] = [
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
];

/// navi sets `NAVI_EXPORT_ENV=1` on real hardware only; nested it's removed.
pub fn wants_export(v: Option<&OsStr>) -> bool {
    v == Some(OsStr::new("1"))
}

/// The exported variables that are set, as `get` reads them.
fn exported(get: impl Fn(&str) -> Option<String>) -> Vec<(&'static str, String)> {
    EXPORTED
        .iter()
        .filter_map(|&k| Some((k, get(k).filter(|v| !v.is_empty())?)))
        .collect()
}

/// `UpdateActivationEnvironment` on the bus and `SetEnvironment` on systemd's user manager. A
/// failure is a warning: apps started from the taskbar still get the variables.
pub fn export_env() {
    let vars = exported(|k| std::env::var(k).ok());
    let names: Vec<&str> = vars.iter().map(|(k, _)| *k).collect();
    let conn = match Connection::session() {
        Ok(c) => c,
        Err(e) => return warn!("environment not exported, no session bus: {e}"),
    };
    let map: HashMap<&str, &str> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let bus = conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "UpdateActivationEnvironment",
        &(map,),
    );
    if let Err(e) = bus {
        warn!("environment not exported to the bus: {e}");
    }
    let list: Vec<String> = vars.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let systemd = conn.call_method(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        Some("org.freedesktop.systemd1.Manager"),
        "SetEnvironment",
        &(list,),
    );
    if let Err(e) = systemd {
        warn!("environment not exported to systemd: {e}");
    }
    info!(?names, "environment exported");
}

pub fn emit(conn: &Connection, (id, reason): Closed) {
    let sent = conn.emit_signal(
        None::<&str>,
        PATH,
        NAME,
        "NotificationClosed",
        &(id, reason),
    );
    match sent {
        Ok(()) => info!(id, reason, "notification closed"),
        Err(e) => warn!(id, "NotificationClosed not sent: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real exports and proxy filtering, on a private bus without a display or GPU.
    #[test]
    #[ignore = "requires dbus-run-session, xdg-dbus-proxy and Python GI"]
    fn notification_only_proxy_isolation() {
        if std::env::var_os("NAVI_DBUS_TEST_HELPER").as_deref() == Some(OsStr::new("1")) {
            assert_eq!(std::env::var("NAVI_PRIVATE_DBUS_TEST").as_deref(), Ok("1"));
            use smithay_client_toolkit::reexports::calloop::channel;
            let (tx, rx) = channel::channel();
            let notifications = serve(tx.clone()).expect("notification export");
            assert!(
                serve(tx.clone()).is_none(),
                "do not replace another notification owner"
            );
            let _settings = serve_settings(tx).expect("settings export");
            let mut notes: Vec<(u32, String)> = Vec::new();
            // Only application state is a fixture; registration, dispatch, lock guard and
            // signal routing use the production interfaces above.
            while let Ok(call) = rx.recv() {
                match call {
                    Call::Notify(sender, _, reply) => {
                        let id = notes.len() as u32 + 1;
                        notes.push((id, sender));
                        reply.send(id).unwrap();
                    }
                    Call::Settings(value, reply) => {
                        if value["op"] == "action" {
                            let id = value["id"].as_u64().unwrap() as u32;
                            let (_, sender) = notes.iter().find(|(n, _)| *n == id).unwrap();
                            // A token here only exercises the signal routing; production
                            // Settings actions never carry one.
                            let token = value["token"].as_str();
                            action(
                                &notifications,
                                sender,
                                id,
                                value["key"].as_str().unwrap(),
                                token,
                            );
                        }
                        reply.send(serde_json::json!({
                            "ok": true,
                            "history": notes.iter().map(|(id, _)| serde_json::json!({"id": id})).collect::<Vec<_>>()
                        })).unwrap();
                    }
                    _ => {}
                }
            }
            return;
        }
        let status = std::process::Command::new("dbus-run-session")
            .args(["--", "python3"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/check-notification-isolation.py"
            ))
            .arg(std::env::current_exe().unwrap())
            .env("NAVI_PRIVATE_DBUS_TEST", "1")
            .status()
            .expect("start isolated D-Bus integration");
        assert!(status.success(), "notification-only proxy isolation failed");
    }

    #[test]
    fn ac3_the_token_goes_before_the_action() {
        assert_eq!(
            signals(7, "default", Some("tok")),
            [
                Signal::ActivationToken(7, "tok"),
                Signal::ActionInvoked(7, "default")
            ]
        );
        assert_eq!(
            signals(7, "default", None),
            [Signal::ActionInvoked(7, "default")]
        );
    }

    #[test]
    fn exports_only_session_display_names() {
        assert!(wants_export(Some(OsStr::new("1"))));
        for v in [None, Some(""), Some("0"), Some("yes")] {
            assert!(!wants_export(v.map(OsStr::new)), "{v:?}");
        }
        let env = |k: &str| match k {
            "WAYLAND_DISPLAY" => Some("wayland-2".to_string()),
            "DISPLAY" => Some(":2".to_string()),
            "XDG_CURRENT_DESKTOP" => Some("navi".into()),
            "XDG_SESSION_TYPE" => Some(String::new()),
            _ => Some("leak".into()),
        };
        assert_eq!(
            exported(env),
            [
                ("WAYLAND_DISPLAY", "wayland-2".to_string()),
                ("DISPLAY", ":2".to_string()),
                ("XDG_CURRENT_DESKTOP", "navi".into()),
            ]
        );
    }
}

pub fn shell_name() -> String {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let suffix: String = display
        .bytes()
        .take(80)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("io.github.Yoshiofthewire.Navi.Shell.d{suffix}")
}
struct SettingsServer(Sender<Call>);
#[interface(name = "io.github.Yoshiofthewire.Navi.Shell1")]
impl SettingsServer {
    fn request(&self, text: &str) -> fdo::Result<String> {
        if text.len() > 16384 {
            return Err(fdo::Error::InvalidArgs("Request too large".into()));
        }
        // Keep the compositor's independent locked-session guard for history and actions too.
        let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
        if display.is_empty() || display.contains('/') {
            return Err(fdo::Error::Failed("No Navi instance".into()));
        }
        use std::io::{BufRead, Write};
        let path =
            std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_default())
                .join(format!("navi-{display}.settings"));
        let guard = || -> Option<()> {
            let mut socket = std::os::unix::net::UnixStream::connect(path).ok()?;
            socket.set_read_timeout(Some(REPLY_WAIT)).ok()?;
            socket.set_write_timeout(Some(REPLY_WAIT)).ok()?;
            socket.write_all(b"{\"op\":\"get\"}\n").ok()?;
            let mut reply = String::new();
            use std::io::Read;
            std::io::BufReader::new(socket.take(65537))
                .read_line(&mut reply)
                .ok()?;
            (serde_json::from_str::<serde_json::Value>(&reply).ok()?["ok"] == true).then_some(())
        };
        if guard().is_none() {
            return Err(fdo::Error::AccessDenied(
                "Unlock Navi to view notifications".into(),
            ));
        }
        let value = serde_json::from_str(text)
            .map_err(|_| fdo::Error::InvalidArgs("Invalid request".into()))?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.0
            .send(Call::Settings(value, tx))
            .map_err(|_| fdo::Error::Failed("Shell closed".into()))?;
        Ok(rx
            .recv_timeout(REPLY_WAIT)
            .map_err(|_| fdo::Error::Failed("Shell did not answer".into()))?
            .to_string())
    }
}

/// A signal for an invoked action.
#[derive(Debug, PartialEq, Eq)]
pub enum Signal<'a> {
    ActivationToken(u32, &'a str),
    ActionInvoked(u32, &'a str),
}

/// What an action sends, in order: the token first, so the app can raise itself on the action.
pub fn signals<'a>(id: u32, key: &'a str, token: Option<&'a str>) -> Vec<Signal<'a>> {
    let token = token.map(|t| Signal::ActivationToken(id, t));
    token
        .into_iter()
        .chain([Signal::ActionInvoked(id, key)])
        .collect()
}

/// Sends an action's signals to the notification's sender only, never broadcast.
pub fn action(conn: &Connection, sender: &str, id: u32, key: &str, token: Option<&str>) {
    for s in signals(id, key, token) {
        let _ = match s {
            Signal::ActivationToken(id, t) => {
                conn.emit_signal(Some(sender), PATH, NAME, "ActivationToken", &(id, t))
            }
            Signal::ActionInvoked(id, k) => {
                conn.emit_signal(Some(sender), PATH, NAME, "ActionInvoked", &(id, k))
            }
        };
    }
}
