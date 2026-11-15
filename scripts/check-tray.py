#!/usr/bin/env python3
"""A StatusNotifierItem with a dbusmenu, for navi-bar's tray tests.

It registers with navi-bar's watcher by bus name or object path and logs every call it
receives to a JSON-lines file (`GetAll` shows as one read of `Id`). Hostile modes send bad
pixmaps, floods, deep or wide menus and spoofed names. Commands on stdin, one a line:
`exit`, `flood N` (N NewIcon in 1 s), `layout N` (N LayoutUpdated), `status S`, `hang` (every
later `GetAll` answers only after 6 s).
"""
import argparse
import json
import os
import sys
import threading
import time

from gi.repository import Gio, GLib

WATCHER = 'org.kde.StatusNotifierWatcher'
ITEM_PATH = '/StatusNotifierItem'
MENU_PATH = '/MenuBar'

ITEM_XML = """
<node><interface name="org.kde.StatusNotifierItem">
  <property name="Category" type="s" access="read"/>
  <property name="Id" type="s" access="read"/>
  <property name="Title" type="s" access="read"/>
  <property name="Status" type="s" access="read"/>
  <property name="IconName" type="s" access="read"/>
  <property name="IconThemePath" type="s" access="read"/>
  <property name="IconPixmap" type="a(iiay)" access="read"/>
  <property name="ItemIsMenu" type="b" access="read"/>
  %s
  <method name="ProvideXdgActivationToken"><arg type="s" direction="in"/></method>
  <method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
  <method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
  <method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
  <method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method>
  <signal name="NewIcon"/><signal name="NewStatus"><arg type="s"/></signal>
  <signal name="NewTitle"/>
</interface></node>"""

MENU_XML = """
<node><interface name="com.canonical.dbusmenu">
  <property name="Version" type="u" access="read"/>
  <method name="GetLayout">
    <arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/>
    <arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/>
  </method>
  <method name="Event">
    <arg type="i" direction="in"/><arg type="s" direction="in"/>
    <arg type="v" direction="in"/><arg type="u" direction="in"/>
  </method>
  <method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method>
  <signal name="LayoutUpdated"><arg type="u"/><arg type="i"/></signal>
</interface></node>"""


def node(id_, props, children=()):
    """A layout node; `av` holds its children's nodes."""
    props = {k: variant(v) for k, v in props.items()}
    return GLib.Variant('(ia{sv}av)', (id_, props, list(children)))


def variant(v):
    if isinstance(v, bool):
        return GLib.Variant('b', v)
    if isinstance(v, int):
        return GLib.Variant('i', v)
    return GLib.Variant('s', v)


child = node


def deep(labels, levels):
    inner = node(levels + 1, {'label': labels})
    for i in range(levels, 0, -1):
        inner = node(i, {'label': labels, 'children-display': 'submenu'}, [inner])
    return node(0, {}, [inner])


def menu(mode, labels):
    if mode == 'wide':
        return node(0, {}, [child(i + 1, {'label': labels}) for i in range(10000)])
    sub = [child(21, {'label': f'{labels} inner'})]
    return node(0, {'children-display': 'submenu'}, [
        child(1, {'label': f'_Open {labels}'}),
        child(2, {'type': 'separator'}),
        child(3, {'label': 'Disabled', 'enabled': False}),
        child(4, {'label': 'Hidden', 'visible': False}),
        child(5, {'label': 'Wi__Fi', 'toggle-type': 'checkmark', 'toggle-state': 1}),
        child(6, {'label': 'Radio', 'toggle-type': 'radio', 'toggle-state': 1}),
        child(7, {'label': 'More', 'children-display': 'submenu'}, sub),
        child(8, {'label': 7, 'enabled': 'no'}),
        child(9, {'label': 'Quit'}),
    ])


