# Shared model also executes without GTK for headless checks. No shell evaluation.
import concurrent.futures
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import subprocess

PAGES = [
    ("Displays", "video-display-symbolic", "monitor screen scale arrangement primary resolution refresh"),
    ("Wallpaper", "preferences-desktop-wallpaper-symbolic", "background image appearance"),
    ("Wi-Fi & Network", "network-wireless-symbolic", "internet ethernet vpn wireless"),
    ("Bluetooth", "bluetooth-symbolic", "pair headphones devices wireless"),
    ("Sound", "audio-volume-high-symbolic", "volume speakers microphone audio output input"),
    ("Power & Locking", "system-lock-screen-symbolic", "battery idle inactivity lock sleep"),
    ("Mouse & Keyboard", "input-keyboard-symbolic", "touchpad pointer acceleration scroll tap repeat input"),
    ("Default Applications", "application-x-executable-symbolic", "browser email files terminal default mime"),
    ("About This PC", "computer-symbolic", "version cpu processor memory ram gpu operating system diagnostics"),
    ("Appearance", "preferences-desktop-theme-symbolic", "theme light dark color accent icons cursor font dock"),
    ("Notifications", "preferences-system-notifications-symbolic", "push unifiedpush history alerts do not disturb privacy"),
    ("System Sounds", "audio-speakers-symbolic", "event theme chime alert warning login sounds"),
    ("Accessibility", "preferences-desktop-accessibility-symbolic", "large text high contrast visual alerts keyboard accessibility"),
    ("Printers", "printer-symbolic", "printing cups printer default queue"),
    ("Accounts", "system-users-symbolic", "users account password administrator provisioning"),
    ("Date, Time & Language", "preferences-system-time-symbolic", "clock timezone ntp locale region language date time"),
]

def matching_pages(query):
    query = query.casefold().strip()
    return [i for i, (name, _, words) in enumerate(PAGES) if query in (name + " " + words).casefold()]

def request(op, **values):
    display = os.environ.get("WAYLAND_DISPLAY", "")
    if not display or "/" in display:
        raise RuntimeError("Open Navi Settings from Navi’s application launcher.")
    runtime = os.environ.get("XDG_RUNTIME_DIR", "")
    if not Path(runtime).is_absolute():
        raise RuntimeError("No desktop runtime directory is available.")
    endpoint = Path(runtime) / ("navi-" + display + ".settings")
    data = (json.dumps(dict(op=op, **values)) + "\n").encode()
    if len(data) > 16384:
        raise RuntimeError("Settings request is too large.")
    try:
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(5)
            client.connect(str(endpoint))
            client.sendall(data)
            with client.makefile("rb") as stream:
                reply = stream.readline(65537)
            if len(reply) > 65536:
                raise RuntimeError("Navi returned an oversized response.")
            value = json.loads(reply)
    except (OSError, ValueError) as error:
        raise RuntimeError("Could not reach Navi. Open Settings from its launcher.") from error
    if not value.get("ok"):
        raise RuntimeError(value.get("error", "Navi could not apply these settings."))
    return value

def about_pc():
    def text(path, limit=65536):
        try:
            with open(path) as file:
                return file.read(limit)
        except OSError:
            return ""
    release = {}
    for line in text("/etc/os-release").splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            release[key] = value.strip('"')
    cpu = next((s.split(":", 1)[1].strip() for s in text("/proc/cpuinfo").splitlines() if s.startswith("model name")), platform.machine())
    memory = next((s.split()[1] for s in text("/proc/meminfo").splitlines() if s.startswith("MemTotal:")), "0")
    disk = shutil.disk_usage(str(Path.home()))
    gpus = []
    for card in sorted(Path("/sys/class/drm").glob("card*")):
        if not card.name[4:].isdigit():
            continue
        device = card / "device"
        try:
            driver = (device / "driver").resolve(strict=True).name
        except OSError:
            driver = "Unknown driver"
        vendor, product = text(device / "vendor", 32).strip(), text(device / "device", 32).strip()
        if vendor and product:
            gpus.append(f"{driver} ({vendor}:{product})")
    return [("Graphics", ", ".join(gpus) or "Unavailable"), ("Operating system", release.get("PRETTY_NAME", "Linux")), ("Kernel", platform.release()),
            ("Processor", cpu), ("Memory", f"{int(memory) / 1048576:.1f} GiB"),
            ("Home storage", f"{disk.free / 1073741824:.1f} GiB free of {disk.total / 1073741824:.1f} GiB"),
            ("Architecture", platform.machine())]

