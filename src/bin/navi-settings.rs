//! Native GTK settings run only while opened; compositor dependencies stay unchanged.
#![forbid(unsafe_code)]
#[path = "../theme.rs"]
mod theme;

use std::{os::unix::process::CommandExt, process::Command};

fn main() {
    // ponytail: embed this small app; install modules if source approaches Linux’s 128 KiB argument limit.
    let error = Command::new("/usr/bin/python3")
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
