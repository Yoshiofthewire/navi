#!/usr/bin/env python3
"""Check real shell/notification exports through a notification-only xdg-dbus-proxy.

The ignored Rust test supplies a headless server on a private session bus. The same
check is also used against the real nested desktop by check-shell-settings.py.
"""
import json
import os
import socket
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time

from gi.repository import Gio, GLib

NOTIFICATIONS = 'org.freedesktop.Notifications'
SHELL_PATH = '/io/github/Yoshiofthewire/Navi/Shell'
SHELL_INTERFACE = 'io.github.Yoshiofthewire.Navi.Shell1'
TEST = 'dbus::tests::notification_only_proxy_isolation'


def request(bus, destination, **value):
    return json.loads(bus.call_sync(
        destination, SHELL_PATH, SHELL_INTERFACE, 'Request',
        GLib.Variant('(s)', (json.dumps(value),)), None,
        Gio.DBusCallFlags.NO_AUTO_START, 5000, None).unpack()[0])


def notify(bus, title):
    return bus.call_sync(
        NOTIFICATIONS, '/org/freedesktop/Notifications', NOTIFICATIONS, 'Notify',
        GLib.Variant('(susssasa{sv}i)', ('Isolation check', 0, '', title, '',
            ['default', 'Open'], {}, 10000)), None,
        Gio.DBusCallFlags.NO_AUTO_START, 5000, None).unpack()[0]


