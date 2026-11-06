# Native desktop services. Credentials travel only over NetworkManager's D-Bus API.
import threading
import time
import unicodedata
import uuid

NM = 'org.freedesktop.NetworkManager'
NM_ROOT = '/org/freedesktop/NetworkManager'
PROPERTIES = 'org.freedesktop.DBus.Properties'
BLUEZ = 'org.bluez'
AGENT_XML = '''<node><interface name="org.bluez.Agent1">
<method name="Release"/>
<method name="RequestPinCode"><arg type="o" direction="in"/><arg type="s" direction="out"/></method>
<method name="RequestPasskey"><arg type="o" direction="in"/><arg type="u" direction="out"/></method>
<method name="DisplayPinCode"><arg type="o" direction="in"/><arg type="s" direction="in"/></method>
<method name="DisplayPasskey"><arg type="o" direction="in"/><arg type="u" direction="in"/><arg type="q" direction="in"/></method>
<method name="RequestConfirmation"><arg type="o" direction="in"/><arg type="u" direction="in"/></method>
<method name="RequestAuthorization"><arg type="o" direction="in"/></method>
<method name="AuthorizeService"><arg type="o" direction="in"/><arg type="s" direction="in"/></method>
<method name="Cancel"/>
</interface></node>'''

def display_name(value):
    value = str(value)
    return ''.join(c for c in value if unicodedata.category(c) not in ('Cc', 'Cf'))[:160] or 'Unnamed device'

def wifi_security(properties):
    flags = properties.get('RsnFlags', 0) | properties.get('WpaFlags', 0)
    if flags & 0x100:
        return 'wpa-psk'
    if flags & 0x400:
        return 'sae'
    if flags & (0x200 | 0x2000):
        return 'enterprise'
    if flags & (0x800 | 0x1000):
        return 'owe'
    return 'wep' if properties.get('Flags', 0) & 1 else 'open'

def wifi_profile(ssid, security, password, remember, username, hidden=False):
    if not 1 <= len(ssid) <= 32:
        raise ValueError('A Wi-Fi network name must contain 1–32 bytes.')
    if security not in ('open', 'wpa-psk', 'sae', 'owe'):
        raise ValueError('This security type needs a provisioned NetworkManager profile.')
    if security in ('wpa-psk', 'sae'):
        size = len(password.encode('utf-8'))
        raw_key = security == 'wpa-psk' and len(password) == 64 and all(c in '0123456789abcdefABCDEF' for c in password)
        if not raw_key and not ((8 if security == 'wpa-psk' else 1) <= size <= 63):
            raise ValueError('Enter a valid Wi-Fi password (8–63 bytes for WPA2, 1–63 for WPA3).')
    profile = {
        'connection': {'id': ('s', display_name(ssid.decode('utf-8', 'replace'))), 'uuid': ('s', str(uuid.uuid4())),
                       'type': ('s', '802-11-wireless'), 'permissions': ('as', ['user:' + username + ':']),
                       'autoconnect': ('b', bool(remember))},
        '802-11-wireless': {'ssid': ('ay', list(ssid)), 'hidden': ('b', hidden)},
        'ipv4': {'method': ('s', 'auto')}, 'ipv6': {'method': ('s', 'auto')},
    }
    if security != 'open':
        profile['802-11-wireless-security'] = {'key-mgmt': ('s', security)}
        if security in ('wpa-psk', 'sae'):
            profile['802-11-wireless-security'].update({'psk': ('s', password), 'psk-flags': ('u', 0 if remember else 2)})
    return profile

def audio_percent(item):
    channels = list(item.get('volume', {}).values())
    return round(sum(c.get('value', 0) for c in channels) / len(channels) / 65536 * 100) if channels else 0

