#!/usr/bin/env python3
"""Verify the installed UnifiedPush distributor without public network or host changes.

Run: python3 scripts/check-unifiedpush.py
Needs dbus-run-session, kunifiedpush-distributor, and Python PyGObject (Gio).
Uses a private session bus, private XDG roots, a loopback Ntfy SSE fixture, and an
in-process connector. No actual user application, provider, device or service is changed.
Exit 77 means an optional prerequisite is unavailable. --output retains evidence.
This checks the external distributor's protocol/persistence, not Navi's runtime supervisor.
"""
import argparse
import base64
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time


SERVICE = "org.unifiedpush.Distributor.kde"
APP = "org.navi.UnifiedPushCheck"
TOKEN = "private-test-registration"
PAYLOAD = b"\x00\x01opaque-encrypted-test\xff"


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def private_session(root):
    # Refuse accidentally running this internal mode against the user's session bus.
    check(os.environ.get("NAVI_UNIFIEDPUSH_CHECK_ROOT") == str(root),
          "Internal mode requires the script's private D-Bus session")
    from gi.repository import Gio, GLib

    config = root / "child-config/KDE"
    config.mkdir(parents=True, mode=0o700)
    for name in ("child-data", "child-state", "child-cache"):
        (root / name).mkdir(mode=0o700)
    events, requests, callback_errors = [], [], []
    event_lock = threading.Lock()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            # Ntfy's stream path is /[optional-prefix/]topic/sse?up=1&since=...
            topic = self.path.split("?", 1)[0].split("/")[-2]
            with event_lock:
                requests.append({"topic": topic, "request": self.path})
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            try:
                self.wfile.write(b"event: open\ndata: {}\n\n")
                self.wfile.flush()
                time.sleep(0.05)
                message = {"topic": topic, "message": base64.b64encode(PAYLOAD).decode(),
                           "encoding": "base64", "id": "fixture-replay-cursor"}
                self.wfile.write(("data: " + json.dumps(message) + "\n\n").encode())
                self.wfile.flush()
                time.sleep(0.5)
            except (BrokenPipeError, ConnectionResetError):
                pass  # Provider reconfiguration intentionally closes an old stream.

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    url = "http://127.0.0.1:" + str(server.server_port)
    configuration = config / "kunifiedpush-distributor.conf"
    configuration.write_text("[PushProvider]\nType=Ntfy\n[Ntfy]\nUrl=" + url + "\n")
    configuration.chmod(0o600)
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(destination, path, interface, method, args, signature):
        return bus.call_sync(destination, path, interface, method, args,
                             GLib.VariantType.new(signature),
                             Gio.DBusCallFlags.NO_AUTO_START, 5000, None).unpack()

    def bus_call(method, args, signature):
        return call("org.freedesktop.DBus", "/org/freedesktop/DBus",
                    "org.freedesktop.DBus", method, args, signature)

    def management(method, args=None, signature="(s)"):
        return call(SERVICE, "/Management", "org.kde.kunifiedpush.Management",
                    method, args, signature)

    check(bus_call("RequestName", GLib.Variant("(su)", (APP, 4)), "(u)") == (1,),
          "Could not own the isolated connector name")
    methods = "".join('<method name="' + name + '"><arg type="a{sv}" direction="in"/>'
                      '<arg type="a{sv}" direction="out"/></method>'
                      for name in ("NewEndpoint", "Message", "Unregistered"))
    info = Gio.DBusNodeInfo.new_for_xml('<node><interface name="org.unifiedpush.Connector2">'
                                       + methods + "</interface></node>")

    def connector(connection, sender, path, interface, method, parameters, invocation):
        data = parameters.unpack()[0]
        try:
            check(data["token"] == TOKEN, "Unexpected connector token")
            if method == "Message":
                check(bytes(data["message"]) == PAYLOAD, "Push payload bytes changed")
            with event_lock:
                events.append({"method": method, "sender": sender,
                               "endpoint": data.get("endpoint"), "id": data.get("id"),
                               "payload_matches": method == "Message"})
            response = {"id": GLib.Variant("s", data["id"])} if "id" in data else {}
            invocation.return_value(GLib.Variant("(a{sv})", (response,)))
        except Exception as error:
            callback_errors.append(str(error))
            invocation.return_dbus_error("org.navi.CheckFailed", str(error))

    object_id = bus.register_object("/org/unifiedpush/Connector", info.interfaces[0],
                                    connector, None, None)
    loop = GLib.MainLoop()
    loop_thread = threading.Thread(target=loop.run, daemon=True)
    loop_thread.start()
    child_env = dict(os.environ, XDG_CONFIG_HOME=str(root / "child-config"),
                     XDG_CONFIG_DIRS=str(root / "empty"), XDG_DATA_HOME=str(root / "child-data"),
                     XDG_DATA_DIRS=str(root / "empty"), XDG_STATE_HOME=str(root / "child-state"),
                     XDG_CACHE_HOME=str(root / "child-cache"),
                     DBUS_SYSTEM_BUS_ADDRESS="unix:path=/nonexistent")
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "NAVI_CONTROL_FD"):
        child_env.pop(key, None)
    child = None
    log = (root / "daemon.log").open("w")

    def wait_for(predicate, message):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            check(not callback_errors, "; ".join(callback_errors))
            if predicate():
                return
            time.sleep(0.02)
        raise RuntimeError(message)

    def has_owner():
        return bus_call("NameHasOwner", GLib.Variant("(s)", (SERVICE,)), "(b)")[0]

    def start():
        nonlocal child
        check(not has_owner(), "Distributor already running on test bus")
        child = subprocess.Popen([shutil.which("kunifiedpush-distributor")],
                                 env=child_env, stdin=subprocess.DEVNULL,
                                 stdout=log, stderr=log)
        wait_for(lambda: child.poll() is None and has_owner(), "Distributor failed to start")

    def stop():
        nonlocal child
        if child is not None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
            child = None
        wait_for(lambda: not has_owner(), "Owned distributor did not release test bus name")

    def register():
        values = {"service": GLib.Variant("s", APP), "token": GLib.Variant("s", TOKEN),
                  "description": GLib.Variant("s", "Isolated receive verification")}
        result = call(SERVICE, "/org/unifiedpush/Distributor", "org.unifiedpush.Distributor2",
                      "Register", GLib.Variant("(a{sv})", (values,)), "(a{sv})")
        check(result == ({"success": "REGISTRATION_SUCCEEDED"},), "Registration failed")

    def endpoint_matches(prefix):
        with event_lock:
            return any(e["method"] == "NewEndpoint" and e["endpoint"].startswith(prefix)
                       for e in events)

    try:
        start()
        check(management("status", signature="(i)") == (1,), "Expected idle distributor")
        register()
        wait_for(lambda: endpoint_matches(url + "/") and
                 any(e["method"] == "Message" for e in events), "Missing real connector callbacks")
        first_endpoint = next(e["endpoint"] for e in events if e["method"] == "NewEndpoint")
        stop()
        saved = configuration.read_text()
        check("Tokens=" in saved and "[Ntfy-internal]" in saved and "Endpoint=" in saved,
              "Expected registration/provider state in QSettings file")
        start()
        clients = management("registeredClients", signature="(a(sss))")[0]
        check(len(clients) == 1 and clients[0][0] == TOKEN, "Registration lost on restart")
        register()
        wait_for(lambda: sum(e["method"] == "Message" for e in events) >= 2 and
                 sum(e["method"] == "NewEndpoint" for e in events) >= 2,
                 "Missing post-restart callbacks")
        check(next(e["endpoint"] for e in reversed(events) if e["method"] == "NewEndpoint")
              == first_endpoint, "Endpoint changed after restart")
        check(len({r["topic"] for r in requests}) == 1, "Topic changed after restart")
        # Exercise native provider changes without editing KDE's durable client records.
        new_url = url + "/changed"
        management("setPushProvider", GLib.Variant("(sa{sv})",
                   ("Ntfy", {"Url": GLib.Variant("s", new_url)})), "()")
        wait_for(lambda: endpoint_matches(new_url + "/"), "Provider change did not rotate endpoint")
        wait_for(lambda: len({r["topic"] for r in requests}) >= 2,
                 "Provider change did not replace topic")
        check(management("registeredClients", signature="(a(sss))")[0][0][0] == TOKEN,
              "Provider change lost application registration")
        unregister = GLib.Variant("(a{sv})", ({"token": GLib.Variant("s", TOKEN)},))
        call(SERVICE, "/org/unifiedpush/Distributor", "org.unifiedpush.Distributor2",
             "Unregister", unregister, "(a{sv})")
        wait_for(lambda: management("registeredClients", signature="(a(sss))") == ([],),
                 "Unregister did not remove client")
        (root / "results.json").write_text(json.dumps({"events": events, "sse_requests": requests,
             "restart_kept_endpoint": True, "provider_change_kept_registration": True,
             "unregister_succeeded": True}, indent=2))
        print("PASS: real SSE receive, opaque bytes, endpoint callback, persistent restart, "
              "provider update and unregister; private bus and loopback only")
    finally:
        stop()
        loop.quit()
        loop_thread.join(timeout=5)
        bus.unregister_object(object_id)
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=5)
        log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="retain evidence in a new directory")
    parser.add_argument("--private-session", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.private_session:
        private_session(args.private_session)
        return 0
    if not all(shutil.which(name) for name in ("dbus-run-session", "kunifiedpush-distributor")):
        print("SKIP: optional dbus-run-session or kunifiedpush-distributor unavailable")
        return 77
    try:
        from gi.repository import Gio, GLib  # noqa: F401
    except ImportError:
        print("SKIP: Python PyGObject/Gio unavailable")
        return 77
    with tempfile.TemporaryDirectory(prefix="navi-unifiedpush-check-") as temporary:
        root = args.output.resolve() if args.output else Path(temporary)
        if args.output:
            root.mkdir(mode=0o700, parents=True, exist_ok=False)
        services = root / "bus-data/dbus-1/services"
        services.mkdir(parents=True)
        (services / (APP + ".service")).write_text(
            "[D-BUS Service]\nName=" + APP + "\nExec=/usr/bin/false\n")
        (root / "empty").mkdir()
        env = dict(os.environ, XDG_DATA_HOME=str(root / "bus-data"),
                   XDG_DATA_DIRS=str(root / "empty"), XDG_CONFIG_HOME=str(root / "bus-config"),
                   XDG_CONFIG_DIRS=str(root / "empty"), XDG_CACHE_HOME=str(root / "bus-cache"),
                   NAVI_UNIFIEDPUSH_CHECK_ROOT=str(root))
        result = subprocess.run(["dbus-run-session", "--", sys.executable,
                                 str(Path(__file__).resolve()), "--private-session", str(root)],
                                env=env, timeout=45)
        if args.output:
            print("Evidence:", root)
        return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
