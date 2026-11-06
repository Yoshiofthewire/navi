"""Native system services used only by explicit Settings controls, off GTK's thread."""
import datetime
import ipaddress
import re
import urllib.parse


def shell_request(op, **values):
    from gi.repository import Gio, GLib
    display = os.environ.get('WAYLAND_DISPLAY', '')
    if not display or '/' in display:
        raise RuntimeError('Open Settings from Navi’s launcher.')
    name = 'io.github.Yoshiofthewire.Navi.Shell.d' + display.encode().hex()[:160]
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    result = bus.call_sync(name, '/io/github/Yoshiofthewire/Navi/Shell',
        'io.github.Yoshiofthewire.Navi.Shell1', 'Request',
        GLib.Variant('(s)', (json.dumps(dict(op=op, **values)),)), None,
        Gio.DBusCallFlags.NO_AUTO_START, 5000, None)
    value = json.loads(result.unpack()[0])
    if not value.get('ok'):
        raise RuntimeError(value.get('error', 'Navi could not complete the request.'))
    return value


def theme_choices(kind):
    data = [Path(os.environ.get('XDG_DATA_HOME', str(Path.home() / '.local/share')))]
    data += [Path(p) for p in os.environ.get('XDG_DATA_DIRS', '/usr/local/share:/usr/share').split(':') if Path(p).is_absolute()]
    roots = [p / ('sounds' if kind == 'sounds' else 'icons') for p in data]
    if kind != 'sounds':
        roots.append(Path.home() / '.icons')
    names = set()
    for base in roots:
        try:
            for folder in list(base.iterdir())[:1024]:
                if folder.is_dir() and ((folder / 'cursors').is_dir() if kind == 'cursors' else (folder / 'index.theme').is_file()):
                    if re.fullmatch(r'[^/\\\x00-\x1f]{1,128}', folder.name):
                        names.add(folder.name)
        except OSError:
            pass
    return sorted(names, key=str.casefold)


def ipv4_settings(method, address, gateway, dns):
    if method not in ('auto', 'manual', 'disabled'):
        raise ValueError('Choose automatic, manual, or disabled IPv4.')
    servers = [str(ipaddress.IPv4Address(s.strip())) for s in dns.split(',') if s.strip()]
    if len(servers) > 8:
        raise ValueError('Use at most eight DNS servers.')
    values = {'method': ('s', method), 'dns-data': ('as', servers), 'ignore-auto-dns': ('b', bool(servers))}
    if method == 'manual':
        interface = ipaddress.IPv4Interface(address.strip())
        if '/' not in address:
            raise ValueError('Include the prefix, for example 192.168.1.20/24.')
        values['address-data'] = ('aa{sv}', [{'address': str(interface.ip), 'prefix': interface.network.prefixlen}])
        if gateway.strip():
            values['gateway'] = ('s', str(ipaddress.IPv4Address(gateway.strip())))
    return values