def pixmaps(mode):
    red = bytes([0xFF, 0xFF, 0x00, 0x00])
    entries = {
        'good': [(16, 16, red * 256), (22, 22, red * 484), (48, 48, red * 2304)],
        'huge': [(100000, 100000, red)],
        'negative': [(-4, -4, red * 16)],
        'mismatch': [(4, 4, red * 15)],
        'zero': [(0, 0, b'')],
        'many': [(1, 1, b'\0')] * 1000 + [(22, 22, red * 484)],
        'none': [],
    }[mode]
    return GLib.Variant('a(iiay)', entries)


class Item:
    def __init__(self, args, bus, path):
        self.args, self.bus, self.path = args, bus, path
        self.status = args.status
        self.hang = False
        self.log_lock = threading.Lock()
        nodes = Gio.DBusNodeInfo.new_for_xml(
            ITEM_XML % ('' if args.no_menu else '<property name="Menu" type="o" access="read"/>'))
        bus.register_object(path, nodes.interfaces[0], self.call, self.prop, None)
        if not args.no_menu:
            menu_info = Gio.DBusNodeInfo.new_for_xml(MENU_XML)
            bus.register_object(MENU_PATH, menu_info.interfaces[0], self.call, self.prop, None)

    def log(self, **entry):
        entry['conn'] = self.bus.get_unique_name()
        entry['t'] = time.monotonic()
        entry['wall'] = time.time()
        with self.log_lock, open(self.args.log, 'a') as f:
            f.write(json.dumps(entry) + '\n')

    def prop(self, bus, sender, path, interface, name):
        if name == 'Id':
            self.log(call='GetAll')
            if self.hang:
                time.sleep(6)
        a = self.args
        return {
            'Category': GLib.Variant('s', 'ApplicationStatus'),
            'Id': GLib.Variant('s', a.title + ' id'),
            'Title': GLib.Variant('s', a.title),
            'Status': GLib.Variant('s', self.status),
            'IconName': GLib.Variant('s', a.icon_name),
            'IconThemePath': GLib.Variant('s', a.theme_path),
            'IconPixmap': pixmaps(a.pixmap),
            'ItemIsMenu': GLib.Variant('b', a.item_is_menu),
            'Menu': GLib.Variant('o', MENU_PATH),
            'Version': GLib.Variant('u', 3),
        }.get(name)

    def call(self, bus, sender, path, interface, method, params, invocation):
        self.log(call=method, args=params.unpack())
        if self.args.fail_calls and interface == 'org.kde.StatusNotifierItem':
            return invocation.return_dbus_error('org.navi.Test.Failed', self.args.title)
        if method == 'ProvideXdgActivationToken' and self.args.no_token_method:
            return invocation.return_dbus_error(
                'org.freedesktop.DBus.Error.UnknownMethod', 'no tokens here')
        if method == 'GetLayout':
            if self.args.menu == 'hang':
                self.hung = invocation  # Never answered.
                return
            labels = self.args.title + ' label'
            if self.args.menu == 'deep':
                # D-Bus allows 32 nested arrays and 32 structs: 100 levels can't be sent.
                for levels in (100, 20, 10):
                    try:
                        layout = deep(labels, levels)
                        self.log(depth=levels)
                        return invocation.return_value(GLib.Variant.new_tuple(
                            GLib.Variant('u', 1), layout))
                    except Exception as e:
                        self.log(error=str(e), levels=levels)
            try:
                layout = menu(self.args.menu, labels)
                return invocation.return_value(GLib.Variant.new_tuple(
                    GLib.Variant('u', 1), layout))
            except Exception as e:  # A layout GLib can't send.
                self.log(error=str(e))
                return invocation.return_dbus_error('org.navi.Test.TooDeep', str(e))
        if method == 'AboutToShow':
            return invocation.return_value(GLib.Variant('(b)', (False,)))
        invocation.return_value(None)

    def emit(self, signal, params=None, path=None, interface='org.kde.StatusNotifierItem'):
        self.bus.emit_signal(None, path or self.path, interface, signal, params)


def watcher(bus, method, arg):
    return bus.call_sync(WATCHER, '/StatusNotifierWatcher', WATCHER, method,
                         GLib.Variant('(s)', (arg,)), None, Gio.DBusCallFlags.NO_AUTO_START,
                         5000, None)


