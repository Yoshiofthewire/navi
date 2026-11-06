#!/usr/bin/env python3
"""Exercise real shell settings and the packaged GTK launcher in a disposable nested Navi.

Requires a host X11/XWayland display, dbus-run-session and a debug build. The child has
private XDG roots, a private session bus and an unavailable system bus. No host devices,
credentials or sessions are changed. Screenshots contain only Navi's own output.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path)
    parser.add_argument('--inside',type=Path,help=argparse.SUPPRESS)
    args=parser.parse_args()
    repo=Path(__file__).resolve().parents[1]
    if not args.inside:
        root=args.output or Path(tempfile.mkdtemp(prefix='navi-shell-settings-'))
        root.mkdir(parents=True,exist_ok=True)
        env=dict(os.environ,NAVI_PRIVATE_SHELL_CHECK=str(root))
        return subprocess.run(['dbus-run-session','--',sys.executable,str(Path(__file__).resolve()),'--inside',str(root)],env=env).returncode
    root=args.inside
    assert os.environ.get('NAVI_PRIVATE_SHELL_CHECK')==str(root),'Use the public entrypoint to create a private bus'
    assert root.is_absolute() and root.is_relative_to('/tmp'),'Use a disposable /tmp output directory'
    for name in ['runtime','config','state','data','cache']:
        (root/name).mkdir(mode=0o700,exist_ok=True)
    env=dict(os.environ,DISPLAY=os.environ.get('DISPLAY',':0'),WINIT_UNIX_BACKEND='x11',
        DBUS_SYSTEM_BUS_ADDRESS='unix:path=/nonexistent',XDG_RUNTIME_DIR=str(root/'runtime'),
        XDG_CONFIG_HOME=str(root/'config'),XDG_DATA_HOME=str(root/'data'),XDG_STATE_HOME=str(root/'state'),
        XDG_CACHE_HOME=str(root/'cache'),NAVI_SCREENSHOT=str(root/'desktop.png'))
    for name in ['WAYLAND_DISPLAY','WAYLAND_SOCKET','NAVI_INPUT_REPLAY']:env.pop(name,None)
    target=Path(os.environ.get('CARGO_TARGET_DIR',str(repo/'target')))
    if not (target/'debug/navi').is_file():target=Path('/home/yoshi/git/navi/target')
    navi=wrapper=None
    def wait(test,description,seconds=12):
        end=time.monotonic()+seconds
        while time.monotonic()<end:
            if test():return
            if navi.poll() is not None:raise RuntimeError('Navi exited: '+(root/'navi.log').read_text()[-2000:])
            time.sleep(.1)
        raise RuntimeError('Timed out: '+description)
    try:
        with (root/'navi.log').open('w') as log:
            navi=subprocess.Popen([str(target/'debug/navi')],env=env,stdout=log,stderr=log,start_new_session=True)
        wait(lambda: re.search(r'listening on WAYLAND_DISPLAY=(wayland-\d+)',(root/'navi.log').read_text()),'nested socket')
        display=re.search(r'listening on WAYLAND_DISPLAY=(wayland-\d+)',(root/'navi.log').read_text())[1]
        env['WAYLAND_DISPLAY']=display
        for name in ['XDG_RUNTIME_DIR','XDG_CONFIG_HOME','XDG_STATE_HOME','XDG_DATA_HOME','XDG_CACHE_HOME','WAYLAND_DISPLAY']:
            os.environ[name]=env[name]
        spec=importlib.util.spec_from_file_location('navi_check',repo/'src/settings/model.py')
        model=importlib.util.module_from_spec(spec);spec.loader.exec_module(model)
        exec((repo/'src/settings/system.py').read_text(),model.__dict__)
        from gi.repository import Gio,GLib
        bus=Gio.bus_get_sync(Gio.BusType.SESSION,None)
        def get():return model.shell_request('get')
        wait(lambda: (root/'runtime'/('navi-'+display+'.settings')).exists(),'settings socket')
        time.sleep(1)
        baseline=model.request('get')['desktop']
        assert baseline['push_enabled'] is False
        assert get()['push']['owned'] is False
        def save(**values):
            current=model.request('get')['desktop'];current.update(values)
            model.request('desktop',settings=current)
            wait(lambda: all(get()['settings'][k]==v for k,v in values.items()),'shell preference reload')
        def notify(title,app='org.navi.Integration'):
            return bus.call_sync('org.freedesktop.Notifications','/org/freedesktop/Notifications','org.freedesktop.Notifications','Notify',
                GLib.Variant('(susssasa{sv}i)',('Navi integration',0,'',title,'<b>Plain text body</b>',['default','Open'],{'desktop-entry':GLib.Variant('s',app)},10000)),None,Gio.DBusCallFlags.NO_AUTO_START,5000,None).unpack()[0]
        isolation_spec=importlib.util.spec_from_file_location('isolation_check',repo/'scripts/check-notification-isolation.py')
        isolation=importlib.util.module_from_spec(isolation_spec);isolation_spec.loader.exec_module(isolation)
        isolation.check(bus,'io.github.Yoshiofthewire.Navi.Shell.d'+display.encode().hex()[:160])
        first=notify('Normal notification')
        note=next(n for n in get()['history'] if n['id']==first)
        assert note['body']=='Plain text body' and note['actions']==[['default','Open']]
        received=[]
        bus.signal_subscribe(None,'org.freedesktop.Notifications','ActionInvoked','/org/freedesktop/Notifications',None,Gio.DBusSignalFlags.NONE,
            lambda *args:received.append(args[-2].unpack()),None)
        model.shell_request('action',id=first,key='default')
        end=time.monotonic()+3
        while not received and time.monotonic()<end:GLib.MainContext.default().iteration(False);time.sleep(.01)
        assert (first,'default') in received,received
        save(dnd=True)
        second=notify('Do Not Disturb')
        assert next(n for n in get()['history'] if n['id']==second)['actions']==[]
        save(dnd=False,blocked_apps=['org.navi.Blocked'])
        blocked=notify('Blocked notification','org.navi.Blocked')
        assert all(n['id']!=blocked for n in get()['history'])
        save(history=False)
        notify('History disabled')
        assert get()['history']==[]
        save(history=True,push_enabled=True)
        wait(lambda:get()['push']['state']=='Unavailable in nested Navi','nested push guard')
        assert not get()['push']['owned']
        save(push_enabled=False,scheme='light',accent='purple')
        assert model.request('get')['desktop']['scheme']=='light'
        model.shell_request('clear');assert get()['history']==[]
        config=root/'config/navi/desktop.json'
        good=config.read_bytes();config.write_text('{broken')
        try:
            try:model.request('desktop',settings=baseline)
            except RuntimeError:pass
            else:raise AssertionError('Malformed configuration was overwritten')
            assert config.read_text()=='{broken'
        finally:config.write_bytes(good)
        with (root/'settings.log').open('w') as log:
            wrapper=subprocess.Popen([str(target/'debug/navi-settings')],env=env,stdout=log,stderr=log)
        time.sleep(4)
        assert wrapper.poll() is None,(root/'settings.log').read_text()
        assert 'Traceback' not in (root/'settings.log').read_text()
        if (root/'desktop.png').exists():shutil.copyfile(root/'desktop.png',root/'packaged-settings.png')
        model.request('desktop',settings=baseline)
        print('PASS: real notifications, actions, DND, blocking, history, nested push guard, malformed-config preservation and packaged Settings startup')
        print('Evidence:',root)
        return 0
    finally:
        if wrapper and wrapper.poll() is None:wrapper.terminate();wrapper.wait(timeout=5)
        if navi and navi.poll() is None:
            os.killpg(navi.pid,signal.SIGTERM)
            try:navi.wait(timeout=5)
            except subprocess.TimeoutExpired:os.killpg(navi.pid,signal.SIGKILL);navi.wait()


if __name__=='__main__':raise SystemExit(main())
