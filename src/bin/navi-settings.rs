//! Native GTK settings run only while opened; compositor dependencies stay unchanged.
#![forbid(unsafe_code)]
#[path = "../theme.rs"]
mod theme;

use std::{os::unix::process::CommandExt, process::Command};

fn python_command() -> Command {
    let mut command = Command::new("/usr/bin/python3");
    // Embedded sources must import only trusted system modules.
    command.arg("-I");
    command
}

fn main() {
    // ponytail: embed this small app; install modules if source approaches Linux’s 128 KiB argument limit.
    let error = python_command()
        .env("NAVI_SETTINGS_COLORS", format!("{{\"#0b0d12\":\"#{:06x}\",\"#d0d4dc\":\"#{:06x}\",\"#151922\":\"#{:06x}\",\"#303847\":\"#{:06x}\",\"#262c38\":\"#{:06x}\",\"#2ba3d8\":\"#{:06x}\"}}", theme::BG,theme::INK,theme::BAR_BG,theme::SHELL_EDGE,theme::SELECTED,theme::ACCENT))
        .arg("-c")
        .arg(concat!(
            include_str!("../settings/model.py"), "\n", include_str!("../settings/services.py"),
            "\n",
            include_str!("../settings/app.py")
        ))
        .args(std::env::args_os().skip(1))
        .exec();
    eprintln!("Navi Settings needs Python 3, PyGObject and GTK 4: {error}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    #[test]
    fn python_is_isolated_with_system_gtk_available() {
        let output = super::python_command()
            .env("PYTHONPATH", std::env::current_dir().unwrap())
            .args([
                "-c",
                "import os, sys; assert sys.flags.isolated; assert sys.flags.ignore_environment; assert sys.flags.no_user_site; assert '' not in sys.path; assert os.getcwd() not in sys.path; import gi; gi.require_version('Gtk', '4.0'); from gi.repository import Gtk",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