def check(bus, shell):
    def owner(name):
        return bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus',
            'org.freedesktop.DBus', 'GetNameOwner', GLib.Variant('(s)', (name,)),
            None, Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]

    private_id = notify(bus, 'Other application private history')
    assert any(n['id'] == private_id for n in request(bus, shell, op='get')['history'])
    notification_owner, shell_owner = owner(NOTIFICATIONS), owner(shell)
    with tempfile.TemporaryDirectory(prefix='navi-notification-proxy-') as root:
        path = Path(root) / 'bus'
        lifetime, proxy_lifetime = socket.socketpair()
        lifetime.settimeout(5)
        try:
            proxy = subprocess.Popen(['xdg-dbus-proxy', f'--fd={proxy_lifetime.fileno()}',
                os.environ['DBUS_SESSION_BUS_ADDRESS'], str(path), '--filter',
                '--talk=' + NOTIFICATIONS], pass_fds=(proxy_lifetime.fileno(),))
        finally:
            proxy_lifetime.close()
        sandbox = None
        try:
            assert lifetime.recv(1), 'Proxy exited before listening'
            # bus_get_sync caches connections: explicitly connect to the proxy instead.
            sandbox = Gio.DBusConnection.new_for_address_sync('unix:path=' + str(path),
                Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT |
                Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
            sandbox_id = notify(sandbox, 'Allowed sandbox notification')
            assert any(n['id'] == sandbox_id for n in request(bus, shell, op='get')['history'])
            for destination in dict.fromkeys([NOTIFICATIONS, notification_owner, shell, shell_owner]):
                for value in [dict(op='get'), dict(op='action', id=private_id, key='default')]:
                    try:
                        request(sandbox, destination, **value)
                    except GLib.Error as error:
                        remote = Gio.DBusError.get_remote_error(error)
                        assert remote in {
                            'org.freedesktop.DBus.Error.AccessDenied',
                            'org.freedesktop.DBus.Error.ServiceUnknown',
                            'org.freedesktop.DBus.Error.NameHasNoOwner',
                            'org.freedesktop.DBus.Error.UnknownObject',
                            'org.freedesktop.DBus.Error.UnknownMethod',
                            'org.freedesktop.DBus.Error.UnknownInterface',
                        }, (destination, value['op'], remote, str(error))
                    else:
                        raise AssertionError(f'Notification-only client reached shell {value["op"]} via {destination}')
            assert notification_owner != shell_owner, 'Settings must have a separate bus owner'
            # The notifying client (`bus`) gets the token, then the action; another client
            # on the bus, subscribed to the same signals, gets neither.
            other = Gio.DBusConnection.new_for_address_sync(
                os.environ['DBUS_SESSION_BUS_ADDRESS'],
                Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT |
                Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
            received, overheard = [], []
            def listen(connection, into):
                return [connection.signal_subscribe(notification_owner, NOTIFICATIONS, name,
                    '/org/freedesktop/Notifications', None, Gio.DBusSignalFlags.NONE,
                    lambda *args: into.append((args[-3], args[-2].unpack())), None)
                    for name in ('ActivationToken', 'ActionInvoked')]
            subscriptions = [(bus, s) for s in listen(bus, received)]
            subscriptions += [(other, s) for s in listen(other, overheard)]
            def wait_for(want):
                end = time.monotonic() + 3
                while len(received) < len(want) and time.monotonic() < end:
                    GLib.MainContext.default().iteration(False)
                    time.sleep(.01)
                # Let anything misrouted arrive too.
                end = time.monotonic() + .3
                while time.monotonic() < end:
                    GLib.MainContext.default().iteration(False)
                    time.sleep(.01)
                assert received == want, (received, want)
                assert overheard == [], overheard
                received.clear()
            try:
                assert request(bus, shell, op='action', id=private_id, key='default')['ok']
                wait_for([('ActionInvoked', (private_id, 'default'))])
                assert request(bus, shell, op='action', id=private_id, key='default',
                               token='isolation-token')['ok']
                wait_for([('ActivationToken', (private_id, 'isolation-token')),
                          ('ActionInvoked', (private_id, 'default'))])
            finally:
                for connection, subscription in subscriptions:
                    connection.signal_unsubscribe(subscription)
                other.close_sync(None)
        finally:
            if sandbox:
                sandbox.close_sync(None)
            lifetime.close()
            try:
                proxy.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proxy.kill()
                proxy.wait()
    print('PASS: notification-only proxy rejects shell history/actions; notifications and trusted Settings work; '
          'ActivationToken precedes ActionInvoked, sender only')


def main():
    assert os.environ.get('NAVI_PRIVATE_DBUS_TEST') == '1', 'Run the ignored Rust integration test'
    with tempfile.TemporaryDirectory(prefix='navi-notification-test-') as root:
        env = dict(os.environ, XDG_RUNTIME_DIR=root, WAYLAND_DISPLAY='wayland-isolation',
                   NAVI_DBUS_TEST_HELPER='1')
        shell = 'io.github.Yoshiofthewire.Navi.Shell.d' + env['WAYLAND_DISPLAY'].encode().hex()
        bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        # Only the headless test uses this stand-in for the compositor lock guard.
        locked = threading.Event()
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(Path(root) / 'navi-wayland-isolation.settings'))
        listener.listen()
        def guard():
            while True:
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(5)
                    assert connection.makefile('rb').readline(16384) == b'{"op":"get"}\n'
                    connection.sendall(json.dumps({'ok': not locked.is_set()}).encode() + b'\n')
        threading.Thread(target=guard, daemon=True).start()
        helper = subprocess.Popen([sys.argv[1], '--exact', TEST, '--ignored', '--nocapture'], env=env)
        try:
            end = time.monotonic() + 10
            while True:
                try:
                    request(bus, shell, op='get')
                    break
                except GLib.Error:
                    assert helper.poll() is None, 'Headless shell exited'
                    assert time.monotonic() < end, 'Headless shell did not register'
                    time.sleep(.02)
            check(bus, shell)
            locked.set()
            for value in [dict(op='get'), dict(op='action', id=1, key='default')]:
                try:
                    request(bus, shell, **value)
                except GLib.Error as error:
                    assert Gio.DBusError.get_remote_error(error) == 'org.freedesktop.DBus.Error.AccessDenied'
                else:
                    raise AssertionError('Locked compositor guard was bypassed')
            locked.clear()
            assert request(bus, shell, op='get')['ok']
            print('PASS: compositor lock guard retained')
        finally:
            helper.terminate()
            helper.wait(timeout=5)


if __name__ == '__main__':
    main()