class DesktopServices:
    def __init__(self):
        self._bus = None
        self.cancelled = threading.Event()
        self.discovering = set()
        self.agent_path = '/io/github/Yoshiofthewire/Navi/Settings/Agent' + str(os.getpid())
        self.agent_registration = None
        self.agent_registered = False
        self.blue_owner = None
        self.pairing_device = None
        self.agent_device = None

    @property
    def bus(self):
        if self._bus is None:
            from gi.repository import Gio
            self._bus = Gio.bus_get_sync(Gio.BusType.SYSTEM, None)
        return self._bus

    def call(self, service, path, interface, method, signature=None, args=(), timeout=8000):
        from gi.repository import Gio, GLib
        parameters = GLib.Variant(signature, args) if signature else None
        result = self.bus.call_sync(service, path, interface, method, parameters, None, Gio.DBusCallFlags.NONE, timeout, None)
        return result.unpack() if result else ()

    def properties(self, service, path, interface):
        return self.call(service, path, PROPERTIES, 'GetAll', '(s)', (interface,))[0]

    def set_property(self, service, path, interface, key, signature, value):
        from gi.repository import GLib
        self.call(service, path, PROPERTIES, 'Set', '(ssv)', (interface, key, GLib.Variant(signature, value)))

    def network(self):
        deadline = time.monotonic() + 8
        manager = self.properties(NM, NM_ROOT, NM)
        profiles = []
        try:
            paths = self.call(NM, NM_ROOT + '/Settings', NM + '.Settings', 'ListConnections')[0]
            for path in paths[:512]:
                if time.monotonic() >= deadline or self.cancelled.is_set():
                    break
                try:
                    data = self.call(NM, path, NM + '.Settings.Connection', 'GetSettings')[0]
                    profiles.append({'path': path, 'settings': data})
                except Exception:
                    continue
        except Exception:
            pass
        devices, networks = [], {}
        for path in manager.get('Devices', [])[:64]:
            if self.cancelled.is_set():
                break
            props = self.properties(NM, path, NM + '.Device')
            kind = props.get('DeviceType')
            if kind not in (1, 2):
                continue
            device = dict(path=path, interface=display_name(props.get('Interface', 'Network adapter')), kind=kind,
                          connected=props.get('State') == 100, state=props.get('State', 0),
                          profiles=props.get('AvailableConnections', []))
            devices.append(device)
            if kind != 2:
                continue
            wireless = self.properties(NM, path, NM + '.Device.Wireless')
            for ap_path in wireless.get('AccessPoints', [])[:256]:
                if time.monotonic() >= deadline or self.cancelled.is_set():
                    break
                try:
                    ap = self.properties(NM, ap_path, NM + '.AccessPoint')
                except Exception:
                    continue
                ssid = bytes(ap.get('Ssid', []))
                security = wifi_security(ap)
                saved = next((p for p in profiles if p['path'] in device['profiles'] and bytes(p['settings'].get('802-11-wireless', {}).get('ssid', [])) == ssid), None)
                network = dict(name=display_name(ssid.decode('utf-8', 'replace')) if ssid else 'Hidden network', ssid=ssid,
                               security=security, strength=int(ap.get('Strength', 0)), path=ap_path, device=path,
                               connected=wireless.get('ActiveAccessPoint') == ap_path,
                               profile=saved['path'] if saved else None)
                key = (ssid, security)
                old = networks.get(key)
                if old is None or network['connected'] or (not old['connected'] and network['strength'] > old['strength']):
                    networks[key] = network
        return dict(enabled=manager.get('WirelessEnabled', False), hardware=manager.get('WirelessHardwareEnabled', False),
                    devices=devices, networks=sorted(networks.values(), key=lambda n: (not n['connected'], -n['strength'], n['name'])),
                    profiles=[dict(path=p['path'], name=display_name(p['settings'].get('connection', {}).get('id', 'Connection')),
                                   kind=p['settings'].get('connection', {}).get('type')) for p in profiles])

    def scan_wifi(self, device):
        self.call(NM, device, NM + '.Device.Wireless', 'RequestScan', '(a{sv})', ({},))

    def wifi_enabled(self, enabled):
        self.set_property(NM, NM_ROOT, NM, 'WirelessEnabled', 'b', enabled)

    def disconnect_network(self, device):
        self.call(NM, device, NM + '.Device', 'Disconnect')

    def forget_network(self, profile):
        self.call(NM, profile, NM + '.Settings.Connection', 'Delete')

    def connect_network(self, device, profile=None, network=None, password='', remember=True, hidden=False):
        from gi.repository import GLib
        try:
            if profile:
                active = self.call(NM, NM_ROOT, NM, 'ActivateConnection', '(ooo)', (profile, device, network['path'] if network else '/'))[0]
            else:
                settings = {}
                specific = '/'
                if network:
                    spec = wifi_profile(network['ssid'], network['security'], password, remember, GLib.get_user_name(), hidden)
                    settings = {section: {key: GLib.Variant(signature, value) for key, (signature, value) in values.items()} for section, values in spec.items()}
                    specific = network.get('path', '/')
                active = self.call(NM, NM_ROOT, NM, 'AddAndActivateConnection2', '(a{sa{sv}}ooa{sv})',
                                   (settings, device, specific, {'persist': GLib.Variant('s', 'disk' if remember else 'volatile')}))[1]
            deadline = time.monotonic() + 45
            while time.monotonic() < deadline and not self.cancelled.is_set():
                state = self.properties(NM, active, NM + '.Connection.Active').get('State')
                if state == 2:
                    return
                if state == 4:
                    break
                self.cancelled.wait(0.4)
            raise RuntimeError('The network did not connect. Check its credentials and signal, then try again.')
        except Exception:
            # Never propagate a service error that might echo a submitted credential.
            raise RuntimeError('Could not connect. Check the network password, permissions, and configuration.') from None

    def bluetooth(self):
        objects = self.call(BLUEZ, '/', 'org.freedesktop.DBus.ObjectManager', 'GetManagedObjects')[0]
        adapters, devices = [], []
        for path, interfaces in list(objects.items())[:512]:
            if BLUEZ + '.Adapter1' in interfaces:
                props = interfaces[BLUEZ + '.Adapter1']
                adapters.append(dict(path=path, name=display_name(props.get('Alias', 'Bluetooth adapter')), powered=props.get('Powered', False), discovering=props.get('Discovering', False)))
            if BLUEZ + '.Device1' in interfaces:
                props = interfaces[BLUEZ + '.Device1']
                devices.append(dict(path=path, adapter=props.get('Adapter'), name=display_name(props.get('Alias', props.get('Address', 'Bluetooth device'))),
                                    paired=props.get('Paired', False), connected=props.get('Connected', False), trusted=props.get('Trusted', False),
                                    battery=interfaces.get(BLUEZ + '.Battery1', {}).get('Percentage')))
        return dict(adapters=adapters, devices=sorted(devices, key=lambda d: (not d['paired'], d['name'])))

    def bluetooth_power(self, adapter, enabled):
        self.set_property(BLUEZ, adapter, BLUEZ + '.Adapter1', 'Powered', 'b', enabled)

    def discover(self, adapter, enabled):
        self.call(BLUEZ, adapter, BLUEZ + '.Adapter1', 'StartDiscovery' if enabled else 'StopDiscovery')
        if enabled:
            self.discovering.add(adapter)
        else:
            self.discovering.discard(adapter)

    def prepare_agent(self, callback):
        from gi.repository import Gio
        if self.agent_registration is None:
            node = Gio.DBusNodeInfo.new_for_xml(AGENT_XML)
            self.agent_registration = self.bus.register_object(self.agent_path, node.interfaces[0], callback, None, None)

    def register_agent(self):
        owner = self.call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus', 'GetNameOwner', '(s)', (BLUEZ,))[0]
        if owner != self.blue_owner:
            self.agent_registered = False
        self.blue_owner = owner
        if not self.agent_registered:
            self.call(BLUEZ, '/org/bluez', BLUEZ + '.AgentManager1', 'RegisterAgent', '(os)', (self.agent_path, 'KeyboardDisplay'))
            self.agent_registered = True

    def pair(self, path):
        self.register_agent()
        self.pairing_device = path
        self.agent_device = path
        try:
            self.call(BLUEZ, path, BLUEZ + '.Device1', 'Pair', timeout=120000)
        finally:
            self.pairing_device = None
            self.agent_device = None

    def device_action(self, path, action):
        if action not in ('Connect', 'Disconnect', 'CancelPairing'):
            raise ValueError('Unsupported Bluetooth action')
        if action == 'Connect':
            self.register_agent()
            self.agent_device = path
        try:
            self.call(BLUEZ, path, BLUEZ + '.Device1', action, timeout=30000)
        finally:
            if action == 'Connect':
                self.agent_device = None

    def forget_bluetooth(self, device):
        self.call(BLUEZ, device['adapter'], BLUEZ + '.Adapter1', 'RemoveDevice', '(o)', (device['path'],))

    def trust_bluetooth(self, path, value):
        self.set_property(BLUEZ, path, BLUEZ + '.Device1', 'Trusted', 'b', value)

    def audio_command(self, *args, json_output=False):
        program = shutil.which('pactl')
        if not program:
            raise RuntimeError('Install pactl (pulseaudio-utils on Debian/Ubuntu) and enable PipeWire’s PulseAudio service or PulseAudio.')
        command = [program, *(['--format=json'] if json_output else []), '--', *args]
        result = subprocess.run(command, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=8, check=False)
        if result.returncode:
            raise RuntimeError('The audio service could not complete the request. Check that PipeWire-Pulse or PulseAudio is running.')
        return json.loads(result.stdout) if json_output else None

    def sound(self):
        info = self.audio_command('info', json_output=True)
        def devices(kind):
            return [dict(index=int(d['index']), name=d['name'], label=display_name(d.get('description', d['name'])),
                         volume=audio_percent(d), mute=bool(d.get('mute')), channels=len(d.get('volume', {})))
                    for d in self.audio_command('list', kind, json_output=True)
                    if not (kind == 'sources' and d.get('monitor_of_sink') is not None)]
        applications = [dict(index=int(d['index']), label=display_name(d.get('properties', {}).get('application.name', d.get('name', 'Application'))),
                             volume=audio_percent(d), mute=bool(d.get('mute'))) for d in self.audio_command('list', 'sink-inputs', json_output=True)[:128]]
        return dict(outputs=devices('sinks'), inputs=devices('sources'), applications=applications,
                    default_output=info.get('default_sink_name'), default_input=info.get('default_source_name'))

    def set_audio(self, kind, index, volume, mute, default=False):
        if kind not in ('sink', 'source', 'sink-input') or not isinstance(index, int) or not 0 <= index < 2**32:
            raise ValueError('Invalid audio device')
        if not 0 <= volume <= 100:
            raise ValueError('Volume must be between 0 and 100 percent')
        self.audio_command('set-' + kind + '-volume', str(index), str(int(volume)) + '%')
        self.audio_command('set-' + kind + '-mute', str(index), '1' if mute else '0')
        if default and kind in ('sink', 'source'):
            self.audio_command('set-default-' + kind, str(index))

    def close(self):
        self.cancelled.set()
        if self._bus is None:
            return
        from gi.repository import Gio
        for adapter in list(self.discovering):
            self.bus.call(BLUEZ, adapter, BLUEZ + '.Adapter1', 'StopDiscovery', None, None, Gio.DBusCallFlags.NONE, 2000, None, None)
        if self.pairing_device:
            self.bus.call(BLUEZ, self.pairing_device, BLUEZ + '.Device1', 'CancelPairing', None, None, Gio.DBusCallFlags.NONE, 2000, None, None)
