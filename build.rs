use std::{env, path::PathBuf, process::Command};

fn main() {
    for path in [
        "scripts/build-locker.sh",
        "patches/swaylock-eight-slices.patch",
        "tests/lock-ring.c",
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
}
