use std::{env, path::PathBuf, process::Command};

fn main() {
    for path in [
        "scripts/build-locker.sh",
        "patches/swaylock-eight-slices.patch",
        "tests/lock-ring.c",
        "src/utility-gtk3.c",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let profile = out.ancestors().nth(3).unwrap();
    let cache = profile.parent().unwrap().join("navi-locker");
    let status = Command::new("bash")
        .arg("scripts/build-locker.sh")
        .arg(profile)
        .arg(cache)
        .status()
        .expect("start native swaylock build");
    assert!(
        status.success(),
        "native locker build failed; see README build dependencies"
    );
    let flags = Command::new("pkg-config")
        .args(["--cflags", "--libs", "gtk+-3.0"])
        .output()
        .expect("pkg-config for GTK 3 utility module");
    assert!(
        flags.status.success(),
        "install GTK 3 development headers (libgtk-3-dev)"
    );
    let status = Command::new("cc")
        .args([
            "-shared",
            "-fPIC",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "src/utility-gtk3.c",
            "-o",
        ])
        .arg(profile.join("libnavi-gtk3.so"))
        .args(String::from_utf8(flags.stdout).unwrap().split_whitespace())
        .status()
        .expect("build GTK utility module");
    assert!(status.success(), "GTK utility module build failed");
}
