"""Checks for search, native panel detection and the actual bounded socket exchange."""
import importlib.util
import json
import os
from pathlib import Path
import socket
import tempfile
import threading
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('settings_model', Path(__file__).with_name('model.py'))
model = importlib.util.module_from_spec(spec)
spec.loader.exec_module(model)
exec(Path(__file__).with_name('services.py').read_text(), model.__dict__)

class SettingsTests(unittest.TestCase):
    def test_search_finds_tasks_and_empty_result(self):
        self.assertEqual(model.matching_pages('microphone'), [4])
        self.assertEqual(model.matching_pages('  BLUETOOTH '), [3])
        self.assertEqual(model.matching_pages('unfindable'), [])
        self.assertEqual(len(model.matching_pages('')), len(model.PAGES))

    def test_socket_request_and_service_errors(self):
        with tempfile.TemporaryDirectory() as directory:
            path = str(Path(directory) / 'navi-wayland-test.settings')
            listener = socket.socket(socket.AF_UNIX)
            listener.bind(path)
            listener.listen(1)
            observed = []
            def server():
                for result in [{'ok': True, 'nested': True}, {'ok': False, 'error': 'Unlock Navi'}]:
                    client, _ = listener.accept()
                    with client, client.makefile('rb') as stream:
                        observed.append(json.loads(stream.readline()))
                        client.sendall((json.dumps(result) + '\n').encode())
            thread = threading.Thread(target=server)
            thread.start()
            with patch.dict(os.environ, {'WAYLAND_DISPLAY': 'wayland-test', 'XDG_RUNTIME_DIR': directory}):
                self.assertTrue(model.request('get')['nested'])
                with self.assertRaisesRegex(RuntimeError, 'Unlock Navi'):
                    model.request('keep', token=3)
                with self.assertRaisesRegex(RuntimeError, 'too large'):
                    model.request('preview', arrangement='a' * 17000)
            thread.join(3)
            listener.close()
            self.assertFalse(thread.is_alive())
            self.assertEqual(observed, [{'op': 'get'}, {'op': 'keep', 'token': 3}])

    def test_wifi_profiles_keep_credentials_out_of_process_arguments(self):
        profile = model.wifi_profile(b'Test Wi-Fi', 'wpa-psk', 'dummy-passphrase', False, 'tester')
        self.assertEqual(profile['802-11-wireless-security']['psk'], ('s', 'dummy-passphrase'))
        self.assertEqual(profile['802-11-wireless-security']['psk-flags'], ('u', 2))
        self.assertEqual(profile['connection']['permissions'], ('as', ['user:tester:']))
        self.assertFalse(profile['connection']['autoconnect'][1])
        self.assertNotIn('802-11-wireless-security', model.wifi_profile(b'Open', 'open', '', True, 'tester'))
        for ssid, security, password in [(b'', 'open', ''), (b'x'*33, 'open', ''), (b'Test', 'enterprise', ''), (b'Test', 'wpa-psk', 'short')]:
            with self.assertRaises(ValueError):
                model.wifi_profile(ssid, security, password, True, 'tester')
        self.assertEqual(model.wifi_security({'RsnFlags': 0x400}), 'sae')
        self.assertEqual(model.wifi_security({'RsnFlags': 0x200}), 'enterprise')
        self.assertEqual(model.wifi_security({'Flags': 1}), 'wep')

    def test_native_variant_signatures_and_pairing_agent(self):
        try:
            from gi.repository import GLib, Gio
        except ImportError:
            self.skipTest('PyGObject is required for native variant checks')
        import gi
        gi.require_version('Gtk', '4.0')
        from gi.repository import Gtk
        for widget, method in [(Gtk.PasswordEntry, 'get_text'), (Gtk.AppChooserButton, 'new'), (Gtk.DropDown, 'new_from_strings')]:
            self.assertTrue(callable(getattr(widget, method)))
        raw = model.wifi_profile(b'Test', 'wpa-psk', 'dummy-password', True, 'tester')
        settings = {group: {key: GLib.Variant(sig, val) for key, (sig, val) in values.items()} for group, values in raw.items()}
        params = GLib.Variant('(a{sa{sv}}ooa{sv})', (settings, '/device', '/ap', {'persist': GLib.Variant('s', 'disk')}))
        self.assertEqual(params.unpack()[0]['802-11-wireless']['ssid'], list(b'Test'))
        agent = Gio.DBusNodeInfo.new_for_xml(model.AGENT_XML)
        self.assertIn('RequestConfirmation', [m.name for m in agent.interfaces[0].methods])
        self.assertIn('RequestPasskey', [m.name for m in agent.interfaces[0].methods])

    def test_audio_uses_fixed_commands_and_bounded_values(self):
        service = model.DesktopServices()
        with patch.object(model.shutil, 'which', return_value='/usr/bin/pactl'), patch.object(model.subprocess, 'run') as run:
            run.return_value.returncode = 0
            service.set_audio('sink', 51, 30, False, True)
            self.assertEqual([call.args[0] for call in run.call_args_list], [
                ['/usr/bin/pactl', '--', 'set-sink-volume', '51', '30%'],
                ['/usr/bin/pactl', '--', 'set-sink-mute', '51', '0'],
                ['/usr/bin/pactl', '--', 'set-default-sink', '51']])
            for call in run.call_args_list:
                self.assertNotIn('shell', call.kwargs)
                self.assertEqual(call.kwargs['timeout'], 8)
        with self.assertRaises(ValueError):
            service.set_audio('sink;anything', 1, 30, False)
        with self.assertRaises(ValueError):
            service.set_audio('source', 1, 101, False)
        self.assertEqual(model.audio_percent({'volume': {'left': {'value': 32768}, 'right': {'value': 65536}}}), 75)

    def test_pairing_registration_tracks_daemon_owner_and_has_no_implicit_trust(self):
        service = model.DesktopServices()
        calls = []
        def call(bus, path, interface, method, *args, **kwargs):
            calls.append(method)
            return (':1.42',) if method == 'GetNameOwner' else ()
        service.call = call
        service.pair('/device')
        service.pair('/device')
        self.assertEqual(calls.count('RegisterAgent'), 1)
        self.assertEqual(calls.count('Pair'), 2)
        self.assertIsNone(service.pairing_device)
        self.assertNotIn('Set', calls)
        service.blue_owner = ':1.old'
        service.pair('/device')
        self.assertEqual(calls.count('RegisterAgent'), 2)

    def test_about_returns_real_summary_without_serials_or_network_ids(self):
        values = dict(model.about_pc())
        self.assertIn('Operating system', values)
        self.assertIn('GiB', values['Memory'])
        self.assertNotIn('Serial number', values)

if __name__ == '__main__':
    unittest.main()
