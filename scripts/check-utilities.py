#!/usr/bin/env python3
"""Real GTK module and Thunar operations in a private bus, Xvfb and nested Navi.

Requires debug binaries, GTK 3 GI, Thunar and Xvfb. Host configuration, devices,
credentials and MIME defaults are never changed. Captures contain only Navi.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def probe(output):
    import gi
    gi.require_version('Gtk', '3.0')
    from gi.repository import Gtk, GLib
    window = Gtk.Window(title='Navi utility theme check')
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12, margin=24)
    label = Gtk.Label(label='Native utility controls')
    box.pack_start(label, False, False, 0)
    box.pack_start(Gtk.Entry(text='Folder name'), False, False, 0)
    box.pack_start(Gtk.Button(label='Apply'), False, False, 0)
    box.pack_start(Gtk.CheckButton(label='Show hidden files', active=True), False, False, 0)
    box.pack_start(Gtk.RadioButton(label='Use icon view', active=True), False, False, 0)
    box.pack_start(Gtk.Switch(active=True, halign=Gtk.Align.START), False, False, 0)
    window.add(box)
    window.show_all()
    def sample():
        settings = Gtk.Settings.get_default()
        color = window.get_style_context().get_background_color(Gtk.StateFlags.NORMAL)
        result = dict(background=[round(color.red*255), round(color.green*255), round(color.blue*255)],
                      font=label.get_style_context().get_property('font', Gtk.StateFlags.NORMAL).get_size()*4/3/1024,
                      icons=settings.get_property('gtk-icon-theme-name'), cursor=settings.get_property('gtk-cursor-theme-name'),
                      cursor_size=settings.get_property('gtk-cursor-theme-size'),
                      controls=settings.get_property('gtk-decoration-layout'))
        tmp = output.with_suffix('.tmp'); tmp.write_text(json.dumps(result)); tmp.replace(output)
        return True
    GLib.timeout_add(100, sample)
    GLib.timeout_add_seconds(90, lambda: (Gtk.main_quit(), False)[1])
    Gtk.main()


def inside(args):
    root = args.inside
    assert os.environ.get('NAVI_PRIVATE_UTILITY_CHECK') == str(root)
    assert root.is_absolute()
    repo = Path(__file__).resolve().parents[1]
    target = args.target.resolve()
    env = dict(os.environ, DBUS_SYSTEM_BUS_ADDRESS='unix:path=/nonexistent', GSETTINGS_BACKEND='memory',
               GTK_A11Y='none', NO_AT_BRIDGE='1', WINIT_UNIX_BACKEND='x11', RUST_LOG='warn,navi=info')
    for key in ['WAYLAND_DISPLAY','WAYLAND_SOCKET','NAVI_INPUT_REPLAY','GTK_MODULES','GTK_THEME','NAVI_UTILITY_APPEARANCE']:
        env.pop(key, None)
    for key, name in [('XDG_CONFIG_HOME','config'),('XDG_DATA_HOME','data'),('XDG_STATE_HOME','state'),
                      ('XDG_CACHE_HOME','cache'),('XDG_RUNTIME_DIR','runtime'),('HOME','home')]:
        (root/name).mkdir(mode=0o700, exist_ok=True); env[key] = str(root/name)
    os.environ.update(env)
    from gi.repository import Gio, GLib
    env['NAVI_SCREENSHOT'] = str(root/'native.png')
    children = []
    log = (root/'services.log').open('w')
    def start(command, child_env=env):
        child = subprocess.Popen(command, env=child_env, stdout=log, stderr=log, start_new_session=True)
        children.append(child); return child
    def wait(test, description, seconds=12):
        end = time.monotonic()+seconds
        while time.monotonic()<end:
            if test(): return
            time.sleep(.05)
        raise AssertionError(description+'; inspect '+str(root/'services.log'))
    import re
    def log_text():
        return re.sub(r'\x1b\[[0-9;]*m', '', (root/'services.log').read_text())
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    def call(name, path, interface, method, signature=None, values=()):
        return bus.call_sync(name,path,interface,method,GLib.Variant(signature,values) if signature else None,
                             None,Gio.DBusCallFlags.NO_AUTO_START,8000,None)
    def owned(name):
        return call('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus','NameHasOwner','(s)',(name,)).unpack()[0]
    def thunar(method, signature, values):
        interface = 'org.xfce.Trash' if method=='MoveToTrash' else ('org.xfce.Thunar' if method=='Terminate' else 'org.xfce.FileManager')
        return call('org.xfce.Thunar','/org/xfce/FileManager',interface,method,signature,values)
    try:
        r,w = os.pipe()
        xvfb = subprocess.Popen([args.xvfb,'-displayfd',str(w),'-screen','0','1280x800x24','-nolisten','tcp'],
                                env=env, pass_fds=(w,), stdout=log, stderr=log, start_new_session=True)
        children.append(xvfb); os.close(w)
        import select
        assert select.select([r],[],[],10)[0], 'Xvfb did not provide a display'
        env['DISPLAY'] = ':'+os.read(r,32).decode().strip(); os.close(r)
        if args.xfconfd: start([args.xfconfd])
        navi = start([str(target/'navi')])
        import re
        def display():
            text=log_text()
            return re.search(r'listening on WAYLAND_DISPLAY=(wayland-\d+)',text)
        wait(display,'Navi did not start')
        client = dict(env, WAYLAND_DISPLAY=display()[1], DISPLAY='', GDK_BACKEND='wayland')
        # Only this private bus: activated Xfconf/GVfs/portal helpers use the disposable roots too.
        activation = {key:client[key] for key in ('HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_STATE_HOME',
                      'XDG_CACHE_HOME','XDG_RUNTIME_DIR','DISPLAY','WAYLAND_DISPLAY','GDK_BACKEND',
                      'DBUS_SYSTEM_BUS_ADDRESS','GSETTINGS_BACKEND','GTK_A11Y','NO_AT_BRIDGE')}
        call('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus',
             'UpdateActivationEnvironment','(a{ss})',(activation,))
        appearance = root/'runtime'/('navi-'+client['WAYLAND_DISPLAY']+'.utility/appearance.ini')
        wait(appearance.exists,'No utility appearance file')
        assert appearance.stat().st_mode & 0o777 == 0o600
        os.environ.update({k:client[k] for k in ('XDG_RUNTIME_DIR','WAYLAND_DISPLAY')})
        spec=importlib.util.spec_from_file_location('utility_model',repo/'src/settings/model.py')
        model=importlib.util.module_from_spec(spec); spec.loader.exec_module(model)
        def save(**values):
            settings=model.request('get')['desktop']; settings.update(values)
            model.request('desktop',settings=settings)
        probe_output=root/'probe.json'
        widget=start([sys.executable,str(Path(__file__).resolve()),'--probe',str(probe_output)],
                     dict(client,GTK_MODULES=str(target/'libnavi-gtk3.so'),NAVI_UTILITY_APPEARANCE=str(appearance)))
        wait(probe_output.exists,'Native GTK module probe did not start')
        variants=[('dark',dict(scheme='dark',accent='blue',icon_theme='Adwaita',cursor_theme='Adwaita',cursor_size=32,font_size=14,high_contrast=False),[11,13,18]),
                  ('light',dict(scheme='light',accent='purple'),[245,246,248]),
                  ('contrast',dict(scheme='dark',accent='green',high_contrast=True,font_size=20),[0,0,0])]
        results={}
        for name,changes,color in variants:
            save(**changes)
            wait(lambda:json.loads(probe_output.read_text())['background']==color and json.loads(probe_output.read_text())['cursor_size']==32,'Live '+name+' palette did not apply')
            value=json.loads(probe_output.read_text())
            assert value['icons']=='Adwaita' and value['cursor']=='Adwaita' and value['cursor_size']==32, value
            assert value['controls']==':minimize,maximize,close',value
            wait(lambda:json.loads(probe_output.read_text())['font']==changes.get('font_size',14),'Live font size did not apply')
            results[name]=json.loads(probe_output.read_text())
        widget.terminate(); widget.wait(timeout=5)
        save(scheme='dark',accent='blue',high_contrast=False,font_size=14)
        folder=root/'home/Work Files'; folder.mkdir(exist_ok=True)
        (root/'home/Desktop').mkdir(exist_ok=True)
        source=folder/'Notes 日本語 with spaces.txt'; source.write_text('Private Navi file operation check.\n')
        destination=folder/'Projects'; destination.mkdir(exist_ok=True)
        files=start([str(target/'navi-files'),str(folder)],client)
        wait(lambda:owned('org.xfce.Thunar'),'Navi Files did not start Thunar')
        time.sleep(2)
        assert files.poll() is None
        import gi
        gi.require_version('GdkPixbuf', '2.0')
        from gi.repository import GdkPixbuf
        geometry=re.search(r'placed app="thunar".*?Rect \{ x: (\d+), y: (\d+), w: (\d+), h: (\d+)',log_text())
        assert geometry,'No Thunar geometry'
        x,y,width,height=map(int,geometry.groups())
        def content_color():
            image=GdkPixbuf.Pixbuf.new_from_file(str(root/'native.png'))
            offset=(y+height*3//4)*image.get_rowstride()+(x+width*3//4)*image.get_n_channels()
            return list(image.get_pixels()[offset:offset+3])
        for name,changes,color in variants:
            save(**changes)
            wait(lambda:content_color()==color,'Thunar did not render the live '+name+' palette')
            shutil.copyfile(root/'native.png',root/('files-'+name+'.png'))
        copy=folder/'Copied.txt'
        thunar('CopyTo','(sasasss)',(str(folder),[str(source)],[str(copy)],'',''))
        wait(copy.exists,'Thunar copy failed'); assert copy.read_bytes()==source.read_bytes()
        thunar('MoveInto','(sassss)',(str(folder),[str(copy)],str(destination),'',''))
        moved=destination/copy.name
        wait(lambda:moved.exists() and not copy.exists(),'Thunar move failed')
        thunar('LinkInto','(sassss)',(str(folder),[str(source)],str(destination),'',''))
        wait(lambda:(destination/source.name).is_symlink(),'Thunar symlink failed')
        thunar('MoveToTrash','(asss)',([str(moved)],'',''))
        wait(lambda:not moved.exists(),'Thunar trash failed')
        wait(lambda:any((root/'data/Trash/files').glob('Copied*')),'File was not moved to private trash')
        # A real MIME handler, invoked by Thunar, records argv in private test storage.
        appdir=root/'data/applications';appdir.mkdir(exist_ok=True)
        handler=root/'open-file.py'
        handler.write_text('import sys,json\nfrom pathlib import Path\nPath('+repr(str(root/'opened.json'))+').write_text(json.dumps(sys.argv[1:]))\n')
        desktop=appdir/'navi-test-handler.desktop'
        desktop.write_text('[Desktop Entry]\nType=Application\nName=Navi Test Editor\nExec='+sys.executable+' '+str(handler)+' %U\nMimeType=text/plain;\n')
        # These settings affect only this test process and its disposable config/data roots.
        for key in ('XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_DATA_DIRS'):
            if key in client:os.environ[key]=client[key]
        info=Gio.DesktopAppInfo.new_from_filename(str(desktop)); assert info
        assert info.set_as_default_for_type('text/plain')
        thunar('LaunchFiles','(sasss)',(str(folder),[str(source)],'',''))
        wait(lambda:(root/'opened.json').exists(),'Thunar did not use the chosen MIME handler')
        assert str(source) in json.loads((root/'opened.json').read_text())[0]
        again=subprocess.run([str(target/'navi-files'),str(root/'home/Desktop')],env=client,stdout=log,stderr=log,timeout=10)
        assert again.returncode==0,'A second Files launch did not reuse its own themed instance'
        thunar('Terminate','()',())
        wait(lambda:files.poll() is not None,'Thunar did not exit')
        wait(lambda:not owned('org.xfce.Thunar'),'Thunar retained its bus name')
        call('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus','RequestName','(su)',('org.xfce.Thunar',4))
        foreign=start([str(target/'navi-files')],client)
        wait(lambda:'already running outside' in log_text(),'Foreign Thunar owner was not refused')
        assert foreign.poll() is None,'Foreign owner error was not visible'
        wait(lambda:log_text().count('placed app="io.github.Yoshiofthewire.Navi.Files.Error"')==1,'Foreign owner error did not render')
        time.sleep(2);shutil.copyfile(root/'native.png',root/'foreign-owner.png')
        foreign.terminate();foreign.wait(timeout=5)
        call('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus','ReleaseName','(s)',('org.xfce.Thunar',))
        missing=start([str(target/'navi-files')],dict(client,PATH='/nonexistent'))
        wait(lambda:'Install Thunar;' in log_text(),'Missing Thunar recovery was not shown')
        assert missing.poll() is None,'Missing app error was not visible'
        wait(lambda:log_text().count('placed app="io.github.Yoshiofthewire.Navi.Files.Error"')==2,'Missing app error did not render')
        time.sleep(2);shutil.copyfile(root/'native.png',root/'missing-thunar.png')
        missing.terminate();missing.wait(timeout=5)
        results['file_operations']=['copy','move','symlink','trash','MIME open','Desktop folder','second launch']
        results['visible_recovery']=['foreign Thunar owner','missing Thunar']
        results['core_has_no_gtk_dependency']='libgtk' not in subprocess.check_output(['ldd',str(target/'navi')],text=True)
        assert results['core_has_no_gtk_dependency']
        (root/'results.json').write_text(json.dumps(results,indent=2))
        print('PASS: native live palette/font/icons/cursor/window controls and real Thunar file operations; no host state changed')
    finally:
        for child in reversed(children):
            if child.poll() is None:
                os.killpg(child.pid,signal.SIGTERM)
                try:child.wait(timeout=4)
                except subprocess.TimeoutExpired:os.killpg(child.pid,signal.SIGKILL);child.wait()
        log.close()


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target',type=Path,default=Path(os.environ.get('CARGO_TARGET_DIR','target'))/'debug')
    parser.add_argument('--xvfb',default='Xvfb')
    parser.add_argument('--xfconfd')
    parser.add_argument('--output',type=Path)
    parser.add_argument('--inside',type=Path,help=argparse.SUPPRESS)
    parser.add_argument('--probe',type=Path,help=argparse.SUPPRESS)
    args=parser.parse_args()
    if args.probe:return probe(args.probe)
    if args.inside:return inside(args)
    root=(args.output or Path(tempfile.mkdtemp(prefix='navi-utilities-'))).resolve()
    root.mkdir(mode=0o700,parents=True,exist_ok=True)
    if any(root.iterdir()):parser.error('use a fresh, empty output directory')
    command=['dbus-run-session','--',sys.executable,str(Path(__file__).resolve()),'--inside',str(root),'--target',str(args.target.resolve()),'--xvfb',args.xvfb]
    if args.xfconfd:command+=['--xfconfd',args.xfconfd]
    return subprocess.run(command,env=dict(os.environ,NAVI_PRIVATE_UTILITY_CHECK=str(root))).returncode


if __name__=='__main__':raise SystemExit(main())
