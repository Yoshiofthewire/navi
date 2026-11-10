//! Thunar integration. Its native operations, MIME associations and GVfs remain authoritative.
#![forbid(unsafe_code)]
#[allow(dead_code)]
#[path = "../desktop_config.rs"]
mod desktop_config;
#[allow(dead_code)]
#[path = "../files.rs"]
mod files;
#[path = "../theme.rs"]
mod theme;
#[path = "../utility_theme.rs"]
#[allow(dead_code)]
mod utility_theme;

use std::{
    os::unix::{ffi::OsStrExt, process::CommandExt},
    path::Path,
    process::Command,
    time::Duration,
};

// A visible, themed recovery path even when Thunar or its module is not installed.
const ERROR_DIALOG: &str = r#"
import gi, os, sys
gi.require_version('Gtk', '4.0')
from gi.repository import Gtk, Gdk, Gio
app = Gtk.Application(application_id='io.github.Yoshiofthewire.Navi.Files.Error', flags=Gio.ApplicationFlags.NON_UNIQUE)
def activate(app):
    provider = Gtk.CssProvider()
    provider.load_from_data(os.environ['NAVI_UTILITY_CSS'].encode())
    Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
    window = Gtk.ApplicationWindow(application=app, title='Navi Files', default_width=460)
    header = Gtk.HeaderBar(); window.set_titlebar(header)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=16, margin_start=24, margin_end=24, margin_top=24, margin_bottom=24)
    box.append(Gtk.Label(label='Files could not open', xalign=0))
    message = Gtk.Label(label=sys.argv[1], xalign=0, wrap=True, selectable=True)
    box.append(message)
    close = Gtk.Button(label='Close', halign=Gtk.Align.END)
    close.connect('clicked', lambda _: window.close()); box.append(close)
    window.set_child(box); window.present(); close.grab_focus(); message.select_region(0, 0)
app.connect('activate', activate)
app.run([sys.argv[0]])
raise SystemExit(1)
"#;

fn compatible_owner(path: &Path) -> Result<(), String> {
    let connection = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.method_timeout(Duration::from_secs(3)).build())
        .map_err(|_| "A desktop session bus is needed to open Files.".to_string())?;
    let bus = zbus::blocking::Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .map_err(|e| e.to_string())?;
    let owned: bool = bus
        .call("NameHasOwner", &("org.xfce.Thunar",))
        .map_err(|e| e.to_string())?;
    if owned {
        let pid: u32 = bus
            .call("GetConnectionUnixProcessID", &("org.xfce.Thunar",))
            .map_err(|e| e.to_string())?;
        let environ = files::read_bytes(Path::new(&format!("/proc/{pid}/environ")), 1 << 20)
            .ok()
            .flatten()
            .unwrap_or_default();
        let expected = [
            b"NAVI_UTILITY_APPEARANCE=".as_slice(),
            path.as_os_str().as_bytes(),
        ]
        .concat();
        if !environ.split(|b| *b == 0).any(|e| e == expected) {
            return Err("Thunar is already running outside this Navi Files session. Close its windows and try again. Existing file operations have been left running.".into());
        }
    }
    Ok(())
}

fn launch() -> Result<(), String> {
    let path = utility_theme::path()
        .filter(|p| p.is_file())
        .ok_or("Open Files from Navi’s launcher. If Navi just started, try again in a moment.")?;
    let module = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("libnavi-gtk3.so");
    if !module.is_file() {
        return Err(
            "Install libnavi-gtk3.so beside navi-files to enable Navi’s utility theme.".into(),
        );
    }
    compatible_owner(&path)?;
    let mut command = Command::new("thunar");
    command.arg("--window").arg("--");
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        command.arg(std::env::var_os("HOME").unwrap_or_else(|| "/".into()));
    } else {
        command.args(args);
    }
    let mut modules = std::env::var_os("GTK_MODULES").unwrap_or_default();
    if !modules.is_empty() {
        modules.push(":");
    }
    modules.push(&module);
    let error = command
        .env("GTK_MODULES", modules)
        .env("NAVI_UTILITY_APPEARANCE", &path)
        .env_remove("GTK_THEME")
        .env_remove("WAYLAND_SOCKET")
        .env_remove("NAVI_CONTROL_FD")
        .exec();
    Err(format!(
        "Thunar could not start: {error}. Install Thunar; add GVfs for trash, removable drives and network locations."
    ))
}

fn main() {
    if let Err(error) = launch() {
        eprintln!("Navi Files: {error}");
        let settings = desktop_config::load().unwrap_or_else(|_| desktop_config::defaults());
        let error = Command::new("/usr/bin/python3")
            .arg("-I")
            .arg("-c")
            .arg(ERROR_DIALOG)
            .arg(error)
            .env("NAVI_UTILITY_CSS", utility_theme::css(&settings))
            .env(
                "GSK_RENDERER",
                std::env::var("GSK_RENDERER").unwrap_or_else(|_| "cairo".into()),
            )
            .env_remove("WAYLAND_SOCKET")
            .env_remove("NAVI_CONTROL_FD")
            .exec();
        eprintln!("Navi Files needs Python 3/PyGObject/GTK 4 to display errors: {error}");
        std::process::exit(1);
    }
}
