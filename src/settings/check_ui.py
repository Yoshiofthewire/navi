"""Native UI check inside a disposable Navi, with simulated device services.

Set NAVI_UI_CHECK_OUTPUT to Navi's screenshot directory and give this process that
Navi's WAYLAND_DISPLAY and private XDG_CONFIG_HOME. Never changes host devices.
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading

import gi
gi.require_version('Gtk', '4.0')
from gi.repository import Gtk, GLib

root = Path(__file__).resolve().parent
output = Path(os.environ['NAVI_UI_CHECK_OUTPUT'])
assert Path(os.environ['XDG_CONFIG_HOME']).is_relative_to('/tmp'), 'Use disposable XDG configuration'
namespace = {'__name__': 'settings_ui_check'}
exec('\n'.join((root / file).read_text() for file in ['model.py', 'services.py', 'app.py']), namespace)

class Devices(namespace['DesktopServices']):
    def __init__(self):
        super().__init__()
        self.operations = []
        self.wifi_on = True
        self.connected = False
        self.long_names = False
    @property
    def bus(self):
        raise AssertionError("The UI check must never contact the system bus")
    def audio_command(self, *args, **kwargs):
        raise AssertionError("The UI check must never contact the host audio service")
    def network(self):
        return dict(enabled=self.wifi_on, hardware=True,
                    devices=[dict(kind=2, path='/wifi', interface='Test wireless adapter', profiles=[], connected=False)],
                    networks=[dict(name=('Test Wi-Fi ' * 10) if self.long_names else 'Test Wi-Fi', ssid=b'Test Wi-Fi', security='wpa-psk', strength=80, path='/ap', device='/wifi', connected=self.connected, profile=None)], profiles=[])
    def bluetooth(self):
        return dict(adapters=[dict(path='/adapter', name='Test Bluetooth adapter', powered=True)],
                    devices=[dict(path='/device', adapter='/adapter', name='Test headphones', paired=True, connected=False, trusted=False, battery=75)])
    def sound(self):
        return dict(outputs=[dict(index=1, name='test-output', label='Test speakers', volume=45, mute=False)],
                    inputs=[dict(index=2, name='test-input', label='Test microphone', volume=60, mute=False)],
                    applications=[dict(index=3, label='Test music player', volume=50, mute=False)],
                    default_output='test-output', default_input='test-input')
    def connect_network(self, device, **values):
        assert threading.current_thread() != threading.main_thread()
        assert values['password'] == 'dummy-password'
        self.operations.append('wifi-connect')
        self.connected = True
    def set_audio(self, kind, index, volume, mute, default=False):
        assert threading.current_thread() != threading.main_thread()
        assert isinstance(index, int) and isinstance(volume, int) and isinstance(mute, bool)
        self.operations.append('audio-apply')
    def close(self):
        self.cancelled.set()

namespace['DesktopServices'] = Devices
steps = list(range(9)) + ['empty', 'search', 'wifi-form', 'wifi-password', 'audio', 'pair-confirm', 'pair-accept', 'pair-passkey', 'pair-passkey-accept', 'preview', 'revert']
if os.environ.get('NAVI_UI_CHECK_RESIZE_HELPER'):
    steps += ['small', 'small-network', 'small-sound']
    steps += ['compact-' + str(i) for i in range(9)]
    steps += ['compact-empty', 'compact-clear', 'compact-preview', 'compact-preview-scroll', 'compact-revert']
    steps += ['narrow-network', 'narrow-sound', 'keyboard-focus', 'unavailable']
failures = []
original = Gtk.ApplicationWindow.present

class Invocation:
    def __init__(self):
        self.values = []
        self.errors = []
    def return_value(self, value):
        self.values.append(value)
    def return_dbus_error(self, name, message):
        self.errors.append(name)

def all_widgets(widget):
    yield widget
    child = widget.get_first_child()
    while child:
        yield from all_widgets(child)
        child = child.get_next_sibling()

def present(window):
    original(window)
    app = window.get_application()
    GLib.timeout_add(700, lambda: (window.maximize(), False)[1])
    pending = {}
    def advance():
        try:
            if not app.snapshot:
                raise AssertionError('Navi settings socket did not connect')
            if not steps:
                assert 'wifi-connect' in app.services.operations
                assert 'audio-apply' in app.services.operations
                print('NATIVE UI CHECK PASSED: pages, search, credential submission, audio, pairing sender/device guards, confirmation/passkey and preview/revert', flush=True)
                app.quit()
                return False
            step = steps.pop(0)
            if isinstance(step, int):
                app.nav.select_row(app.rows[step])
                name = 'page-' + str(step)
            elif step == 'empty':
                app.search.set_text('zzzz-no-match')
                name = 'search-empty'
            elif step == 'search':
                assert app.empty.get_visible()
                app.search.set_text('microphone')
                name = 'search-sound'
            elif step == 'wifi-form':
                assert app.page == 4
                app.search.set_text('')
                app.nav.select_row(app.rows[2])
                network = app.services.network()['networks'][0]
                app.wifi_dialog(network)
                name = 'wifi-password-form'
            elif step == 'wifi-password':
                dialog = next(w for w in Gtk.Window.list_toplevels() if isinstance(w, Gtk.Dialog))
                entry = next(w for w in all_widgets(dialog) if isinstance(w, Gtk.PasswordEntry))
                entry.set_text('dummy-password')
                dialog.response(Gtk.ResponseType.ACCEPT)
                assert entry.get_text() == ''
                name = 'wifi-connected'
            elif step == 'audio':
                app.nav.select_row(app.rows[4])
                def apply():
                    button = next(w for w in all_widgets(app.content) if isinstance(w, Gtk.Button) and w.get_label() == 'Apply Output')
                    button.emit('clicked')
                    return False
                GLib.timeout_add(400, apply)
                name = 'audio-applied'
            elif step == 'pair-confirm':
                app.nav.select_row(app.rows[3])
                app.services.blue_owner = ':1.fixture'
                app.services.pairing_device = '/device'
                app.services.agent_device = '/device'
                wrong = Invocation()
                app.pair_request(None, ':1.forged', '', '', 'RequestConfirmation', GLib.Variant('(ou)', ('/device', 123456)), wrong)
                assert wrong.errors and not wrong.values
                wrong_device = Invocation()
                app.pair_request(None, ':1.fixture', '', '', 'RequestAuthorization', GLib.Variant('(o)', ('/other',)), wrong_device)
                assert wrong_device.errors and not wrong_device.values
                pending['confirm'] = Invocation()
                app.pair_request(None, ':1.fixture', '', '', 'RequestConfirmation', GLib.Variant('(ou)', ('/device', 123456)), pending['confirm'])
                assert not pending['confirm'].values
                name = 'pairing-confirm'
            elif step == 'pair-accept':
                app.pair_dialog.response(Gtk.ResponseType.ACCEPT)
                assert pending['confirm'].values == [None]
                name = 'pairing-accepted'
            elif step == 'pair-passkey':
                pending['key'] = Invocation()
                app.pair_request(None, ':1.fixture', '', '', 'RequestPasskey', GLib.Variant('(o)', ('/device',)), pending['key'])
                name = 'pairing-passkey'
            elif step == 'pair-passkey-accept':
                entry = next(w for w in all_widgets(app.pair_dialog) if isinstance(w, Gtk.Entry))
                entry.set_text('-1')
                app.pair_dialog.response(Gtk.ResponseType.ACCEPT)
                assert not pending['key'].values
                entry.set_text('123456')
                app.pair_dialog.response(Gtk.ResponseType.ACCEPT)
                assert pending['key'].values[0].unpack() == (123456,)
                app.services.pairing_device = None
                name = 'pairing-passkey-accepted'
            elif step == 'preview':
                app.nav.select_row(app.rows[0])
                app.preview_displays()
                name = 'display-preview'
            elif step == 'revert':
                assert app.token is not None
                app.finish_preview('revert')
                name = 'display-restored'
            elif step == 'small':
                pid = os.environ['NAVI_UI_CHECK_NAVI_PID']
                subprocess.run([os.environ['NAVI_UI_CHECK_RESIZE_HELPER'], pid, '660', '650'], check=True, env=dict(os.environ, DISPLAY=os.environ['NAVI_UI_CHECK_HOST_DISPLAY']))
                app.nav.select_row(app.rows[8])
                name = 'small-about'
            elif step in ('small-network', 'small-sound'):
                assert app.compact.get_visible() and not app.sidebar_scroll.get_visible()
                app.compact_select.set_selected(2 if step == 'small-network' else 4)
                name = step
            elif step.startswith('compact-') and step.split('-')[1].isdigit():
                app.compact_select.set_selected(int(step.split('-')[1]))
                name = step
            elif step == 'compact-empty':
                app.compact_search.set_text('zzzz-no-match')
                name = step
            elif step == 'compact-clear':
                app.compact_search.set_text('')
                name = step
            elif step == 'compact-preview':
                app.nav.select_row(app.rows[0])
                app.preview_displays()
                name = step
            elif step == 'compact-preview-scroll':
                adjustment = app.scroll.get_vadjustment()
                adjustment.set_value(adjustment.get_upper() - adjustment.get_page_size())
                name = step
            elif step == 'compact-revert':
                assert app.token is not None
                app.finish_preview('revert')
                name = step
            elif step == 'narrow-network':
                subprocess.run([os.environ['NAVI_UI_CHECK_RESIZE_HELPER'], os.environ['NAVI_UI_CHECK_NAVI_PID'], '420', '740'], check=True, env=dict(os.environ, DISPLAY=os.environ['NAVI_UI_CHECK_HOST_DISPLAY']))
                app.services.long_names = True
                app.nav.select_row(app.rows[2])
                def reveal_network():
                    adjustment = app.scroll.get_vadjustment()
                    adjustment.set_value(adjustment.get_upper() - adjustment.get_page_size())
                    return False
                GLib.timeout_add(800, reveal_network)
                name = step
            elif step == 'narrow-sound':
                assert app.window.get_width() <= 420, 'Settings forced the narrow tile wider'
                app.nav.select_row(app.rows[4])
                name = step
            elif step == 'keyboard-focus':
                app.compact_search.grab_focus()
                name = step
            elif step == 'unavailable':
                focus = app.window.get_focus()
                assert focus and focus.is_ancestor(app.compact_search), 'Search keyboard focus was lost'
                def unavailable():
                    raise RuntimeError('Simulated unavailable network service')
                app.services.network = unavailable
                app.nav.select_row(app.rows[2])
                name = step
            def capture():
                try:
                    assert app.content.get_width() <= app.scroll.get_width(), 'Settings content overflowed the tile'
                    if app.search.get_text() == 'zzzz-no-match':
                        assert int(app.empty.get_mapped()) + int(app.compact_empty.get_mapped()) == 1, 'Duplicate or missing no-results message'
                    if app.token is not None and app.page == 0:
                        for widget in (app.preview_box, app.preview_box.get_last_child()):
                            valid, bounds = widget.compute_bounds(app.window)
                            assert valid and bounds.get_y() >= 0 and bounds.get_y() + bounds.get_height() <= app.window.get_height(), 'Timed display recovery was clipped'
                    if step == 4 or app.page == 8:
                        labels = ('Apply Output', 'Apply Input') if step == 4 else ('Copy System Information',)
                        for label in labels:
                            button = next(w for w in all_widgets(app.window) if isinstance(w, Gtk.Button) and w.get_label() == label)
                            valid, bounds = button.compute_bounds(app.window)
                            assert valid and bounds.get_y() >= 0 and bounds.get_y() + bounds.get_height() <= app.window.get_height(), label + ' was outside the first viewport'
                except AssertionError as error:
                    failures.append(str(error))
                    print('UI CHECK FAILED:', str(error), flush=True)
                    app.quit()
                    return False
                shutil.copyfile(output / 'settings-live.png', output / (name + '.png'))
                print('Captured', name, flush=True)
                GLib.timeout_add(100, advance)
                return False
            # Navi captures at 1 Hz; allow background-window frame throttling.
            GLib.timeout_add(3500, capture)
        except Exception as error:
            failures.append(str(error))
            print('UI CHECK FAILED:', type(error).__name__, str(error), flush=True)
            app.quit()
        return False
    GLib.timeout_add(2500, advance)
Gtk.ApplicationWindow.present = present
code = namespace['run_settings']()
raise SystemExit(1 if failures else code)
