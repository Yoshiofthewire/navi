def run_settings():
    import sys
    os.environ.setdefault("GSK_RENDERER", "cairo")
    try:
        import gi
        gi.require_version("Gtk", "4.0")
        gi.require_version("Gdk", "4.0")
        from gi.repository import Gtk, Gdk, Gio, GLib
    except (ImportError, ValueError) as error:
        print("Navi Settings needs Python 3, PyGObject and GTK 4. See Readme.md.", file=sys.stderr)
        return 1

    class Settings(Gtk.Application):
        def __init__(self):
            super().__init__(application_id="io.github.Yoshiofthewire.Navi.Settings", flags=Gio.ApplicationFlags.NON_UNIQUE)
            self.pool = concurrent.futures.ThreadPoolExecutor(max_workers=2)
            self.services = DesktopServices()
            self.busy = False
            self.pair_dialog = None
            self.pair_invocation = None
            self.closing_pair_prompt = False
            self.pair_kind = None
            self.snapshot = None
            self.token = None
            self.generation = 0
            self.page = 0
            self.connect("activate", self.activate_window)
            self.connect("shutdown", lambda _: self.pool.shutdown(wait=False, cancel_futures=True))

        def activate_window(self, _):
            self.window = Gtk.ApplicationWindow(application=self, title="Navi Settings", default_width=1000, default_height=700)
            self.window.set_size_request(360, 360)
            self.window.connect("close-request", self.close_window)
            Gtk.Settings.get_default().set_property("gtk-application-prefer-dark-theme", True)
            Gtk.Settings.get_default().set_property("gtk-enable-animations", False)
            provider = Gtk.CssProvider()
            css = """
                window { background: #0b0d12; color: #d0d4dc; font-family: 'Noto Sans', sans-serif; }
                headerbar, .sidebar { background: #151922; }
                .sidebar { padding: 12px; border-right: 1px solid #303847; }
                .sidebar row { padding: 10px 8px; border-radius: 6px; min-height: 24px; }
                .sidebar row:selected { background: #262c38; color: #d0d4dc; }
                .page { padding: 24px; }
                .title { font-size: 24px; font-weight: 600; }
                .section { font-size: 17px; font-weight: 600; margin-top: 12px; }
                .muted { color: #a4adbd; }
                .error { color: #ff9aa6; }
                .success { color: #5bd6a2; }
                .setting-row { padding: 12px 0; border-bottom: 1px solid #303847; }
                button, entry, spinbutton, dropdown { min-height: 32px; }
                button.suggested-action:not(:disabled), button.suggested-action:not(:disabled):backdrop { background: #2ba3d8; color: #0b0d12; }
                button.suggested-action:not(:disabled) label, button.suggested-action:not(:disabled):backdrop label { color: #0b0d12; }
                button.suggested-action:disabled, button.suggested-action:disabled label { background: #262c38; color: #a4adbd; }
                button:focus-visible, entry:focus, spinbutton:focus-within, dropdown:focus-visible { outline: 2px solid #2ba3d8; outline-offset: 2px; }
                selection { background: #2ba3d8; color: #0b0d12; }
            """
            for old, new in json.loads(os.environ.get("NAVI_SETTINGS_COLORS", "{}")).items():
                css = css.replace(old, new)
            provider.load_from_data(css.encode())
            Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
            header = Gtk.HeaderBar()
            header.set_title_widget(Gtk.Label(label="Navi Settings"))
            refresh = Gtk.Button(icon_name="view-refresh-symbolic", tooltip_text="Refresh settings")
            refresh.connect("clicked", lambda _: self.refresh())
            self.name_control(refresh, "Refresh settings")
            header.pack_end(refresh)
            self.window.set_titlebar(header)
            body = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
            self.window.set_child(body)
            sidebar = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
            sidebar.add_css_class("sidebar")
            sidebar.set_size_request(215, -1)
            self.search = Gtk.SearchEntry(placeholder_text="Search settings")
            self.search.connect("search-changed", self.filter_pages)
            self.name_control(self.search, "Search settings")
            sidebar.append(self.search)
            self.nav = Gtk.ListBox(selection_mode=Gtk.SelectionMode.SINGLE)
            self.rows = []
            for name, icon, _ in PAGES:
                box = Gtk.Box(spacing=10)
                box.append(Gtk.Image.new_from_icon_name(icon))
                box.append(Gtk.Label(label=name, xalign=0))
                self.nav.append(box)
                self.rows.append(box.get_parent())
            self.nav.connect("row-selected", self.selected)
            sidebar.append(self.nav)
            self.empty = Gtk.Label(label="No matching settings", wrap=True)
            self.empty.add_css_class("muted")
            self.empty.set_visible(False)
            sidebar.append(self.empty)
            sidebar_scroll = Gtk.ScrolledWindow(hscrollbar_policy=Gtk.PolicyType.NEVER, child=sidebar)
            self.sidebar_scroll = sidebar_scroll
            body.append(sidebar_scroll)
            right = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, hexpand=True)
            body.append(right)
            self.compact = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8, margin_start=16, margin_end=16, margin_top=12)
            self.compact_search = Gtk.SearchEntry(placeholder_text="Search settings")
            self.compact_search.connect("search-changed", lambda entry: self.search.set_text(entry.get_text()))
            self.name_control(self.compact_search, "Search settings")
            self.compact.append(self.compact_search)
            self.compact_indices = list(range(len(PAGES)))
            self.compact_select = Gtk.DropDown.new_from_strings([p[0] for p in PAGES])
            category_label = Gtk.Label(label="Settings category", xalign=0)
            category_label.set_mnemonic_widget(self.compact_select)
            self.compact.append(category_label)
            self.name_control(self.compact_select, "Settings category")
            self.compact_select.connect("notify::selected", self.compact_selected)
            self.compact.append(self.compact_select)
            self.compact_empty = Gtk.Label(label="No matching settings", xalign=0)
            self.compact_empty.set_visible(False)
            self.compact.append(self.compact_empty)
            self.compact.set_visible(False)
            right.append(self.compact)
            GLib.timeout_add(200, self.responsive)
            self.notice = Gtk.Label(xalign=0, wrap=True, margin_start=24, margin_end=24, margin_top=12)
            self.notice.add_css_class("muted")
            right.append(self.notice)
            self.scroll = Gtk.ScrolledWindow(vexpand=True, hscrollbar_policy=Gtk.PolicyType.NEVER)
            right.append(self.scroll)
            self.nav.select_row(self.rows[0])
            self.window.present()
            self.refresh()

        def responsive(self):
            compact = 0 < self.window.get_width() < 760
            if self.compact.get_visible() != compact:
                self.compact.set_visible(compact)
                self.sidebar_scroll.set_visible(not compact)
            return True

        def compact_selected(self, widget, _):
            if getattr(self, "updating_compact", False):
                return
            index = widget.get_selected()
            if index < len(self.compact_indices):
                self.nav.select_row(self.rows[self.compact_indices[index]])

        def close_window(self, _):
            self.cancel_pair_prompt()
            self.services.close()
            # The compositor's timer also restores the layout if this process dies.
            if self.token is not None:
                self.pool.submit(request, "revert", token=self.token)
            return False

        def work(self, job, done):
            future = self.pool.submit(job)
            def completed(f):
                try:
                    result, error = f.result(), None
                except Exception as e:
                    result, error = None, str(e)
                GLib.idle_add(done, result, error)
            future.add_done_callback(completed)

        def show_notice(self, message, error=False):
            self.notice.set_text(message)
            self.notice.remove_css_class("error")
            if error:
                self.notice.add_css_class("error")

        def refresh(self):
            self.show_notice("Connecting to Navi…")
            def ready(result, error):
                if error:
                    self.snapshot = None
                    self.show_notice(error, True)
                else:
                    self.snapshot = result
                    self.show_notice("Nested session · Navi changes affect this window. Network, Bluetooth and audio changes affect the computer." if result["nested"] else "Connected to your Navi session")
                self.render()
            self.work(lambda: request("get"), ready)

        def filter_pages(self, entry):
            visible = matching_pages(entry.get_text())
            for i, row in enumerate(self.rows):
                row.set_visible(i in visible)
            self.empty.set_visible(not visible)
            self.updating_compact = True
            self.compact_indices = visible
            self.compact_select.set_model(Gtk.StringList.new([PAGES[i][0] for i in visible]))
            self.compact_select.set_sensitive(bool(visible))
            if self.page in visible:
                self.compact_select.set_selected(visible.index(self.page))
            self.compact_empty.set_visible(not visible)
            self.compact_search.set_tooltip_text("No matching settings" if not visible else "Search settings")
            self.updating_compact = False
            if visible and self.page not in visible:
                self.nav.select_row(self.rows[visible[0]])

        def selected(self, _, row):
            if row:
                previous = self.page
                self.page = self.rows.index(row)
                if self.page in self.compact_indices:
                    self.updating_compact = True
                    self.compact_select.set_selected(self.compact_indices.index(self.page))
                    self.updating_compact = False
                if previous == 3 and self.page != 3:
                    for adapter in list(self.services.discovering):
                        self.work(lambda a=adapter: self.services.discover(a, False), lambda *_: None)
                self.render()

        def label(self, text, style=None):
            label = Gtk.Label(label=text, xalign=0, wrap=True, selectable=True)
            if style:
                label.add_css_class(style)
            return label

        def name_control(self, widget, name):
            def apply_name(control):
                if isinstance(control, Gtk.Button):
                    control.reset_relation(Gtk.AccessibleRelation.LABELLED_BY)
                control.update_property([Gtk.AccessibleProperty.LABEL], [name])
            apply_name(widget)
            widget.connect("map", apply_name)
            widget._navi_accessible_named = True

        def row_names(self, widget, name):
            if getattr(widget, "_navi_accessible_named", False) or isinstance(widget, Gtk.Label):
                return
            if isinstance(widget, Gtk.Button):
                title = widget.get_label() or widget.get_tooltip_text()
                self.name_control(widget, (title + ": " + name) if title else name)
            elif isinstance(widget, (Gtk.Switch, Gtk.SpinButton, Gtk.DropDown, Gtk.Scale, Gtk.Entry, Gtk.PasswordEntry, Gtk.AppChooserButton)):
                self.name_control(widget, name)
            else:
                child = widget.get_first_child()
                while child:
                    self.row_names(child, name)
                    child = child.get_next_sibling()

        def row(self, name, widget=None, detail=None, context=None):
            row = Gtk.Box(spacing=16)
            row.add_css_class("setting-row")
            labels = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4, hexpand=True)
            row_label = self.label(name + (" · " + context if context else ""))
            labels.append(row_label)
            if widget and not isinstance(widget, (Gtk.Box, Gtk.Button, Gtk.Label)):
                row_label.set_mnemonic_widget(widget)
            if detail:
                labels.append(self.label(detail, "muted"))
            row.append(labels)
            if widget:
                self.row_names(widget, name + (": " + context if context else ""))
                widget.set_valign(Gtk.Align.CENTER)
                row.append(widget)
            self.content.append(row)
            return widget

        def button(self, text, action, primary=False):
            button = Gtk.Button(label=text)
            if primary:
                button.add_css_class("suggested-action")
            button.connect("clicked", lambda _: action())
            return button

        def spin(self, value, low, high, step=1):
            spin = Gtk.SpinButton.new_with_range(low, high, step)
            spin.set_value(value)
            spin.set_numeric(True)
            return spin

        def render(self):
            self.generation += 1
            self.content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
            self.content.add_css_class("page")
            self.scroll.set_child(self.content)
            name = PAGES[self.page][0]
            self.content.append(self.label(name, "title"))
            if name == "Wi-Fi & Network":
                self.network_page()
            elif name == "Bluetooth":
                self.bluetooth_page()
            elif name == "Sound":
                self.sound_page()
            elif name == "Default Applications":
                self.defaults()
            elif name == "About This PC":
                self.about()
            elif not self.snapshot:
                self.content.append(self.label("Open this app from Navi’s launcher to change desktop settings. You can still use Network, Bluetooth, Sound and About This PC.", "muted"))
            elif name == "Displays":
                self.displays()
            elif name == "Wallpaper":
                self.wallpaper()
            elif name == "Power & Locking":
                self.locking()
            elif name == "Mouse & Keyboard":
                self.input_settings()

        def displays(self):
            self.content.append(self.label("Set display positions in logical pixels and choose a scale. Changes are previewed for 20 seconds before you keep them.", "muted"))
            if self.snapshot["nested"]:
                self.content.append(self.label("This previews Navi’s nested output. The host controls its physical monitor resolution and refresh rate; preview changes are not saved.", "muted"))
            self.fields = []
            outputs = self.snapshot["outputs"]
            for output in sorted(outputs, key=lambda o: o["priority"]):
                self.content.append(self.label(output["name"], "section"))
                self.row("Current mode", self.label(f"{output['width']} × {output['height']} · {output['refresh'] / 1000:g} Hz"))
                fields = {}
                for name, key, low, high, step in [("Horizontal position", "x", -65535, 65535, 1), ("Vertical position", "y", -65535, 65535, 1), ("Scale", "scale", 0.5, 4, 0.25)]:
                    fields[key] = self.row(name, self.spin(output[key], low, high, step), context=output["name"])
                modes = output.get("modes", [])
                if not self.snapshot["nested"] and modes:
                    picker = Gtk.DropDown.new_from_strings([f"{w} × {h} · {r / 1000:g} Hz" for w,h,r in modes])
                    picker.set_selected(next((i for i,m in enumerate(modes) if m == [output["width"], output["height"], output["refresh"]]), 0))
                    self.row("Resolution and refresh rate", picker, context=output["name"])
                    fields["mode"] = (picker, modes)
                fields["id"] = output["id"]
                self.fields.append(fields)
            self.primary = Gtk.DropDown.new_from_strings([o["name"] for o in outputs])
            self.primary.set_selected(next((i for i, o in enumerate(outputs) if o["priority"] == 0), 0))
            self.row("Primary display", self.primary, "The laptop panel has lower priority while an external display is connected.")
            apply = self.button("Preview Changes", self.preview_displays, True)
            apply.set_sensitive(bool(outputs) and self.token is None and not self.snapshot["pending"])
            self.content.append(apply)
            self.preview_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
            self.content.append(self.preview_box)
            if self.token is not None:
                self.keep_controls()

        def preview_displays(self):
            text = "navi-monitors 1\n"
            for fields in self.fields:
                text += f"{fields['id']} {int(fields['x'].get_value())} {int(fields['y'].get_value())} {fields['scale'].get_value():g}\n"
                if "mode" in fields:
                    picker, modes = fields["mode"]
                    w, h, r = modes[picker.get_selected()]
                    text += f"mode {fields['id']} {w} {h} {r}\n"
            primary = self.snapshot["outputs"][self.primary.get_selected()]["id"]
            text += "main " + primary + "\n"
            def ready(result, error):
                if error:
                    self.show_notice(error, True)
                    return
                self.token = result["token"]
                self.remaining = result["seconds"]
                self.render()
                self.show_notice("Display preview active. Keep or revert it on the Displays page.")
                def reveal():
                    adjustment = self.scroll.get_vadjustment()
                    adjustment.set_value(adjustment.get_upper() - adjustment.get_page_size())
                    return False
                GLib.timeout_add(100, reveal)
                GLib.timeout_add_seconds(1, self.countdown)
            self.work(lambda: request("preview", arrangement=text), ready)

        def keep_controls(self):
            self.count_label = self.label(f"Keep these display settings? Reverting in {self.remaining} seconds.")
            self.preview_box.append(self.count_label)
            buttons = Gtk.Box(spacing=12)
            buttons.append(self.button("Revert", lambda: self.finish_preview("revert")))
            buttons.append(self.button("Keep Settings", lambda: self.finish_preview("keep"), True))
            self.preview_box.append(buttons)

        def countdown(self):
            if self.token is None:
                return False
            self.remaining -= 1
            if self.remaining <= 0:
                self.token = None
                self.refresh()
                return False
            if self.page == 0:
                self.count_label.set_text(f"Keep these display settings? Reverting in {self.remaining} seconds.")
            return True

        def finish_preview(self, op):
            token = self.token
            def ready(result, error):
                if error:
                    self.show_notice(error, True)
                    # Timer remains authoritative; keep Revert available while pending.
                    return
                self.token = None
                self.snapshot = result
                self.render()
                self.show_notice("Display settings kept" if op == "keep" else "Previous display settings restored")
            self.work(lambda: request(op, token=token), ready)

        def wallpaper(self):
            self.content.append(self.label("Choose a PNG or JPEG for each display. The image fills the screen using Navi’s centered crop and is also used by the lock screen.", "muted"))
            for output in sorted(self.snapshot["outputs"], key=lambda o: o["priority"]):
                self.row(output["name"], self.button("Choose Image…", lambda p=output["priority"]: self.choose_wallpaper(p)))

        def choose_wallpaper(self, priority):
            chooser = Gtk.FileChooserNative.new("Choose Wallpaper", self.window, Gtk.FileChooserAction.OPEN, "Choose", "Cancel")
            filter = Gtk.FileFilter()
            filter.set_name("PNG and JPEG images")
            filter.add_mime_type("image/png")
            filter.add_mime_type("image/jpeg")
            chooser.add_filter(filter)
            def response(dialog, result):
                if result == Gtk.ResponseType.ACCEPT:
                    file = dialog.get_file()
                    path = file.get_path() if file else None
                    if path:
                        self.work(lambda: request("wallpaper", priority=priority, path=path), lambda _, error: self.show_notice(error or "Wallpaper applied", bool(error)))
                dialog.destroy()
            chooser.connect("response", response)
            chooser.show()

        def save_preferences(self, values):
            combined = dict(self.snapshot["preferences"], **values)
            def ready(result, error):
                if error:
                    self.show_notice(error, True)
                else:
                    self.snapshot = result
                    self.show_notice("Preferences saved and applied")
            self.work(lambda: request("preferences", preferences=combined), ready)

        def locking(self):
            self.content.append(self.label("Navi locks before session-menu Sleep and Switch User. Password authentication is handled by swaylock and your system’s PAM service.", "muted"))
            idle = self.row("Lock after inactivity", self.spin(self.snapshot["preferences"]["idle_minutes"], 0, 240), "Minutes. Set to 0 to turn automatic idle locking off.")
            self.content.append(self.button("Save Locking Preferences", lambda: self.save_preferences({"idle_minutes": int(idle.get_value())}), True))
            self.row("Lock now", self.button("Lock Screen", lambda: self.work(lambda: request("lock"), lambda _, error: self.show_notice(error or "Lock requested", bool(error)))))
            self.content.append(self.label("Inactivity counts input in Navi; applications cannot inhibit idle locking yet. Automatic screen-off and sleep timeouts are not configured by this version.", "muted"))

        def input_settings(self):
            p = self.snapshot["preferences"]
            self.content.append(self.label("Pointer and touchpad preferences apply to devices managed by a full Navi desktop session. A nested window keeps the host’s device configuration.", "muted"))
            speed = self.row("Pointer speed", self.spin(p["pointer_speed"], -1, 1, 0.1))
            natural = Gtk.Switch(active=p["natural_scroll"])
            tap = Gtk.Switch(active=p["tap_to_click"])
            self.row("Natural scrolling", natural)
            self.row("Tap to click", tap)
            capabilities = self.snapshot.get("input_capabilities", {})
            for key, widget in [("pointer_speed", speed), ("natural_scroll", natural), ("tap_to_click", tap)]:
                widget.set_sensitive(capabilities.get(key, False))
                if not capabilities.get(key, False):
                    widget.set_tooltip_text("No supported device is managed by this Navi session")
            rate = self.row("Keyboard repeat rate", self.spin(p["repeat_rate"], 0, 100), "Characters per second; 0 disables repeat.")
            delay = self.row("Keyboard repeat delay", self.spin(p["repeat_delay"], 100, 2000, 50), "Milliseconds before a held key repeats.")
            self.content.append(self.button("Save Input Preferences", lambda: self.save_preferences({"pointer_speed": speed.get_value(), "natural_scroll": natural.get_active(), "tap_to_click": tap.get_active(), "repeat_rate": int(rate.get_value()), "repeat_delay": int(delay.get_value())}), True))
            test = Gtk.Entry(placeholder_text="Type here to test keyboard repeat")
            self.row("Test typing", test)

        def load_native(self, job, render, unavailable):
            generation = self.generation
            loading = self.label('Loading devices…', 'muted')
            self.content.append(loading)
            def loaded(data, error):
                if generation != self.generation:
                    return
                self.content.remove(loading)
                if error:
                    self.content.append(self.label(unavailable, 'error'))
                    self.content.append(self.button('Try Again', self.render))
                else:
                    render(data)
            self.work(job, loaded)

        def operation(self, job, success='Settings applied'):
            if self.busy:
                return
            self.busy = True
            self.show_notice('Applying change…')
            def complete(result, error):
                self.busy = False
                self.cancel_pair_prompt()
                self.render()
                self.show_notice(error or success, bool(error))
            self.work(job, complete)

        def change(self, function, *values):
            self.operation(lambda: function(*values))

        def apply_audio(self, kind, index, volume, mute, default=False):
            self.change(self.services.set_audio, kind, index, int(volume.get_value()), mute.get_active(), default)

        def action_button(self, text, job, success='Settings applied'):
            button = self.button(text, lambda: self.operation(job, success))
            button.set_sensitive(not self.busy)
            return button

        def confirm(self, title, message, action):
            dialog = Gtk.MessageDialog(transient_for=self.window, modal=True, text=title, secondary_text=message)
            dialog.add_button('Cancel', Gtk.ResponseType.CANCEL)
            dialog.add_button('Confirm', Gtk.ResponseType.ACCEPT)
            dialog.set_default_response(Gtk.ResponseType.CANCEL)
            def response(widget, value):
                widget.destroy()
                if value == Gtk.ResponseType.ACCEPT:
                    action()
            dialog.connect('response', response)
            dialog.present()

        def network_page(self):
            self.content.append(self.label('Connect to Wi-Fi and Ethernet, scan nearby networks, and manage saved connections.', 'muted'))
            self.load_native(self.services.network, self.render_network, 'NetworkManager is unavailable or access was denied. Install and enable NetworkManager, then try again.')

        def render_network(self, data):
            wifi = [d for d in data['devices'] if d['kind'] == 2]
            if wifi:
                switch = Gtk.Switch(active=data['enabled'])
                switch.set_sensitive(data['hardware'] and not self.busy)
                switch.connect('notify::active', lambda w, _: self.change(self.services.wifi_enabled, w.get_active()))
                self.row('Wi-Fi', switch, 'Blocked by the hardware radio switch' if not data['hardware'] else 'Wireless networking for this computer')
            else:
                self.content.append(self.label('No Wi-Fi adapter is available.', 'muted'))
            for device in data['devices']:
                if device['kind'] == 1:
                    action = (lambda d=device: self.services.disconnect_network(d['path'])) if device['connected'] else (lambda d=device: self.services.connect_network(d['path'], profile=d['profiles'][0] if d['profiles'] else None))
                    self.row(device['interface'], self.action_button('Disconnect' if device['connected'] else 'Connect', action), 'Ethernet · ' + ('Connected' if device['connected'] else 'Disconnected'))
                elif data['enabled'] and data['hardware']:
                    controls = Gtk.Box(spacing=8)
                    def scan(path=device['path']):
                        self.operation(lambda: self.services.scan_wifi(path), 'Scan requested')
                        GLib.timeout_add_seconds(3, lambda: (self.render() if self.page == 2 else None, False)[1])
                    scan_button = self.button('Scan', scan)
                    scan_button.set_sensitive(not self.busy)
                    controls.append(scan_button)
                    controls.append(self.button('Hidden Network…', lambda path=device['path']: self.wifi_dialog(dict(device=path, ssid=b'', security='wpa-psk', path='/'), hidden=True)))
                    self.row(device['interface'], controls, 'Wi-Fi adapter')
            self.content.append(self.label('Nearby networks', 'section'))
            visible = data['networks'] if data['enabled'] and data['hardware'] else []
            if not visible:
                self.content.append(self.label('No networks found. Enable Wi-Fi and scan again.', 'muted'))
            for network in visible:
                controls = Gtk.Box(spacing=8)
                if network['connected']:
                    button = self.action_button('Disconnect', lambda n=network: self.services.disconnect_network(n['device']), 'Disconnected')
                elif network['profile']:
                    button = self.action_button('Connect', lambda n=network: self.services.connect_network(n['device'], profile=n['profile'], network=n), 'Connected')
                elif network['security'] in ('open', 'owe') and network['ssid']:
                    button = self.action_button('Connect', lambda n=network: self.services.connect_network(n['device'], network=n), 'Connected')
                else:
                    button = self.button('Connect', lambda n=network: self.wifi_dialog(n))
                    button.set_sensitive(not self.busy and bool(network['ssid']) and network['security'] in ('open', 'owe', 'wpa-psk', 'sae'))
                controls.append(button)
                security = {'wpa-psk':'WPA Personal', 'sae':'WPA3 Personal', 'enterprise':'Enterprise · provisioned profile required', 'wep':'Legacy WEP · provisioned profile required', 'open':'Open', 'owe':'Enhanced Open'}[network['security']]
                self.row(network['name'], controls, ('Connected · ' if network['connected'] else '') + f"{network['strength']}% signal · {security}")
            saved = [p for p in data['profiles'] if p['kind'] == '802-11-wireless']
            if saved:
                self.content.append(self.label('Saved networks', 'section'))
                for profile in saved:
                    self.row(profile['name'], self.button('Forget…', lambda p=profile: self.confirm('Forget this network?', 'Its saved configuration and credentials will be removed.', lambda: self.operation(lambda: self.services.forget_network(p['path']), 'Network forgotten'))))

        def wifi_dialog(self, network, hidden=False):
            if self.busy:
                return
            dialog = Gtk.Dialog(title='Join Hidden Network' if hidden else 'Connect to ' + network['name'], transient_for=self.window, modal=True)
            dialog.add_button('Cancel', Gtk.ResponseType.CANCEL)
            dialog.add_button('Connect', Gtk.ResponseType.ACCEPT)
            dialog.set_default_response(Gtk.ResponseType.CANCEL)
            box = dialog.get_content_area()
            box.set_spacing(12)
            for setter in (box.set_margin_start, box.set_margin_end, box.set_margin_top, box.set_margin_bottom):
                setter(20)
            name = Gtk.Entry(placeholder_text='Network name (SSID)')
            security = Gtk.DropDown.new_from_strings(['WPA2 Personal', 'WPA3 Personal', 'Open'])
            if hidden:
                box.append(self.label('Network name'))
                box.append(name)
                box.append(security)
            self.name_control(name, 'Network name (SSID)')
            self.name_control(security, 'Wi-Fi security')
            password = Gtk.PasswordEntry(show_peek_icon=True)
            self.name_control(password, 'Wi-Fi password')
            password.set_size_request(300, -1)
            password.set_visible(hidden or network['security'] in ('wpa-psk', 'sae'))
            box.append(self.label('Wi-Fi password' if password.get_visible() else 'Connect to this network?'))
            box.append(password)
            remember = Gtk.CheckButton(label='Remember this network and password', active=True)
            box.append(remember)
            problem = self.label('', 'error')
            box.append(problem)
            def response(widget, result):
                if result != Gtk.ResponseType.ACCEPT:
                    password.set_text('')
                    widget.destroy()
                    return
                selected = dict(network)
                if hidden:
                    selected['ssid'] = name.get_text().encode('utf-8')
                    selected['security'] = ['wpa-psk', 'sae', 'open'][security.get_selected()]
                secret = password.get_text() if selected['security'] in ('wpa-psk', 'sae') else ''
                try:
                    wifi_profile(selected['ssid'], selected['security'], secret, remember.get_active(), GLib.get_user_name(), hidden)
                except ValueError as error:
                    problem.set_text(str(error))
                    return
                save = remember.get_active()
                password.set_text('')
                widget.destroy()
                self.operation(lambda: self.services.connect_network(selected['device'], network=selected, password=secret, remember=save, hidden=hidden), 'Connected')
            dialog.connect('response', response)
            dialog.present()

        def bluetooth_page(self):
            self.content.append(self.label('Pair devices, check their battery levels, and manage connections. Pairing always asks before accepting a code or service.', 'muted'))
            self.load_native(self.services.bluetooth, self.render_bluetooth, 'Bluetooth is unavailable or access was denied. Install and enable BlueZ, then try again.')

        def render_bluetooth(self, data):
            if not data['adapters']:
                self.content.append(self.label('No Bluetooth adapter is available.', 'muted'))
            for adapter in data['adapters']:
                switch = Gtk.Switch(active=adapter['powered'])
                switch.set_sensitive(not self.busy)
                switch.connect('notify::active', lambda w, _, a=adapter: self.change(self.services.bluetooth_power, a['path'], w.get_active()))
                self.row(adapter['name'], switch, 'Bluetooth radio', context='Bluetooth radio')
                if adapter['powered']:
                    own_scan = adapter['path'] in self.services.discovering
                    def scan(a=adapter['path'], enabled=not own_scan):
                        self.operation(lambda: self.services.discover(a, enabled), 'Scanning for Bluetooth devices' if enabled else 'Scan stopped')
                        if enabled:
                            def stop():
                                if a in self.services.discovering:
                                    self.work(lambda: self.services.discover(a, False), lambda *_: self.render() if self.page == 3 else None)
                                return False
                            GLib.timeout_add_seconds(20, stop)
                            def update():
                                if self.page != 3 or a not in self.services.discovering or self.busy:
                                    return False
                                self.render()
                                return True
                            GLib.timeout_add_seconds(3, update)
                    button = self.button('Stop Scanning' if own_scan else 'Scan for Devices', scan)
                    button.set_sensitive(not self.busy)
                    self.content.append(button)
            if not data['devices']:
                self.content.append(self.label('No devices found. Turn on Bluetooth and make the device discoverable, then scan.', 'muted'))
            for device in data['devices']:
                controls = Gtk.Box(spacing=8)
                if device['paired']:
                    action = 'Disconnect' if device['connected'] else 'Connect'
                    button = self.button(action, lambda d=device, a=action: self.bluetooth_action(d, a))
                    button.set_sensitive(not self.busy)
                    controls.append(button)
                    controls.append(self.button('Forget…', lambda d=device: self.confirm('Forget this Bluetooth device?', 'Pairing information will be removed. You will need to pair again to reconnect.', lambda: self.operation(lambda: self.services.forget_bluetooth(d), 'Device forgotten'))))
                else:
                    button = self.button('Pair', lambda d=device: self.start_pairing(d))
                    button.set_sensitive(not self.busy)
                    controls.append(button)
                detail = 'Connected' if device['connected'] else ('Paired' if device['paired'] else 'Not paired')
                if device['battery'] is not None:
                    detail += f" · {device['battery']}% battery"
                self.row(device['name'], controls, detail)
                if device['paired']:
                    trust = Gtk.Switch(active=device['trusted'])
                    trust.set_sensitive(not self.busy)
                    trust.connect('notify::active', lambda w, _, d=device: self.change(self.services.trust_bluetooth, d['path'], w.get_active()))
                    self.row('Allow automatic connections', trust, device['name'], context=device['name'])

        def bluetooth_action(self, device, action):
            if self.busy:
                return
            if action == 'Connect':
                try:
                    self.services.prepare_agent(self.pair_request)
                except Exception:
                    self.show_notice('Could not start the Bluetooth authorization agent.', True)
                    return
            self.operation(lambda: self.services.device_action(device['path'], action), action + ' request completed')

        def start_pairing(self, device):
            if self.busy:
                return
            try:
                self.services.prepare_agent(self.pair_request)
            except Exception:
                self.show_notice('Could not start the Bluetooth pairing agent.', True)
                return
            self.operation(lambda: self.services.pair(device['path']), 'Device paired. You can now connect it.')

        def cancel_pair_prompt(self):
            if self.pair_invocation:
                self.pair_invocation.return_dbus_error('org.bluez.Error.Canceled', 'Pairing cancelled')
                self.pair_invocation = None
            if self.pair_dialog:
                dialog = self.pair_dialog
                self.pair_dialog = None
                self.closing_pair_prompt = True
                try:
                    dialog.destroy()
                finally:
                    self.closing_pair_prompt = False

        def pair_request(self, connection, sender, path, interface, method, parameters, invocation):
            if not self.services.blue_owner or sender != self.services.blue_owner:
                invocation.return_dbus_error('org.bluez.Error.Rejected', 'Unauthorized pairing sender')
                return
            values = parameters.unpack()
            if method in ('Cancel', 'Release'):
                self.cancel_pair_prompt()
                invocation.return_value(None)
                return
            if not values or values[0] != self.services.agent_device:
                invocation.return_dbus_error('org.bluez.Error.Rejected', 'No pairing was requested for this device')
                return
            display_only = method in ('DisplayPinCode', 'DisplayPasskey')
            message = 'Approve this Bluetooth pairing request?'
            if method == 'RequestConfirmation':
                message = f'Does the device show {values[1]:06d}? Confirm only if the codes match.'
            elif method == 'DisplayPinCode':
                message = 'Enter this code on the other device: ' + values[1]
            elif method == 'DisplayPasskey':
                message = f'Enter {values[1]:06d} on the other device ({values[2]} of 6 digits entered).'
            elif method == 'AuthorizeService':
                message = 'Allow this device to use Bluetooth service ' + display_name(values[1]) + '?'
            if display_only and self.pair_dialog and self.pair_kind == method:
                self.pair_message.set_text(message)
                invocation.return_value(None)
                return
            self.cancel_pair_prompt()
            self.pair_kind = method
            dialog = Gtk.Dialog(title='Bluetooth Pairing', transient_for=self.window, modal=True)
            self.pair_dialog = dialog
            dialog.add_button('Cancel', Gtk.ResponseType.CANCEL)
            if not display_only:
                dialog.add_button('Confirm', Gtk.ResponseType.ACCEPT)
            dialog.set_default_response(Gtk.ResponseType.CANCEL)
            box = dialog.get_content_area()
            box.set_spacing(12)
            for setter in (box.set_margin_start, box.set_margin_end, box.set_margin_top, box.set_margin_bottom):
                setter(20)
            self.pair_message = self.label(message)
            box.append(self.pair_message)
            problem = self.label('', 'error')
            box.append(problem)
            entry = Gtk.Entry()
            self.name_control(entry, 'Bluetooth PIN' if method == 'RequestPinCode' else 'Bluetooth passkey')
            if method in ('RequestPinCode', 'RequestPasskey'):
                box.append(self.label('Enter the PIN or passkey provided by the other device.'))
                box.append(entry)
            if display_only:
                invocation.return_value(None)
            else:
                self.pair_invocation = invocation
            def response(widget, result):
                if self.closing_pair_prompt:
                    return
                pending = self.pair_invocation
                if result == Gtk.ResponseType.ACCEPT and pending:
                    value = None
                    if method == 'RequestPinCode':
                        if not 1 <= len(entry.get_text()) <= 16 or not entry.get_text().isascii() or not entry.get_text().isalnum():
                            problem.set_text('Enter a PIN of 1–16 letters or digits.')
                            return
                        value = GLib.Variant('(s)', (entry.get_text(),))
                    elif method == 'RequestPasskey':
                        if not entry.get_text().isascii() or not entry.get_text().isdigit() or len(entry.get_text()) > 6:
                            problem.set_text('Enter a numeric passkey from 0 to 999999.')
                            return
                        value = GLib.Variant('(u)', (int(entry.get_text()),))
                    pending.return_value(value)
                    self.pair_invocation = None
                else:
                    if self.services.pairing_device:
                        self.work(lambda: self.services.device_action(values[0], 'CancelPairing'), lambda *_: None)
                self.cancel_pair_prompt()
            dialog.connect('response', response)
            dialog.present()

        def sound_page(self):
            self.content.append(self.label('Choose speakers and a microphone, adjust volume and mute, and control application audio.', 'muted'))
            self.load_native(self.services.sound, self.render_sound, 'The audio service is unavailable. Install pactl (pulseaudio-utils on Debian/Ubuntu) and enable PipeWire-Pulse or PulseAudio.')

        def render_sound(self, data):
            for title, key, default_key, kind in [('Output', 'outputs', 'default_output', 'sink'), ('Input', 'inputs', 'default_input', 'source')]:
                self.content.append(self.label(title, 'section'))
                devices = data[key]
                if not devices:
                    self.content.append(self.label('No ' + title.lower() + ' devices are available.', 'muted'))
                    continue
                chooser = Gtk.DropDown.new_from_strings([d['label'] for d in devices])
                chooser.set_selected(next((i for i,d in enumerate(devices) if d['name'] == data[default_key]), 0))
                self.row('Device', chooser, context=title)
                volume = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 0, 100, 1)
                volume.set_size_request(180, -1)
                volume.set_draw_value(True)
                volume.set_value_pos(Gtk.PositionType.RIGHT)
                mute = Gtk.Switch()
                def selected(w, _, items=devices, slider=volume, switch=mute):
                    d = items[w.get_selected()]
                    slider.set_value(min(100, d['volume']))
                    switch.set_active(d['mute'])
                chooser.connect('notify::selected', selected)
                selected(chooser, None)
                self.row('Volume (%)', volume, context=title)
                self.row('Muted', mute, context=title)
                self.content.append(self.button('Apply ' + title, lambda c=chooser, v=volume, m=mute, items=devices, k=kind: self.apply_audio(k, items[c.get_selected()]['index'], v, m, True), True))
            self.content.append(self.label('Applications', 'section'))
            if not data['applications']:
                self.content.append(self.label('Applications playing audio appear here.', 'muted'))
            for application in data['applications']:
                controls = Gtk.Box(spacing=8)
                volume = self.spin(min(100, application['volume']), 0, 100)
                mute = Gtk.Switch(active=application['mute'], tooltip_text='Mute application')
                self.name_control(volume, 'Volume (%): ' + application['label'])
                self.name_control(mute, 'Mute: ' + application['label'])
                controls.append(volume)
                controls.append(mute)
                controls.append(self.button('Apply', lambda a=application, v=volume, m=mute: self.apply_audio('sink-input', a['index'], v, m)))
                self.row(application['label'], controls, 'Volume (%) and mute')

        def defaults(self):
            self.content.append(self.label("Choose applications for common links and files. Changes use the system’s standard MIME associations.", "muted"))
            for name, mime in [("Web browser", "x-scheme-handler/https"), ("Email", "x-scheme-handler/mailto"), ("File manager", "inode/directory"), ("PDF documents", "application/pdf"), ("Text files", "text/plain")]:
                button = Gtk.AppChooserButton.new(mime)
                button.set_show_default_item(True)
                button.set_show_dialog_item(True)
                def changed(chooser, content_type=mime):
                    app = chooser.get_app_info()
                    if not app:
                        return
                    try:
                        app.set_as_default_for_type(content_type)
                        if content_type == "x-scheme-handler/https":
                            app.set_as_default_for_type("x-scheme-handler/http")
                        self.show_notice("Default application updated")
                    except GLib.Error as e:
                        self.show_notice(e.message, True)
                button.connect("changed", changed)
                self.row(name, button)

        def about(self):
            self.content.append(self.label("System information for this computer. Copy the summary when reporting an issue.", "muted"))
            version = self.snapshot.get("version", "0.1.0") if self.snapshot else "0.1.0"
            self.row("Navi", self.label(version))
            self.row("Session", self.label("Nested Navi" if self.snapshot and self.snapshot["nested"] else ("Navi desktop" if self.snapshot else "Outside Navi")))
            generation = self.generation
            def loaded(values, error):
                if self.generation != generation:
                    return
                if error:
                    self.content.append(self.label(error, "error"))
                    return
                for key, value in values:
                    self.row(key, self.label(value))
                summary = "Navi " + version + "\n" + "\n".join(key + ": " + value for key, value in values)
                self.content.append(self.button("Copy System Information", lambda: Gdk.Display.get_default().get_clipboard().set(summary)))
            self.work(about_pc, loaded)

    return Settings().run(sys.argv)

if __name__ == "__main__":
    raise SystemExit(run_settings())
