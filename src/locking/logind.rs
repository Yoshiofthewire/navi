//! Full sessions only: logind requests and the before-sleep delay inhibitor.
use super::*;
use zbus::blocking::{Connection, Proxy, connection::Builder};
use zbus::zvariant::{OwnedFd, OwnedObjectPath};

const DEST: &str = "org.freedesktop.login1";
const PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
const SESSION: &str = "org.freedesktop.login1.Session";

fn connection() -> zbus::Result<Connection> {
    Builder::system()?
        .method_timeout(Duration::from_secs(5))
        .build()
}

fn session_path(conn: &Connection) -> zbus::Result<OwnedObjectPath> {
    Proxy::new(conn, DEST, PATH, MANAGER)?.call("GetSessionByPID", &(std::process::id(),))
}

fn inhibit(proxy: &Proxy<'_>) -> zbus::Result<OwnedFd> {
    proxy.call(
        "Inhibit",
        &("sleep", "Navi", "Protect the session before sleep", "delay"),
    )
}

/// No system service or power action is invoked by nested Navi or by the test harness.
pub fn watch(state: &mut Navi) {
    let handle = &state.loop_handle;
    let (tx, rx) = channel::channel::<Option<Sender<bool>>>();
    if let Err(e) = handle.insert_source(rx, |event, _, data| {
        if let channel::Event::Msg(reply) = event {
            // Lock independently of whether the authentication UI can start.
            // A missing/crashing locker must not expose the desktop after wake.
            data.state.protect_session();
            data.state.request_lock(reply);
        }
    }) {
        warn!("logind locking listener unavailable: {e}");
        return;
    }
    let (hint_tx, hints) = std::sync::mpsc::channel::<bool>();
    state.lock.hint = Some(hint_tx);
    std::thread::spawn(move || {
        for locked in hints {
            let run = || -> zbus::Result<()> {
                let conn = connection()?;
                let path = session_path(&conn)?;
                Proxy::new(&conn, DEST, path.as_str(), SESSION)?
                    .call::<_, _, ()>("SetLockedHint", &(locked,))
            };
            if let Err(e) = run() {
                warn!("logind locked hint: {e}");
            }
        }
    });
    let sleep_tx = tx.clone();
    std::thread::spawn(move || {
        let run = || -> Result<(), Box<dyn std::error::Error>> {
            let conn = connection()?;
            let proxy = Proxy::new(&conn, DEST, PATH, MANAGER)?;
            // Subscribe before taking the inhibitor, avoiding a startup gap.
            let signals = proxy.receive_signal("PrepareForSleep")?;
            let mut inhibitor = Some(inhibit(&proxy)?);
            for signal in signals {
                if signal.body().deserialize::<bool>()? {
                    let (ready, response) = std::sync::mpsc::channel();
                    if sleep_tx.send(Some(ready)).is_err() {
                        return Ok(());
                    }
                    if response
                        .recv_timeout(Duration::from_secs(4))
                        .unwrap_or(false)
                    {
                        inhibitor.take();
                    } else {
                        // logind may enforce its own delay limit. Retain the inhibitor,
                        // keep the compositor locked, and never claim readiness.
                        warn!("sleep lock not confirmed; retaining delay inhibitor");
                    }
                } else {
                    if inhibitor.is_none() {
                        inhibitor = Some(inhibit(&proxy)?);
                    }
                }
            }
            Ok(())
        };
        if let Err(e) = run() {
            warn!("logind sleep locking listener stopped: {e}");
        }
    });
    std::thread::spawn(move || {
        let run = || -> Result<(), Box<dyn std::error::Error>> {
            let conn = connection()?;
            let path = session_path(&conn)?;
            let proxy = Proxy::new(&conn, DEST, path.as_str(), SESSION)?;
            for _ in proxy.receive_signal("Lock")? {
                if tx.send(None).is_err() {
                    break;
                }
            }
            Ok(())
        };
        if let Err(e) = run() {
            warn!("logind session lock listener stopped: {e}");
        }
    });
}