def register(item, arg, via=None):
    try:
        watcher(via or item.bus, 'RegisterStatusNotifierItem', arg)
        item.log(register=arg, ok=True)
    except GLib.Error as e:
        item.log(register=arg, ok=False, error=e.message)


def own(bus, name):
    reply = bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus',
                          'org.freedesktop.DBus', 'RequestName',
                          GLib.Variant('(su)', (name, 4)), None, Gio.DBusCallFlags.NONE,
                          5000, None)
    assert reply.unpack()[0] == 1, f'{name} not owned'


def connect():
    return Gio.DBusConnection.new_for_address_sync(
        os.environ['DBUS_SESSION_BUS_ADDRESS'],
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
        | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)


def commands(item, loop):
    for line in sys.stdin:
        words = line.split()
        if not words:
            continue
        if words[0] == 'exit':
            break
        if words[0] == 'flood':
            n = int(words[1])
            item.log(flood=n)
            for i in range(n):
                item.emit('NewIcon')  # From this thread: works while the main loop hangs.
                if i % 100 == 0:
                    time.sleep(0.006)
            item.log(flooded=n)
        elif words[0] == 'layout':
            for _ in range(int(words[1])):
                GLib.idle_add(item.emit, 'LayoutUpdated', GLib.Variant('(ui)', (2, 0)),
                              MENU_PATH, 'com.canonical.dbusmenu')
        elif words[0] == 'hang':
            item.hang = True
        elif words[0] == 'status':
            item.status = words[1]
            GLib.idle_add(item.emit, 'NewStatus', GLib.Variant('(s)', (words[1],)))
        item.log(done=words[0])
    GLib.idle_add(loop.quit)


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--log', required=True)
    p.add_argument('--register', choices=['name', 'path', 'none'], default='name')
    p.add_argument('--twice', action='store_true')
    p.add_argument('--title', default='Tray test')
    p.add_argument('--icon-name', default='')
    p.add_argument('--theme-path', default='')
    p.add_argument('--status', default='Active')
    p.add_argument('--pixmap', default='good')
    p.add_argument('--menu', choices=['normal', 'wide', 'deep', 'hang'], default='normal')
    p.add_argument('--no-menu', action='store_true')
    p.add_argument('--item-is-menu', action='store_true')
    p.add_argument('--no-token-method', action='store_true')
    p.add_argument('--fail-calls', action='store_true',
                   help='item methods fail with the title as the error text')
    p.add_argument('--no-object', action='store_true', help='register a path with nothing there')
    p.add_argument('--spoof', help='register this bus name, owned by someone else')
    p.add_argument('--fill', action='store_true', help='9 connections: 5 items, then 4 each')
    p.add_argument('--split', action='store_true',
                   help='own the name on a second connection, as KStatusNotifierItem does')
    args = p.parse_args()
    loop = GLib.MainLoop()
    bus = connect()
    if args.fill:
        items = []
        for c in range(9):
            conn = bus if c == 0 else connect()
            for i in range(5 if c == 0 else 4):
                path = f'/org/navi/Fill{i}'
                items.append(Item(args, conn, path))
                register(items[-1], path)
        item = items[0]
    elif args.spoof:
        item = Item(args, bus, ITEM_PATH)
        register(item, args.spoof)
    elif args.no_object:
        item = Item.__new__(Item)
        item.args, item.bus, item.path, item.log_lock = args, bus, '/Nothing', threading.Lock()
        register(item, '/Nothing')
    else:
        path = ITEM_PATH if args.register == 'name' else '/org/navi/TestItem'
        item = Item(args, connect() if args.split else bus, path)
        name = f'org.kde.StatusNotifierItem-{os.getpid()}-1'
        own(item.bus, name)
        item.log(name=name)
        arg = name if args.register == 'name' else path
        if args.register != 'none':
            for _ in range(2 if args.twice else 1):
                register(item, arg, bus)
    threading.Thread(target=commands, args=(item, loop), daemon=True).start()
    loop.run()


if __name__ == '__main__':
    main()
