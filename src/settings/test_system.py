"""Native service boundary checks. These never mutate the host."""
import importlib.util
import os
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('settings_system_model', Path(__file__).with_name('model.py'))
model = importlib.util.module_from_spec(spec)
spec.loader.exec_module(model)
for file in ('system.py', 'services.py'):
    exec(Path(__file__).with_name(file).read_text(), model.__dict__)


class SystemTests(unittest.TestCase):
    def test_enterprise_wifi_requires_verified_server_and_keeps_secrets_on_dbus(self):
        enterprise = dict(identity='alice@example.org', domain='radius.example.org')
        profile = model.wifi_profile(b'Office', 'enterprise', 'dummy-secret', False, 'alice', enterprise=enterprise)
        values = profile['802-1x']
        self.assertEqual(values['domain-suffix-match'], ('s', 'radius.example.org'))
        self.assertEqual(values['system-ca-certs'], ('b', True))
        self.assertEqual(values['password-flags'], ('u', 2))
        self.assertEqual(values['password'], ('s', 'dummy-secret'))
        for bad in ({}, dict(identity='alice', domain=''), dict(identity='alice', domain='bad/domain')):
            with self.assertRaises(ValueError):
                model.wifi_profile(b'Office', 'enterprise', 'secret', True, 'alice', enterprise=bad)

    def test_ipv4_editor_validates_prefix_gateway_and_dns_before_service_calls(self):
        values = model.ipv4_settings('manual', '192.168.20.5/24', '192.168.20.1', '1.1.1.1, 9.9.9.9')
        self.assertEqual(values['address-data'][1], [{'address': '192.168.20.5', 'prefix': 24}])
        self.assertEqual(values['dns-data'][1], ['1.1.1.1', '9.9.9.9'])
        self.assertTrue(values['ignore-auto-dns'][1])
        for args in [('manual','192.168.1.2','',''),('manual','bad/24','',''),('auto','','','not-a-dns')]:
            with self.assertRaises(ValueError):
                model.ipv4_settings(*args)

    def test_connection_update_preserves_vpn_types_and_untouched_ipv6(self):
        try:
            from gi.repository import GLib
        except ImportError:
            self.skipTest('PyGObject is required for native variants')
        original = {'connection': {'id': GLib.Variant('s','Office'), 'permissions': GLib.Variant('as',['user:alice:'])},
                    'vpn': {'service-type': GLib.Variant('s','org.freedesktop.NetworkManager.openvpn'), 'data': GLib.Variant('a{ss}',{'remote':'vpn.example.org'})},
                    'ipv6': {'method': GLib.Variant('s','auto')}}
        class Bus:
            def call_sync(self, *args):
                if args[3]=='GetSecrets': return GLib.Variant('(a{sa{sv}})',({'vpn':{'secrets':GLib.Variant('a{ss}',{'password':'dummy-preserved'})}},))
                return GLib.Variant('(a{sa{sv}})', (original,))
        service = model.DesktopServices()
        service._bus = Bus()
        calls = []
        service.call = lambda *args: calls.append(args)
        service.update_ipv4('/org/freedesktop/NetworkManager/Settings/1','manual','10.0.0.5/24','10.0.0.1','1.1.1.1','')
        values = calls[0][-1][0]
        self.assertEqual(values['vpn']['data'].unpack(), {'remote':'vpn.example.org'})
        self.assertEqual(values['ipv6']['method'].unpack(), 'auto')
        self.assertEqual(values['vpn']['secrets'].unpack(), {'password':'dummy-preserved'})
        self.assertEqual(values['ipv4']['address-data'].unpack(), [{'address':'10.0.0.5','prefix':24}])
        self.assertEqual(calls[0][-2], '(a{sa{sv}}ua{sv})')

    def test_accounts_cannot_remove_current_or_last_administrator(self):
        service = model.DesktopServices()
        service.accounts = lambda: [dict(path='/self',name='alice',uid=os.getuid(),admin=True,locked=False)]
        with patch.object(service,'call') as call:
            for action in (lambda: service.delete_account('/self'),lambda: service.update_account('/self','Alice',True,True),
                           lambda: service.update_account('/self','Alice',False,False),lambda: service.create_account('-bad','Bad')):
                with self.assertRaises(ValueError): action()
            call.assert_not_called()

    def test_printer_and_clock_arguments_are_validated_before_native_operations(self):
        service = model.DesktopServices()
        with patch.object(service,'tool') as tool, patch.object(service,'call') as call:
            for name, uri in [('-bad','ipps://printer.local/ipp/print'),('office','file:///etc/passwd'),('office','ipps://user:password@printer.local/ipp/print')]:
                with self.assertRaises(ValueError): service.add_printer(name,uri)
            with self.assertRaises(ValueError): service.set_clock('2026-11-06T12:00:00')
            tool.assert_not_called()
            call.assert_not_called()
            service.add_printer('office','ipps://printer.local/ipp/print')
            self.assertEqual(tool.call_args.args,('lpadmin','-p','office','-E','-v','ipps://printer.local/ipp/print','-m','everywhere'))

    def test_remembered_wifi_is_staged_before_nm_and_never_serializes_the_password(self):
        service = model.DesktopServices()
        order, calls = [], []
        def stage(profile):
            order.append('store')
            self.assertEqual(profile['802-11-wireless-security']['psk-flags'], ('u', 1))
            self.assertEqual(profile['802-11-wireless-security']['psk'], ('s', 'dummy-passphrase'))
            del profile['802-11-wireless-security']['psk']
        def call(*args):
            order.append('activate');calls.append(args)
            return ('/profile','/active',{})
        service.store_network_secret = stage
        service.call = call
        service.properties = lambda *args: {'State':2}
        service.connect_network('/device', network=dict(ssid=b'Fixture',security='wpa-psk',path='/ap'),password='dummy-passphrase')
        self.assertEqual(order, ['store','activate'])
        values=calls[0][-1][0]
        self.assertNotIn('psk', values['802-11-wireless-security'])
        self.assertEqual(values['802-11-wireless-security']['psk-flags'].unpack(), 1)
        self.assertNotIn('dummy-passphrase', repr(values))

    def test_failed_vault_save_does_not_create_or_activate_any_nm_profile(self):
        service = model.DesktopServices()
        service.store_network_secret = lambda _: (_ for _ in ()).throw(RuntimeError('Unlock oo7 and try again'))
        with patch.object(service,'call') as call:
            with self.assertRaisesRegex(RuntimeError,'oo7'):
                service.connect_network('/device',network=dict(ssid=b'Fixture',security='wpa-psk',path='/ap'),password='dummy-passphrase')
            call.assert_not_called()

    def test_temporary_wifi_bypasses_the_keyring_and_enterprise_uses_agent_owned_secrets(self):
        service = model.DesktopServices()
        service.properties = lambda *args: {'State':2}
        with patch.object(service,'store_network_secret') as store, patch.object(service,'call',return_value=('/profile','/active',{})) as call:
            service.connect_network('/device',network=dict(ssid=b'Fixture',security='wpa-psk',path='/ap'),password='dummy-passphrase',remember=False)
            store.assert_not_called()
            profile=call.call_args.args[-1][0]
            self.assertEqual(profile['802-11-wireless-security']['psk-flags'].unpack(),2)
            self.assertEqual(call.call_args.args[-1][-1]['persist'].unpack(),'volatile')
        profile=model.wifi_profile(b'Office','enterprise','dummy-password',True,'alice',enterprise=dict(identity='alice',domain='radius.example.org'))
        self.assertEqual(profile['802-1x']['password-flags'],('u',1))
        with self.assertRaises(ValueError):model.wifi_profile(b'Fixture','wpa-psk','bad\0password',True,'alice')

    def test_new_pages_are_searchable(self):
        for term, title in [('unifiedpush','Notifications'),('accent','Appearance'),('cups','Printers'),('locale','Date, Time & Language'),('administrator','Accounts')]:
            self.assertIn(title,[model.PAGES[i][0] for i in model.matching_pages(term)])


if __name__ == '__main__':
    unittest.main()
