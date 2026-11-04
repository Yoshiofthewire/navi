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
}

struct Server(Sender<Call>);

fn sender(h: &Header<'_>) -> String {
    h.sender().map(|s| s.to_string()).unwrap_or_default()
}

#[interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<&str> {
        vec!["body"]
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
        _actions: Vec<String>,
        _hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> fdo::Result<u32> {
        let r = Request {
            app_name,
            replaces_id,
            summary,
            body,
            expire_timeout,
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

/// The variables exported, and nothing else.
const EXPORTED: [&str; 3] = ["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE"];

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

    #[test]
    fn exports_only_on_request_and_only_three_names() {
        assert!(wants_export(Some(OsStr::new("1"))));
        for v in [None, Some(""), Some("0"), Some("yes")] {
            assert!(!wants_export(v.map(OsStr::new)), "{v:?}");
        }
        let env = |k: &str| match k {
            "WAYLAND_DISPLAY" => Some("wayland-2".to_string()),
            "XDG_CURRENT_DESKTOP" => Some("navi".into()),
            "XDG_SESSION_TYPE" => Some(String::new()),
            _ => Some("leak".into()),
        };
        assert_eq!(
            exported(env),
            [
                ("WAYLAND_DISPLAY", "wayland-2".to_string()),
                ("XDG_CURRENT_DESKTOP", "navi".into()),
            ]
        );
    }
}
