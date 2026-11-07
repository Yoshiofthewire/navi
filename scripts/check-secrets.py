#!/usr/bin/env python3
"""Test real oo7/libsecret on a private bus and encrypted disposable vault.

No installed provider, PAM file, real network or user vault is changed. Dummy master
passwords use stdin, never argv. --nm-applet additionally tests the native NM agent
against a fake NetworkManager using libnm’s upstream session-bus test mode.
Exit 77 means an optional prerequisite is missing.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time


def inside(args):
    assert os.environ.get('NAVI_PRIVATE_SECRET_CHECK') == str(args.inside)
    from gi.repository import Gio, GLib
    import gi
    gi.require_version('Secret', '1')
    from gi.repository import Secret
    root = args.inside
    env = dict(os.environ)
    for key, name in [('XDG_CONFIG_HOME','config'),('XDG_DATA_HOME','data'),('XDG_STATE_HOME','state'),
                      ('XDG_CACHE_HOME','cache'),('XDG_RUNTIME_DIR','runtime')]:
        path = root / name
        path.mkdir(mode=0o700, exist_ok=True)
        env[key] = str(path)
    env.update(OO7_PAM_SOCKET=str(root/'runtime/pam.socket'), DBUS_SYSTEM_BUS_ADDRESS='unix:path=/nonexistent',
               GSETTINGS_BACKEND='memory', GTK_A11Y='none', NO_AT_BRIDGE='1')
    env.setdefault('GSETTINGS_SCHEMA_DIR','/usr/share/glib-2.0/schemas')
    env.pop('CREDENTIALS_DIRECTORY', None)
    env.pop('WAYLAND_SOCKET', None)
    env.pop('NAVI_CONTROL_FD', None)
    for key in ('DISPLAY','WAYLAND_DISPLAY'):env.pop(key,None)
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    loop = GLib.MainLoop()
    loop_thread = threading.Thread(target=loop.run, daemon=True)
    loop_thread.start()
    daemon = applet = display = None
    log = (root/'services.log').open('w')
    master = os.urandom(32).hex().encode()
    secret = 'private-dummy-wifi-' + os.urandom(16).hex()
    namespace = {'__name__':'secret_service_check'}
    source = Path(__file__).resolve().parents[1]/'src/settings'
    exec('\n'.join((source/name).read_text() for name in ('model.py','system.py','services.py')), namespace)
    service = namespace['DesktopServices']()

    def call(destination, path, interface, method, parameters=None):
        return bus.call_sync(destination,path,interface,method,parameters,None,Gio.DBusCallFlags.NO_AUTO_START,8000,None)
    def dbus(method,signature,value):
        return call('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus',method,GLib.Variant(signature,value)).unpack()
    def has_owner(name):return dbus('NameHasOwner','(s)',(name,))[0]
    def wait(predicate, description, seconds=10):
        deadline=time.monotonic()+seconds
        while not predicate():
            assert time.monotonic()<deadline,description
            time.sleep(.025)
    def start(password):
        nonlocal daemon
        assert not has_owner('org.freedesktop.secrets')
        # --login bypasses oo7's hard-coded /run/user/<uid> login-helper lookup.
        daemon=subprocess.Popen([str(args.daemon),'--login'],env=env,stdin=subprocess.PIPE,stdout=log,stderr=log)
        daemon.stdin.write(password);daemon.stdin.close()
        wait(lambda: has_owner('org.freedesktop.secrets') or daemon.poll() is not None,'oo7 did not register')
        assert daemon.poll() is None,'oo7 exited'
    def stop():
        nonlocal daemon
        if daemon:
            daemon.terminate();daemon.wait(timeout=5);daemon=None
            wait(lambda:not has_owner('org.freedesktop.secrets'),'oo7 did not release its name')
    def vault():
        owner=dbus('GetNameOwner','(s)',('org.freedesktop.secrets',))[0]
        return Secret.Service.open_sync(Secret.Service.__gtype__,owner,Secret.ServiceFlags.OPEN_SESSION,None)
    def tool(*arguments, input=None):
        result=subprocess.run(['secret-tool',*arguments],env=env,input=input,capture_output=True,timeout=15)
        assert result.returncode==0,'libsecret secret-tool failed'
        return result.stdout
    def assert_rejected_before_nm():
        attempts=[]
        client=namespace['DesktopServices']()
        client.call=lambda *a,**kw:attempts.append(a)
        try:
            client.connect_network('/device',network={'ssid':b'Private fixture','security':'wpa-psk','path':'/ap'},password=secret)
        except RuntimeError as error:
            assert secret not in str(error),'Credential appeared in an error'
        else:raise AssertionError('Unsafe remembered connection was accepted')
        finally:client.close()
        assert not attempts,'NetworkManager was changed before a successful keyring save'

    results={}
    registrations=[]
    try:
        assert not has_owner('org.freedesktop.secrets'),'Private bus already has a provider'
        assert_rejected_before_nm()
        start(master)
        # First exercise the standard client, independently of Navi's Wi-Fi integration.
        tool('store','--label=Disposable Navi check','navi-check','roundtrip',input=secret.encode())
        assert tool('lookup','navi-check','roundtrip').decode().rstrip('\n')==secret
        for path in (root/'data').rglob('*'):
            if path.is_file():assert secret.encode() not in path.read_bytes(),'Plaintext password on disk'
        stop();start(master)
        assert tool('lookup','navi-check','roundtrip').decode().rstrip('\n')==secret,'Secret lost on restart'
        stop()
        try:
            start(b'wrong-disposable-master-password')
        except AssertionError:
            assert daemon.poll()==1, 'Incorrect master password must fail closed cleanly'
            daemon=None
        else:
            # Some supported daemon versions expose the existing collection locked instead.
            path=call('org.freedesktop.secrets','/org/freedesktop/secrets','org.freedesktop.Secret.Service',
                'ReadAlias',GLib.Variant('(s)',('default',))).unpack()[0]
            locked=call('org.freedesktop.secrets',path,'org.freedesktop.DBus.Properties','Get',
                GLib.Variant('(ss)',('org.freedesktop.Secret.Collection','Locked'))).unpack()[0]
            assert locked,'Wrong password unlocked the keyring'
            stop()
        start(master)
        assert tool('lookup','navi-check','roundtrip').decode().rstrip('\n')==secret
        results['wrong_master_password_rejected']=True
        tool('clear','navi-check','roundtrip')
        assert vault().lookup_sync(None,{'navi-check':'roundtrip'},None) is None
        results.update(libsecret_roundtrip=True,encrypted_at_rest=True,restart_persistence=True,delete=True)
        # No agent: fail before the system bus/profile is touched, even with an open vault.
        assert_rejected_before_nm()
        if args.nm_applet:
            # Upstream libnm explicitly supports non-root fake NM on a session bus.
            # Both bus addresses stay private; production never sets this test switch.
            env['DBUS_SYSTEM_BUS_ADDRESS']=os.environ['DBUS_SESSION_BUS_ADDRESS']
            env['LIBNM_USE_SESSION_BUS']='1'
            service._bus=bus
            assert dbus('RequestName','(su)',('org.freedesktop.NetworkManager',4))==(1,)
            nm='org.freedesktop.NetworkManager'
            properties={
                'Version':GLib.Variant('s','1.54.0'),'State':GLib.Variant('u',20),
                'Startup':GLib.Variant('b',False),'NetworkingEnabled':GLib.Variant('b',True),
                'WirelessEnabled':GLib.Variant('b',True),'WirelessHardwareEnabled':GLib.Variant('b',True),
                'Devices':GLib.Variant('ao',[]),'AllDevices':GLib.Variant('ao',[]),
                'ActiveConnections':GLib.Variant('ao',[]),'Connectivity':GLib.Variant('u',0),
                'PrimaryConnection':GLib.Variant('o','/'),'ActivatingConnection':GLib.Variant('o','/'),
                'Metered':GLib.Variant('u',0),'Capabilities':GLib.Variant('au',[])}
            objects={'/org/freedesktop/NetworkManager':{nm:properties},
                     '/org/freedesktop/NetworkManager/Settings':{nm+'.Settings':{'Connections':GLib.Variant('ao',[])}}}
            observed=[]
            def method(connection,sender,path,interface,name,parameters,invocation):
                if name=='GetManagedObjects':invocation.return_value(GLib.Variant('(a{oa{sa{sv}}})',(objects,)))
                elif name=='GetAll':
                    values={'State':GLib.Variant('u',2)} if path.endswith('/ActiveConnection/1') else objects.get(path,{}).get(parameters.unpack()[0],{})
                    invocation.return_value(GLib.Variant('(a{sv})',(values,)))
                elif name=='GetPermissions':invocation.return_value(GLib.Variant('(a{ss})',({},)))
                elif name in ('RegisterWithCapabilities','Register'):
                    registrations.append(sender);invocation.return_value(None)
                elif name=='Unregister':invocation.return_value(None)
                elif name=='AddAndActivateConnection2':
                    observed.append(parameters.unpack()[0]);invocation.return_value(GLib.Variant('(ooa{sv})',('/org/freedesktop/NetworkManager/Settings/1','/org/freedesktop/NetworkManager/ActiveConnection/1',{})))
                else:invocation.return_dbus_error('org.freedesktop.DBus.Error.UnknownMethod','Fixture method unavailable')
            def export(path,xml):
                info=Gio.DBusNodeInfo.new_for_xml('<node>'+xml+'</node>')
                for interface in info.interfaces:bus.register_object(path,interface,method,None,None)
            export('/org/freedesktop', '<interface name="org.freedesktop.DBus.ObjectManager"><method name="GetManagedObjects"><arg type="a{oa{sa{sv}}}" direction="out"/></method></interface>')
            props='<interface name="org.freedesktop.DBus.Properties"><method name="GetAll"><arg type="s" direction="in"/><arg type="a{sv}" direction="out"/></method></interface>'
            export('/org/freedesktop/NetworkManager',props+'<interface name="'+nm+'"><method name="GetPermissions"><arg type="a{ss}" direction="out"/></method><method name="AddAndActivateConnection2"><arg type="a{sa{sv}}" direction="in"/><arg type="o" direction="in"/><arg type="o" direction="in"/><arg type="a{sv}" direction="in"/><arg type="o" direction="out"/><arg type="o" direction="out"/><arg type="a{sv}" direction="out"/></method></interface>')
            export('/org/freedesktop/NetworkManager/Settings',props)
            export('/org/freedesktop/NetworkManager/ActiveConnection/1',props)
            export('/org/freedesktop/NetworkManager/AgentManager','<interface name="'+nm+'.AgentManager"><method name="RegisterWithCapabilities"><arg type="s" direction="in"/><arg type="u" direction="in"/></method><method name="Register"><arg type="s" direction="in"/></method><method name="Unregister"/></interface>')
            assert args.xvfb,'Supply --xvfb for a disposable display'
            read_fd,write_fd=os.pipe()
            display=subprocess.Popen([str(args.xvfb),'-displayfd',str(write_fd),'-nolisten','tcp','-screen','0','1280x800x24','-ac'],env=env,pass_fds=(write_fd,),stdout=log,stderr=log)
            os.close(write_fd)
            with os.fdopen(read_fd) as reader:env['DISPLAY']=':'+reader.readline().strip()
            env['GDK_BACKEND']='x11'
            applet=subprocess.Popen([str(args.nm_applet)],env=env,stdout=log,stderr=log)
            wait(lambda: bool(registrations) or applet.poll() is not None,'Native agent did not register',15)
            assert registrations and applet.poll() is None,'Native agent failed registration; inspect services.log'
            for security in ('wpa-psk','sae','enterprise'):
                enterprise=dict(identity='dummy-user',domain='radius.example.org') if security=='enterprise' else None
                service.connect_network('/org/freedesktop/NetworkManager/Devices/1',network={'ssid':b'Private fixture','security':security,'path':'/'},password=secret,remember=True,enterprise=enterprise)
                profile=observed[-1]
                section,key=('802-1x','password') if enterprise else ('802-11-wireless-security','psk')
                assert profile[section][key+'-flags']==1 and key not in profile[section]
                typed_profile={group:{k:GLib.Variant('s',v) if isinstance(v,str) else GLib.Variant('u',v) if isinstance(v,int) and not isinstance(v,bool) else GLib.Variant('b',v) if isinstance(v,bool) else GLib.Variant('ay',v) if k=='ssid' else GLib.Variant('as',v) for k,v in values.items()} for group,values in profile.items()}
                value=call(registrations[-1],'/org/freedesktop/NetworkManager/SecretAgent',nm+'.SecretAgent','GetSecrets',GLib.Variant('(a{sa{sv}}osasu)',(typed_profile,'/org/freedesktop/NetworkManager/Settings/1',section,[],0))).unpack()[0]
                assert value[section][key]==secret,'Native agent did not retrieve the staged credential'
                call(registrations[-1],'/org/freedesktop/NetworkManager/SecretAgent',nm+'.SecretAgent','DeleteSecrets',GLib.Variant('(a{sa{sv}}o)',(typed_profile,'/org/freedesktop/NetworkManager/Settings/1')))
                attributes={'connection-uuid':profile['connection']['uuid'],'setting-name':section}
                wait(lambda:vault().lookup_sync(None,attributes,None) is None,'Native agent did not delete its credential')
            from unittest.mock import patch
            with patch.object(Secret.Service,'store_sync',return_value=False) as write:
                assert_rejected_before_nm();write.assert_called_once()
            with patch.object(Secret.Service,'store_sync',side_effect=RuntimeError('native error '+secret)):
                assert_rejected_before_nm()
            results.update(native_nm_agent=True,new_wpa2_wpa3_enterprise_passwords_in_oo7=True,native_agent_delete=True,
                           no_password_in_nm_profiles=True,failed_writes_do_not_change_nm=True,credential_errors_redacted=True)
        # A different existing Secret Service must never receive Navi's new network secret.
        stop()
        assert dbus('RequestName','(su)',('org.freedesktop.secrets',4))==(1,)
        assert_rejected_before_nm()
        dbus('ReleaseName','(s)',('org.freedesktop.secrets',))
        results['foreign_provider_rejected']=True
        (root/'results.json').write_text(json.dumps(results,indent=2))
        print('PASS: oo7/libsecret encrypted storage, persistence, deletion, safe provider/agent failures'+('; actual NM agent WPA2/WPA3/enterprise retrieval and deletion' if args.nm_applet else ''))
    finally:
        service.close()
        if applet and applet.poll() is None:applet.terminate();applet.wait(timeout=5)
        if display and display.poll() is None:display.terminate();display.wait(timeout=5)
        stop();loop.quit();loop_thread.join(timeout=5);log.close()


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--daemon',type=Path)
    parser.add_argument('--nm-applet',type=Path)
    parser.add_argument('--xvfb',type=Path)
    parser.add_argument('--output',type=Path)
    parser.add_argument('--inside',type=Path,help=argparse.SUPPRESS)
    args=parser.parse_args()
    if args.inside:return inside(args)
    args.daemon=args.daemon or next((p for p in map(Path,('/usr/lib/oo7-daemon','/usr/libexec/oo7-daemon')) if p.is_file()),None)
    if not args.daemon or not args.daemon.is_file() or not shutil.which('secret-tool') or not shutil.which('dbus-run-session'):
        print('SKIP: install oo7, libsecret/secret-tool and dbus-run-session');return 77
    with tempfile.TemporaryDirectory(prefix='navi-secret-check-') as temporary:
        root=args.output.resolve() if args.output else Path(temporary)
        if args.output:root.mkdir(mode=0o700,parents=True,exist_ok=False)
        (root/'empty').mkdir()
        env=dict(os.environ,NAVI_PRIVATE_SECRET_CHECK=str(root),XDG_DATA_HOME=str(root/'empty'),XDG_DATA_DIRS=str(root/'empty'),XDG_CONFIG_DIRS=str(root/'empty'))
        command=['dbus-run-session','--',sys.executable,str(Path(__file__).resolve()),'--inside',str(root),'--daemon',str(args.daemon.resolve())]
        for key in ('nm_applet','xvfb'):
            if getattr(args,key):command+=['--'+key.replace('_','-'),str(getattr(args,key).resolve())]
        result=subprocess.run(command,env=env,timeout=100)
        if args.output:print('Evidence:',root)
        return result.returncode


if __name__=='__main__':raise SystemExit(main())
