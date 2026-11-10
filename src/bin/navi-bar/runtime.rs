//! Optional services stay outside the compositor. Only children started here are stopped here.
use crate::{dbus::Call, desktop_config, files};
use serde_json::{Value, json};
use smithay_client_toolkit::reexports::calloop::channel::Sender;
use std::{
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use zbus::blocking::{Proxy, connection::Builder};

pub struct Runtime {
    pub status: Arc<Mutex<Value>>,
    stop: mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn stop(child: &mut Option<Child>) {
    if let Some(mut child) = child.take() {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_child(&child),
            rustix::process::Signal::TERM,
        );
        for _ in 0..20 {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}
fn start_push(settings: &Value) -> Result<Child, String> {
    let conn = Builder::session()
        .map_err(|_| "Session bus unavailable")?
        .method_timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| "Session bus unavailable")?;
    let bus = Proxy::new(
        &conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .map_err(|_| "Session bus unavailable")?;
    if bus
        .call::<_, _, bool>("NameHasOwner", &("org.unifiedpush.Distributor.kde",))
        .map_err(|_| "Could not inspect UnifiedPush")?
    {
        return Err("Another UnifiedPush distributor is running; Navi leaves it unchanged".into());
    }
    let base = desktop_config::path()
        .ok_or("No configuration directory")?
        .with_file_name("unifiedpush");
    let provider = settings["push_provider"].as_str().unwrap_or("Ntfy");
    let url = settings["push_url"].as_str().unwrap_or("https://ntfy.sh");
    let provider_text = format!("[PushProvider]\nType={provider}\n\n[{provider}]\nUrl={url}\n");
    let path = base.join("KDE/kunifiedpush-distributor.conf");
    // KDE owns client tokens and provider identities in this same file. Never rewrite it.
    if files::read_bytes(&path, 1 << 20)?.is_none() {
        files::write_private(&path, &provider_text)
            .map_err(|_| "Could not save private UnifiedPush configuration")?;
    }
    // No inherited KDE provider defaults or credentials; no shell and no token in argv.
    Command::new("/usr/bin/kunifiedpush-distributor")
        .env("XDG_CONFIG_HOME", &base)
        .env("XDG_CONFIG_DIRS", base.join("empty"))
        .env("XDG_DATA_HOME", base.join("data"))
        .env("XDG_STATE_HOME", base.join("state"))
        .env("XDG_CACHE_HOME", base.join("cache"))
        .env_remove("WAYLAND_SOCKET")
        .env_remove("NAVI_CONTROL_FD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Install the optional kunifiedpush package to enable UnifiedPush".into())
}

fn configure_push(pid: u32, settings: &Value) -> Result<(), String> {
    let run = || -> zbus::Result<()> {
        let conn = Builder::session()?
            .method_timeout(Duration::from_secs(2))
            .build()?;
        let bus = Proxy::new(
            &conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )?;
        let unique: String = bus.call("GetNameOwner", &("org.unifiedpush.Distributor.kde",))?;
        let owner: u32 = bus.call("GetConnectionUnixProcessID", &(&unique,))?;
        if owner != pid {
            return Err(zbus::Error::Failure(
                "Another distributor owns the service".into(),
            ));
        }
        let proxy = Proxy::new(
            &conn,
            unique.as_str(),
            "/Management",
            "org.kde.kunifiedpush.Management",
        )?;
        let provider = settings["push_provider"].as_str().unwrap_or("Ntfy");
        let url = settings["push_url"].as_str().unwrap_or("https://ntfy.sh");
        let current: String = proxy.call("pushProviderId", &())?;
        let config: std::collections::HashMap<String, zbus::zvariant::OwnedValue> =
            proxy.call("pushProviderConfiguration", &(provider,))?;
        if current != provider
            || config.get("Url").and_then(|v| <&str>::try_from(v).ok()) != Some(url)
        {
            let values =
                std::collections::HashMap::from([("Url", zbus::zvariant::Value::from(url))]);
            proxy.call::<_, _, ()>("setPushProvider", &(provider, values))?;
        }
        Ok(())
    };
    run().map_err(|_| "Waiting for the owned distributor’s provider configuration".into())
}

/// These are user-session services, never started by a nested desktop. No replacement
/// flags, login passwords, or shell evaluation; an existing provider/app stays untouched.
fn start_secrets(full_session: bool) -> (Option<Child>, Option<Child>) {
    if !full_session {
        return (None, None);
    }
    let (mut vault, mut agent) = (None, None);
    let mut run = || -> zbus::Result<()> {
        let conn = Builder::session()?
            .method_timeout(Duration::from_secs(2))
            .build()?;
        let bus = Proxy::new(
            &conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )?;
        if !bus.call::<_, _, bool>("NameHasOwner", &("org.freedesktop.secrets",))? {
            let path = ["/usr/lib/oo7-daemon", "/usr/libexec/oo7-daemon"]
                .into_iter()
                .find(|p| std::path::Path::new(p).is_file());
            vault = path.and_then(spawn_service);
            if vault.is_none() {
                tracing::warn!("secrets: install oo7 for application and saved Wi-Fi passwords");
                return Ok(());
            }
            for _ in 0..40 {
                if bus.call::<_, _, bool>("NameHasOwner", &("org.freedesktop.secrets",))? {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        agent = if let Ok(owner) =
            bus.call::<_, _, String>("GetNameOwner", &("org.freedesktop.secrets",))
        {
            let pid: u32 = bus.call("GetConnectionUnixProcessID", &(&owner,))?;
            let oo7 = std::fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .is_some_and(|p| p.file_name() == Some(std::ffi::OsStr::new("oo7-daemon")));
            if oo7
                && !bus.call::<_, _, bool>(
                    "NameHasOwner",
                    &("org.freedesktop.network-manager-applet",),
                )?
            {
                let child = spawn_service("/usr/bin/nm-applet");
                if child.is_none() {
                    tracing::warn!(
                        "secrets: install network-manager-applet for Wi-Fi keyring retrieval"
                    );
                }
                child
            } else {
                if !oo7 {
                    tracing::warn!("secrets: another provider is running; it remains unchanged");
                }
                None
            }
        } else {
            None
        };
        Ok(())
    };
    if run().is_err() {
        tracing::warn!("secrets: session service setup unavailable");
    }
    // Retain any child even if a later D-Bus query fails, so logout still reaps it.
    (vault, agent)
}

fn spawn_service(path: &str) -> Option<Child> {
    let mut command = Command::new(path);
    if path.ends_with("/oo7-daemon") {
        use std::os::unix::fs::DirBuilderExt;
        let data = files::xdg_dir(
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
            ".local/share",
        )?
        .join("navi/secrets");
        // Starting oo7 must not discover/import an existing GNOME/KWallet data store.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&data)
            .ok()?;
        command.env("XDG_DATA_HOME", data);
    }
    command
        .env_remove("LIBNM_USE_SESSION_BUS")
        .env_remove("WAYLAND_SOCKET")
        .env_remove("NAVI_CONTROL_FD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

pub fn watch(tx: Sender<Call>, full_session: bool) -> Runtime {
    let (stop_tx, stop_rx) = mpsc::channel();
    let status = Arc::new(Mutex::new(json!({"owned":false,"state":"Disabled"})));
    let state = status.clone();
    let worker = std::thread::spawn(move || {
        let (mut vault, mut network_agent) = start_secrets(full_session);
        let mut authentication = if full_session {
            [
                "/usr/lib/polkit-kde-authentication-agent-1",
                "/usr/libexec/polkit-kde-authentication-agent-1",
                "/usr/lib/polkit-gnome/polkit-gnome-authentication-agent-1",
            ]
            .iter()
            .find(|p| std::path::Path::new(p).is_file())
            .and_then(|p| {
                Command::new(p)
                    .env_remove("WAYLAND_SOCKET")
                    .env_remove("NAVI_CONTROL_FD")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok()
            })
        } else {
            None
        };
        let mut previous = Value::Null;
        let mut child: Option<Child> = None;
        let mut push_key = Value::Null;
        let mut configured = Value::Null;
        loop {
            if let Ok(settings) = desktop_config::load() {
                if settings != previous {
                    let mut appearance = settings.clone();
                    if appearance["icon_theme"] == "" {
                        let config = files::xdg_dir(
                            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
                            std::env::var_os("HOME").as_deref(),
                            ".config",
                        );
                        appearance["icon_theme"] =
                            crate::icons::preferred(config.as_deref()).into();
                    }
                    if let Err(error) = crate::utility_theme::write(&appearance) {
                        tracing::warn!("utility appearance unavailable: {error}");
                    }
                    if tx.send(Call::Configure(settings.clone())).is_err() {
                        break;
                    }
                    previous = settings.clone();
                }
                let key = json!([
                    settings["push_enabled"],
                    settings["push_provider"],
                    settings["push_url"]
                ]);
                if key != push_key {
                    if settings["push_enabled"] != true {
                        stop(&mut child);
                    }
                    configured = Value::Null;
                    let status = if settings["push_enabled"] != true {
                        json!({"owned":false,"state":"Disabled"})
                    } else if !full_session {
                        json!({"owned":false,"state":"Unavailable in nested Navi"})
                    } else if let Some(p) = &child {
                        json!({"owned":true,"pid":p.id(),"state":"Applying provider configuration"})
                    } else {
                        match start_push(&settings) {
                            Ok(p) => {
                                let pid = p.id();
                                child = Some(p);
                                json!({"owned":true,"pid":pid,"state":"Starting"})
                            }
                            Err(e) => json!({"owned":false,"state":e}),
                        }
                    };
                    if let Ok(mut s) = state.lock() {
                        *s = status;
                    }
                    push_key = key.clone();
                }
                if let Some(p) = &child
                    && configured != key
                    && configure_push(p.id(), &settings).is_ok()
                {
                    configured = key;
                }
            }
            if child
                .as_mut()
                .is_some_and(|p| p.try_wait().ok().flatten().is_some())
            {
                child.take();
                if let Ok(mut s) = state.lock() {
                    *s = json!({"owned":false,"state":"Distributor stopped. Disable and enable UnifiedPush to retry."});
                }
            }
            if !matches!(
                stop_rx.recv_timeout(Duration::from_secs(1)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                break;
            }
        }
        stop(&mut child);
        stop(&mut authentication);
        stop(&mut network_agent);
        stop(&mut vault);
    });
    Runtime {
        status,
        stop: stop_tx,
        worker: Some(worker),
    }
}

static SOUND_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn play(settings: &Value, event: &str) {
    if settings["sounds"] != true
        || !settings["sound_events"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == event))
    {
        return;
    }
    let id = match event {
        "notification" => "message-new-instant",
        "warning" => "dialog-warning",
        "volume" => "audio-volume-change",
        "battery" => "battery-low",
        "login" => "desktop-login",
        "logout" => "desktop-logout",
        _ => return,
    };
    let theme = settings["sound_theme"]
        .as_str()
        .unwrap_or("freedesktop")
        .to_owned();
    let volume = settings["sound_volume"].as_u64().unwrap_or(40);
    if volume == 0 {
        return;
    }
    if SOUND_BUSY.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let db = 20.0 * (volume as f64 / 100.0).log10();
    let child = Command::new("canberra-gtk-play")
        .args([
            "--id",
            id,
            "--property",
            &format!("canberra.xdg-theme.name={theme}"),
            "--volume",
            &format!("{db:.2}"),
        ])
        .env_remove("WAYLAND_SOCKET")
        .env_remove("NAVI_CONTROL_FD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        std::thread::spawn(move || {
            for _ in 0..50 {
                if child.try_wait().ok().flatten().is_some() {
                    SOUND_BUSY.store(false, std::sync::atomic::Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
            SOUND_BUSY.store(false, std::sync::atomic::Ordering::Relaxed);
        });
    } else {
        SOUND_BUSY.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn nested_never_starts_secret_services() {
        let (vault, agent) = super::start_secrets(false);
        assert!(vault.is_none() && agent.is_none());
    }
}