class SystemServices:
    def authorized_call(self, *args):
        return self.call(*args,timeout=120000)

    def tool(self, program, *args):
        path = shutil.which(program)
        if not path:
            raise RuntimeError('Install ' + program + ' to use these controls.')
        result = subprocess.run([path, *args], stdin=subprocess.DEVNULL, capture_output=True,
                                text=True, timeout=15, check=False, env=dict(os.environ, LC_ALL='C'))
        if result.returncode and program == 'lpstat' and any(s in (result.stdout + result.stderr) for s in ('No destinations added', 'no system default destination')):
            return ''
        if result.returncode:
            raise RuntimeError(program + ' could not complete the request. Check service availability and permissions.')
        return result.stdout[:262144]

    def power(self):
        profiles = []
        active = ''
        try:
            props = self.properties('net.hadess.PowerProfiles', '/net/hadess/PowerProfiles', 'net.hadess.PowerProfiles')
            profiles = [p['Profile'] for p in props.get('Profiles', [])]
            active = props.get('ActiveProfile', '')
        except Exception:
            pass
        backlights = []
        for device in sorted(Path('/sys/class/backlight').glob('*'))[:16]:
            try:
                maximum = int((device / 'max_brightness').read_text())
                current = int((device / 'brightness').read_text())
                if maximum > 0:
                    backlights.append(dict(name=device.name, maximum=maximum, percent=round(current / maximum * 100)))
            except (OSError, ValueError):
                pass
        return dict(profiles=profiles, active=active, backlights=backlights)

    def power_profile(self, profile):
        if profile not in self.power()['profiles']:
            raise ValueError('Choose an available power profile.')
        self.set_property('net.hadess.PowerProfiles', '/net/hadess/PowerProfiles', 'net.hadess.PowerProfiles', 'ActiveProfile', 's', profile)

    def brightness(self, name, percent):
        devices = self.power()['backlights']
        device = next((d for d in devices if d['name'] == name), None)
        if not device or not 1 <= percent <= 100:
            raise ValueError('Choose an available backlight and brightness from 1–100%.')
        path = self.call('org.freedesktop.login1', '/org/freedesktop/login1', 'org.freedesktop.login1.Manager', 'GetSessionByPID', '(u)', (os.getpid(),))[0]
        self.call('org.freedesktop.login1', path, 'org.freedesktop.login1.Session', 'SetBrightness', '(ssu)',
                  ('backlight', name, max(1, round(device['maximum'] * percent / 100))))

    def regional(self):
        clock = self.properties('org.freedesktop.timedate1', '/org/freedesktop/timedate1', 'org.freedesktop.timedate1')
        locale = self.properties('org.freedesktop.locale1', '/org/freedesktop/locale1', 'org.freedesktop.locale1')
        locales = sorted(set(self.tool('locale', '-a').splitlines()))
        zones = []
        try:
            for line in Path('/usr/share/zoneinfo/zone.tab').read_text().splitlines():
                if line and not line.startswith('#'):
                    zones.append(line.split('\t')[2])
        except OSError:
            pass
        return dict(timezone=clock.get('Timezone', 'UTC'), ntp=clock.get('NTP', False),
                    can_ntp=clock.get('CanNTP', False), zones=sorted(set(['UTC', *zones])),
                    locales=locales, locale=next((s[5:] for s in locale.get('Locale', []) if s.startswith('LANG=')), 'C'))

    def set_regional(self, zone, ntp, locale):
        current = self.regional()
        if zone not in current['zones'] or locale not in current['locales']:
            raise ValueError('Choose an installed locale and time zone.')
        self.authorized_call('org.freedesktop.timedate1', '/org/freedesktop/timedate1', 'org.freedesktop.timedate1', 'SetTimezone', '(sb)', (zone, True))
        if current['can_ntp']:
            self.authorized_call('org.freedesktop.timedate1', '/org/freedesktop/timedate1', 'org.freedesktop.timedate1', 'SetNTP', '(bb)', (bool(ntp), True))
        # Preserve LC_* overrides rather than replacing them when LANG changes.
        values = self.properties('org.freedesktop.locale1', '/org/freedesktop/locale1', 'org.freedesktop.locale1').get('Locale', [])
        values = [s for s in values if not s.startswith('LANG=')] + ['LANG=' + locale]
        self.authorized_call('org.freedesktop.locale1', '/org/freedesktop/locale1', 'org.freedesktop.locale1', 'SetLocale', '(asb)', (values, True))

    def set_clock(self, value):
        parsed = datetime.datetime.fromisoformat(value)
        if parsed.tzinfo is None:
            raise ValueError('Include a time-zone offset, for example 2026-11-06T14:30:00-05:00.')
        if not 2000 <= parsed.year <= 2100:
            raise ValueError('Use a year between 2000 and 2100.')
        self.authorized_call('org.freedesktop.timedate1', '/org/freedesktop/timedate1', 'org.freedesktop.timedate1', 'SetTime', '(xbb)', (int(parsed.timestamp() * 1000000), False, True))

    def accounts(self):
        name = 'org.freedesktop.Accounts'
        paths = self.call(name, '/org/freedesktop/Accounts', name, 'ListCachedUsers')[0]
        users = []
        for path in paths[:128]:
            p = self.properties(name, path, name + '.User')
            users.append(dict(path=path, name=display_name(p.get('UserName', '')), real_name=display_name(p.get('RealName', '')),
                              uid=p.get('Uid'), admin=p.get('AccountType', 0) == 1, locked=p.get('Locked', False)))
        return users

    def create_account(self, name, real_name, admin=False):
        if not re.fullmatch(r'[a-z_][a-z0-9_-]{0,31}', name) or len(real_name) > 128 or any(ord(c) < 32 for c in real_name):
            raise ValueError('Use a Linux account name: lowercase letters, digits, underscores and hyphens.')
        # New accounts remain password-locked until the system password tool sets a password.
        self.authorized_call('org.freedesktop.Accounts', '/org/freedesktop/Accounts', 'org.freedesktop.Accounts', 'CreateUser', '(ssi)', (name, real_name, int(bool(admin))))

    def password_tool(self, username):
        account = next((u for u in self.accounts() if u['name'] == username), None)
        if not account or not re.fullmatch(r'[a-z_][a-z0-9_-]{0,31}', username):
            raise ValueError('Choose an existing local account.')
        args = ['/usr/bin/passwd'] if account['uid'] == os.getuid() else ['/usr/bin/pkexec', '/usr/bin/passwd', username]
        terminals = [('konsole', ['--separate', '-e']), ('gnome-terminal', ['--']), ('xterm', ['-e'])]
        for name, flags in terminals:
            if (program := shutil.which(name)):
                child = subprocess.Popen([program, *flags, *args], stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                threading.Thread(target=child.wait, daemon=True).start()
                return
        raise RuntimeError('Install a terminal (Konsole, GNOME Terminal, or xterm) to use the system password tool.')

    def delete_account(self, path):
        users = self.accounts()
        account = next((u for u in users if u['path'] == path), None)
        if not account or account['uid'] == os.getuid() or account['uid'] < 1000:
            raise ValueError('You cannot remove your current account or a system account here.')
        if account['admin'] and not any(u['admin'] and not u['locked'] and u['path'] != path for u in users):
            raise ValueError('Keep at least one unlocked administrator account.')
        self.authorized_call('org.freedesktop.Accounts', '/org/freedesktop/Accounts', 'org.freedesktop.Accounts', 'DeleteUser', '(xb)', (account['uid'], False))

    def update_account(self, path, real_name, admin, locked):
        users = self.accounts()
        user = next((u for u in users if u['path'] == path), None)
        if user is None or user['uid'] < 1000 or len(real_name) > 128 or any(ord(c) < 32 for c in real_name):
            raise ValueError('Choose an existing account and a valid display name.')
        if user['uid'] == os.getuid() and (locked or (user['admin'] and not admin)):
            raise ValueError('You cannot lock yourself or remove your own administrator access here.')
        if user['admin'] and (locked or not admin) and not any(u['admin'] and not u['locked'] and u['path'] != path for u in users):
            raise ValueError('Keep at least one unlocked administrator account.')
        for method, signature, value in [('SetRealName', '(s)', real_name), ('SetAccountType', '(i)', int(bool(admin))), ('SetLocked', '(b)', bool(locked))]:
            self.authorized_call('org.freedesktop.Accounts', path, 'org.freedesktop.Accounts.User', method, signature, (value,))

    def printers(self):
        rows = []
        for line in self.tool('lpstat', '-p').splitlines()[:128]:
            parts = line.split()
            if len(parts) >= 2 and parts[0] == 'printer':
                rows.append(dict(name=parts[1], status=display_name(' '.join(parts[2:]))))
        default = self.tool('lpstat', '-d').strip().partition(':')[2].strip()
        jobs = []
        for line in self.tool('lpstat', '-o').splitlines()[:128]:
            parts = line.split()
            if len(parts) >= 3 and re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_.-]*-[0-9]+', parts[0]):
                jobs.append(dict(id=parts[0], description=display_name(' '.join(parts[1:]))))
        return dict(printers=rows, default=default, jobs=jobs)

    def cancel_print_job(self, job):
        if not any(j['id'] == job for j in self.printers()['jobs']):
            raise ValueError('Choose an existing print job.')
        self.tool('cancel', job)

    def printer_action(self, name, action):
        if not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_.-]{0,126}', name):
            raise ValueError('Choose a valid printer name.')
        if action == 'default':
            self.tool('lpoptions', '-d', name)
        elif action == 'remove':
            self.tool('lpadmin', '-x', name)
        elif action in ('enable', 'disable'):
            self.tool('cupsenable' if action == 'enable' else 'cupsdisable', name)
        else:
            raise ValueError('Unknown printer action.')

    def add_printer(self, name, uri):
        if not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_.-]{0,126}', name):
            raise ValueError('Use a printer name containing letters, digits, underscores or hyphens.')
        url = urllib.parse.urlsplit(uri)
        if url.scheme not in ('ipp', 'ipps') or not url.hostname or url.username or url.password or len(uri) > 1000:
            raise ValueError('Enter an ipp:// or ipps:// printer address without credentials.')
        self.tool('lpadmin', '-p', name, '-E', '-v', uri, '-m', 'everywhere')

    def push(self):
        result = shell_request('get')['push']
        result['clients'] = []
        if not result.get('owned'):
            return result
        result['state'] = ['Unknown', 'Idle', 'Connected', 'Network unavailable', 'Authentication failed', 'Provider setup required'][max(0, min(5, self.push_call('status')[0]))]
        result['clients'] = [dict(token=t, service=display_name(s), description=display_name(d)) for t, s, d in self.push_call('registeredClients')[0][:128]]
        return result

    def push_call(self, method, signature=None, args=()):
        from gi.repository import Gio, GLib
        bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        owned = shell_request('get')['push']
        if not owned.get('owned'):
            raise RuntimeError('Navi is not managing a UnifiedPush distributor.')
        unique = bus.call_sync('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus','GetNameOwner',GLib.Variant('(s)',('org.unifiedpush.Distributor.kde',)),None,Gio.DBusCallFlags.NO_AUTO_START,5000,None).unpack()[0]
        owner = bus.call_sync('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus','GetConnectionUnixProcessID',
            GLib.Variant('(s)',(unique,)),None,Gio.DBusCallFlags.NO_AUTO_START,5000,None).unpack()[0]
        if owner != owned.get('pid'):
            raise RuntimeError('The distributor belongs to another session. Navi leaves it unchanged.')
        result = bus.call_sync(unique, '/Management', 'org.kde.kunifiedpush.Management', method,
            GLib.Variant(signature, args) if signature else None, None, Gio.DBusCallFlags.NO_AUTO_START, 5000, None)
        return result.unpack() if result else ()

    def remove_push_client(self, token):
        state = self.push()
        if not state.get('owned') or not any(c['token'] == token for c in state['clients']):
            raise ValueError('This registration is no longer managed by Navi.')
        self.push_call('forceUnregisterClient', '(s)', (token,))

    def profile_settings(self, path):
        if not re.fullmatch(r'/org/freedesktop/NetworkManager/Settings/[0-9]+', path):
            raise ValueError('Choose a saved NetworkManager profile.')
        return self.call(NM, path, NM + '.Settings.Connection', 'GetSettings')[0]

    def update_ipv4(self, path, method, address, gateway, dns, proxy):
        from gi.repository import GLib, Gio
        values = ipv4_settings(method, address, gateway, dns)
        if proxy:
            url = urllib.parse.urlsplit(proxy)
            if url.scheme != 'https' or not url.hostname or url.username or url.password or len(proxy) > 1000:
                raise ValueError('Use an HTTPS proxy auto-configuration (PAC) URL without credentials.')
        # Preserve exact variant types in all untouched sections, including VPN/802.1x settings.
        raw = self.bus.call_sync(NM, path, NM + '.Settings.Connection', 'GetSettings', None, None, Gio.DBusCallFlags.NONE, 8000, None).get_child_value(0)
        def typed_sections(raw):
            result={}
            for i in range(raw.n_children()):
                section=raw.get_child_value(i)
                data=section.get_child_value(1)
                result[section.get_child_value(0).get_string()]={data.get_child_value(j).get_child_value(0).get_string():data.get_child_value(j).get_child_value(1).get_variant() for j in range(data.n_children())}
            return result
        profile=typed_sections(raw)
        # Update2 replaces the profile. Preserve stored secrets without displaying or logging them.
        for section in ('802-11-wireless-security','802-1x','vpn','wireguard','pppoe','gsm','cdma'):
            if section not in profile: continue
            try:
                secrets=self.bus.call_sync(NM,path,NM+'.Settings.Connection','GetSecrets',GLib.Variant('(s)',(section,)),None,Gio.DBusCallFlags.NONE,8000,None).get_child_value(0)
                for group,keys in typed_sections(secrets).items(): profile.setdefault(group,{}).update(keys)
            except Exception:
                raise RuntimeError('Could not preserve this connection’s credentials. Unlock its system keyring and try again; no changes were saved.') from None
        previous = profile.setdefault('ipv4', {})
        for key in ('address-data', 'addresses', 'gateway', 'dns', 'dns-data', 'ignore-auto-dns'):
            previous.pop(key, None)
        for key, (sig, value) in values.items():
            if sig == 'aa{sv}':
                value = [{k: GLib.Variant('u' if k == 'prefix' else 's', v) for k, v in a.items()} for a in value]
            previous[key] = GLib.Variant(sig, value)
        profile['proxy'] = dict(method=GLib.Variant('i', int(bool(proxy))))
        if proxy:
            profile['proxy']['pac-url'] = GLib.Variant('s', proxy)
        try:
            self.call(NM, path, NM + '.Settings.Connection', 'Update2', '(a{sa{sv}}ua{sv})', (profile, 1, {}))
        except Exception:
            raise RuntimeError('Could not save this connection. Check NetworkManager permissions and try again.') from None

    def import_vpn(self, path):
        file = Path(path)
        if not file.is_absolute() or not file.is_file() or file.stat().st_size > 1048576 or file.suffix.lower() not in ('.ovpn','.conf'):
            raise ValueError('Choose an OpenVPN .ovpn or WireGuard .conf profile smaller than 1 MiB.')
        self.tool('nmcli', 'connection', 'import', 'type', 'openvpn' if file.suffix.lower()=='.ovpn' else 'wireguard', 'file', str(file))
