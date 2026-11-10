"""Additional native Settings pages, using the existing form and worker helpers."""
def expanded_settings(Gtk, Gdk, Gio, GLib):
    class ExtraSettings:
        def desktop_value(self):
            return self.snapshot.get('desktop', {}) if self.snapshot else {}

        def apply_desktop(self, values):
            current = dict(self.desktop_value(), **values)
            def ready(result, error):
                if error:
                    self.show_notice(error, True)
                    return
                self.snapshot = result
                self.apply_appearance()
                self.render()
                self.show_notice('Desktop preferences saved. Existing applications may need reopening to adopt their theme.')
            self.work(lambda: request('desktop', settings=current), ready)

        def switch_row(self, title, active, description=None):
            switch = Gtk.Switch(active=bool(active))
            self.row(title, switch, description)
            return switch

        def selected_choice(self, title, labels, values, current, description=None):
            control = self.choice(labels)
            control.set_selected(values.index(current) if current in values else 0)
            self.row(title, control, description)
            return control

        def apply_appearance(self):
            p = self.desktop_value()
            light = p.get('scheme') == 'light'
            accents = {'blue': ('#2ba3d8', '#0077aa'), 'green': ('#4bcd96', '#08754f'),
                       'purple': ('#bd94f7', '#7950ad'), 'amber': ('#edb64e', '#906000'), 'pink': ('#f28ab8', '#ad3268')}
            accent = accents.get(p.get('accent'), accents['blue'])[int(light)]
            colors = dict(zip(['#0b0d12','#151922','#262c38','#303847','#d0d4dc','#a4adbd','#7a8396','#1d2430','#ff9aa6','#5bd6a2'],
                ['#f5f6f8','#ffffff','#e3e7ed','#d6dce5','#202734','#525e70','#617084','#e9edf2','#9d233e','#08754f'] if light else
                ['#0b0d12','#151922','#262c38','#303847','#d0d4dc','#a4adbd','#7a8396','#1d2430','#ff9aa6','#5bd6a2']))
            if p.get('high_contrast') and light:
                colors.update({'#0b0d12':'#ffffff','#151922':'#ffffff','#d0d4dc':'#000000','#a4adbd':'#333333','#7a8396':'#333333'})
            if p.get('high_contrast') and not light:
                colors.update({'#0b0d12':'#000000','#151922':'#000000','#d0d4dc':'#ffffff','#a4adbd':'#c0c7d2','#7a8396':'#c0c7d2'})
            colors['#2ba3d8'] = accent
            css = re.sub(r'#[0-9a-f]{6}', lambda m: colors.get(m[0],m[0]), self.base_css)
            scale = p.get('font_size',14) / 14
            css = re.sub(r'font-size: (\d+)px', lambda m: 'font-size: ' + str(round(int(m[1])*scale)) + 'px', css)
            self.css_provider.load_from_data(css.encode())
            Gtk.Settings.get_default().set_property('gtk-application-prefer-dark-theme', not light)
            Gtk.Settings.get_default().set_property('gtk-icon-theme-name',p.get('icon_theme') or self.initial_icon_theme)

        def monitor_diagram(self):
            diagram=Gtk.DrawingArea(content_height=210,hexpand=True)
            diagram.set_tooltip_text('Drag displays to arrange them. The position fields below provide the same controls by keyboard.')
            self.monitor_targets=[]
            def draw(area,cr,width,height):
                rows=[]
                outputs=self.snapshot['outputs']
                for fields in self.fields:
                    output=next(o for o in outputs if o['id']==fields['id'])
                    w,h=output['width'],output['height']
                    if 'mode' in fields:
                        picker,modes=fields['mode'];w,h,_=modes[picker.get_selected()]
                    angle=[0,90,180,270][fields['rotation'].get_selected()] if 'rotation' in fields else 0
                    if angle in (90,270):w,h=h,w
                    scale=fields['scale'].get_value()
                    rows.append((fields,output['name'],fields['x'].get_value(),fields['y'].get_value(),w/scale,h/scale))
                if not rows:return
                left=min(r[2] for r in rows);top=min(r[3] for r in rows)
                span_x=max(r[2]+r[4] for r in rows)-left;span_y=max(r[3]+r[5] for r in rows)-top
                factor=min((width-40)/max(1,span_x),(height-40)/max(1,span_y))
                dx=(width-span_x*factor)/2;dy=(height-span_y*factor)/2
                self.monitor_targets=[];self.monitor_factor=factor
                light=self.desktop_value().get('scheme')=='light'
                for fields,name,x,y,w,h in rows:
                    rect=(dx+(x-left)*factor,dy+(y-top)*factor,w*factor,h*factor)
                    a,b,c,d=rect
                    cr.set_source_rgb(*((.88,.91,.95) if light else (.08,.10,.14)));cr.rectangle(a+2,b+2,max(1,c-4),max(1,d-4));cr.fill_preserve()
                    cr.set_source_rgb(.17,.64,.85);cr.set_line_width(2);cr.stroke()
                    cr.set_source_rgb(*((.12,.15,.20) if light else (.82,.84,.88)));cr.select_font_face('Noto Sans');cr.set_font_size(12)
                    label=name
                    if cr.text_extents(label).width>max(0,c-20):
                        while label and cr.text_extents(label+'…').width>max(0,c-20):label=label[:-1]
                        label=label+'…' if label else ''
                    cr.save();cr.rectangle(a+4,b+4,max(0,c-8),max(0,d-8));cr.clip()
                    if d>=24:
                        cr.move_to(a+10,b+min(24,d/2));cr.show_text(label)
                    cr.restore()
                    self.monitor_targets.append((fields,rect))
            diagram.set_draw_func(draw)
            drag=Gtk.GestureDrag()
            def begin(_,x,y):
                self.drag_monitor=next(((f,f['x'].get_value(),f['y'].get_value()) for f,(a,b,w,h) in reversed(self.monitor_targets) if a<=x<a+w and b<=y<b+h),None)
                self.drag_factor=getattr(self,'monitor_factor',1)
            def update(gesture,dx,dy):
                item=getattr(self,'drag_monitor',None)
                if item:
                    fields,x,y=item
                    fields['x'].set_value(round((x+dx/self.drag_factor)/20)*20)
                    fields['y'].set_value(round((y+dy/self.drag_factor)/20)*20)
                    # The controller owns this callback; capturing its widget leaks each diagram.
                    gesture.get_widget().queue_draw()
            drag.connect('drag-begin',begin);drag.connect('drag-update',update)
            diagram.add_controller(drag)
            return diagram

        def appearance(self):
            self.content.append(self.label('Choose Navi’s colors, installed icon and cursor themes, and dock behavior.', 'muted'))
            p = self.desktop_value()
            self.group('Colors & text')
            scheme = self.selected_choice('Color scheme',['Dark','Light'],['dark','light'],p.get('scheme','dark'))
            accents = ['blue','green','purple','amber','pink']
            accent = self.selected_choice('Accent color',[s.title() for s in accents],accents,p.get('accent','blue'))
            font = self.row('Text size',self.spin(p.get('font_size',14),12,24),'Navi Settings and shell text, in logical pixels')
            self.group('Icons & pointer')
            icons = ['',*theme_choices('icons')]
            cursors = ['',*theme_choices('cursors')]
            if p.get('icon_theme') and p['icon_theme'] not in icons: icons.append(p['icon_theme'])
            if p.get('cursor_theme') and p['cursor_theme'] not in cursors: cursors.append(p['cursor_theme'])
            icon = self.selected_choice('Icon theme',['System default',*icons[1:]],icons,p.get('icon_theme',''))
            cursor = self.selected_choice('Cursor theme',['Navi default',*cursors[1:]],cursors,p.get('cursor_theme',''))
            size = self.row('Cursor size',self.spin(p.get('cursor_size',24),16,64),'Applies to Navi’s pointer and newly opened applications')
            self.group('Dock')
            follow = self.switch_row('Follow the screen edge',p.get('dock_follow',True),'Move the dock by hovering at the bottom edge of another monitor')
            self.content.append(self.button('Apply Appearance',lambda:self.apply_desktop(dict(scheme=['dark','light'][scheme.get_selected()],
                accent=accents[accent.get_selected()],font_size=int(font.get_value()),icon_theme=icons[icon.get_selected()],
                cursor_theme=cursors[cursor.get_selected()],cursor_size=int(size.get_value()),dock_follow=follow.get_active())),True))
            self.content.append(self.label('GTK applications opened from Navi receive the color scheme. Apps with their own themes may keep their preferences.', 'muted'))

        def notifications_page(self):
            p = self.desktop_value()
            self.content.append(self.label('Control desktop alerts and review recent notifications. History is kept in memory for this Navi session.', 'muted'))
            self.group('Desktop notifications')
            enabled = self.switch_row('Show notifications',p.get('notifications',True))
            dnd = self.switch_row('Do Not Disturb',p.get('dnd',False),'Silence banners and notification sounds; history can still collect messages')
            history = self.switch_row('Keep notification history',p.get('history',True),'Up to 50 messages; turning this off clears existing history')
            self.row('Lock-screen privacy',self.label('Notifications stay hidden while locked'))
            self.content.append(self.button('Save Notification Preferences',lambda:self.apply_desktop(dict(notifications=enabled.get_active(),dnd=dnd.get_active(),history=history.get_active())),True))
            self.group('UnifiedPush')
            push = self.switch_row('Enable UnifiedPush',p.get('push_enabled',False),'Off by default. Participating apps receive push through an optional distributor.')
            providers = ['Ntfy','Autopush']
            provider = self.selected_choice('Provider',['ntfy','Mozilla Autopush'],providers,p.get('push_provider','Ntfy'))
            url = Gtk.Entry(text=p.get('push_url','https://ntfy.sh'))
            self.row('Server address',url,'HTTPS required. Provider registration contacts this server only after enabling.')
            def provider_changed(w,_):
                url.set_text('https://ntfy.sh' if w.get_selected() == 0 else 'https://push.services.mozilla.com')
            provider.connect('notify::selected',provider_changed)
            def apply_push():
                values=dict(push_enabled=push.get_active(),push_provider=providers[provider.get_selected()],push_url=url.get_text().strip())
                address=urllib.parse.urlsplit(values['push_url'])
                if address.scheme != 'https' or not address.hostname or address.username or address.password:
                    self.show_notice('Enter an HTTPS provider URL without credentials.',True)
                    return
                if values['push_enabled']:
                    self.confirm('Enable or reconfigure UnifiedPush?', 'Navi will contact this provider. Changing providers can replace app endpoints; applications may need to register again.',lambda:self.apply_desktop(values))
                else: self.apply_desktop(values)
            apply=self.button('Apply UnifiedPush',apply_push,True)
            nested=not self.snapshot or self.snapshot['nested']
            for control in (push,provider,url,apply): control.set_sensitive(not nested)
            self.content.append(apply)
            self.content.append(self.label('UnifiedPush runs only in a full Navi session. Install kunifiedpush to enable it. Navi leaves any existing distributor running independently.', 'muted'))
            self.load_native(lambda:dict(shell=shell_request('get'),push=self.services.push()),lambda data:(self.render_push(data['push']),self.render_history(data['shell'])),'Navi’s notification service is unavailable. Reopen Settings from Navi and unlock the session.')

        def render_history(self,data):
            self.group('Recent notifications')
            self.content.append(self.button('Clear History',lambda:self.operation(lambda:shell_request('clear'),'Notification history cleared')))
            notes=data['history']
            if not notes: self.content.append(self.label('No recent notifications. New messages will appear here.', 'muted'))
            blocked=set(self.desktop_value().get('blocked_apps',[]))
            for note in notes:
                actions=Gtk.Box(spacing=8)
                app=note.get('app_id') or note['app']
                def block(app=app): self.apply_desktop(dict(blocked_apps=sorted(set(self.desktop_value().get('blocked_apps',[]))|{app})))
                actions.append(self.button('Block App',block))
                self.row(note['summary'] or note['app'],actions,note['app']+' · '+note['body'])
                for key,label in note.get('actions',[]):
                    self.content.append(self.button(label,lambda n=note,k=key:self.operation(lambda:shell_request('action',id=n['id'],key=k),'Notification action sent')))
            if blocked:
                self.group('Blocked applications')
                for app in sorted(blocked):
                    self.row(app,self.button('Allow',lambda app=app:self.apply_desktop(dict(blocked_apps=[s for s in self.desktop_value()['blocked_apps'] if s!=app]))))

        def render_push(self,data):
            self.group('Push connection')
            self.row('Status',self.label(data['state']))
            self.content.append(self.button('Refresh Push Status',self.render))
            for client in data.get('clients',[]):
                self.row(client['description'] or client['service'],self.button('Remove…',lambda c=client:self.confirm('Remove this push registration?',
                    'This app will stop receiving messages until it registers again.',lambda:self.operation(lambda:self.services.remove_push_client(c['token']),'Push registration removed'))),client['service'])

        def system_sounds(self):
            p=self.desktop_value()
            self.content.append(self.label('Choose installed event sounds. System sounds run outside the compositor and respect their own volume.', 'muted'))
            self.group('Sound theme')
            enabled=self.switch_row('Play system sounds',p.get('sounds',False))
            themes=sorted(set(['freedesktop',p.get('sound_theme','freedesktop'),*theme_choices('sounds')]))
            theme=self.selected_choice('Theme',themes,themes,p.get('sound_theme','freedesktop'))
            volume=self.row('Event volume (%)',self.spin(p.get('sound_volume',40),0,100))
            self.group('Events')
            events={}
            for key,title in [('notification','New notification'),('warning','Warning'),('volume','Volume change'),('battery','Low battery'),('login','Session start'),('logout','Session end')]:
                events[key]=self.switch_row(title,key in p.get('sound_events',[]))
            self.content.append(self.button('Save System Sounds',lambda:self.apply_desktop(dict(sounds=enabled.get_active(),sound_theme=themes[theme.get_selected()],
                sound_volume=int(volume.get_value()),sound_events=[k for k,w in events.items() if w.get_active()])),True))
            preview=self.button('Preview Notification Sound',lambda:self.operation(lambda:shell_request('sound',event='notification'),'Preview requested'))
            preview.set_sensitive(p.get('sounds',False) and p.get('sound_volume',0)>0)
            self.content.append(preview)
            self.content.append(self.label('Requires canberra-gtk-play and an installed sound theme. Save before previewing. Muting event sounds leaves application audio unchanged.', 'muted'))

        def accessibility(self):
            p=self.desktop_value()
            self.content.append(self.label('Make Navi easier to see and use. Native controls remain available by keyboard.', 'muted'))
            self.group('Vision & alerts')
            contrast=self.switch_row('High contrast',p.get('high_contrast',False))
            alerts=self.switch_row('Visual notification alerts',p.get('visual_alerts',False),'Use a prominent border on notification cards without requiring sound')
            font=self.row('Text size',self.spin(p.get('font_size',14),12,24))
            cursor=self.row('Pointer size',self.spin(p.get('cursor_size',24),16,64))
            sticky=self.switch_row('Sticky modifier keys',p.get('sticky_keys',False),'Tap Shift, Ctrl, Alt or Super, then press the next key. Tap the modifier again to cancel.')
            self.row('Motion',self.label('Animations are disabled'))
            self.content.append(self.button('Apply Accessibility Preferences',lambda:self.apply_desktop(dict(high_contrast=contrast.get_active(),visual_alerts=alerts.get_active(),sticky_keys=sticky.get_active(),
                font_size=int(font.get_value()),cursor_size=int(cursor.get_value()))),True))
            self.content.append(self.label('Screen-reader access to the shell requires further platform work. Native Settings controls expose their labels to assistive technology.', 'muted'))

        def extra_input(self):
            p=self.desktop_value()
            self.group('Mouse & touchpad')
            left=self.switch_row('Left-handed buttons',p.get('left_handed',False))
            flat=self.switch_row('Disable pointer acceleration',p.get('flat_acceleration',False))
            typing=self.switch_row('Disable touchpad while typing',p.get('disable_while_typing',True))
            gestures=self.switch_row('Three-finger window moves',p.get('swipe_moves',False),'Swipe at least 80 logical pixels to move the focused tile one grid cell; occupied cells stop it.')
            for w in (left,flat,typing,gestures): w.set_sensitive(not self.snapshot['nested'])
            self.group('Keyboard layout & shortcuts')
            layout=Gtk.Entry(text=p.get('keyboard_layout','us'))
            variant=Gtk.Entry(text=p.get('keyboard_variant',''))
            options=Gtk.Entry(text=p.get('keyboard_options',''))
            self.row('Layout',layout,'XKB layout names, such as us, gb, de, or us,de')
            self.row('Variant',variant,'Optional XKB variant, such as intl')
            self.row('Layout options',options,'For example grp:alt_shift_toggle to switch layouts')
            modifier=self.selected_choice('Move windows',['Alt + Arrow','Super + Arrow'],['alt','super'],p.get('move_modifier','alt'),shortcut_hint(p.get('move_modifier')))
            hint=self.setting_rows[-1][2].get_last_child()
            modifier.connect('notify::selected',lambda w,_:hint.set_label(shortcut_hint(['alt','super'][w.get_selected()])))
            self.content.append(self.button('Apply Layout & Device Preferences',lambda:self.apply_desktop(dict(swipe_moves=gestures.get_active(),left_handed=left.get_active(),flat_acceleration=flat.get_active(),disable_while_typing=typing.get_active(),
                keyboard_layout=layout.get_text(),keyboard_variant=variant.get_text(),keyboard_options=options.get_text(),
                move_modifier=['alt','super'][modifier.get_selected()],expand_modifier=['super','alt'][modifier.get_selected()])),True))

        def extra_power(self):
            p=self.desktop_value()
            self.group('Display & sleep')
            off=self.row('Turn displays off after',self.spin(p.get('screen_off_minutes',0),0,240),'Minutes of inactivity; 0 disables. Navi locks first.')
            sleep=self.row('Sleep after',self.spin(p.get('sleep_minutes',0),0,240),'Minutes of inactivity; 0 disables. Requires a confirmed screen lock.')
            lid=self.selected_choice('When the lid closes',['Do nothing','Lock screen','Lock and sleep'],['nothing','lock','suspend'],p.get('lid_action','lock'))
            apply=self.button('Save Display & Sleep Preferences',lambda:self.apply_desktop(dict(screen_off_minutes=int(off.get_value()),sleep_minutes=int(sleep.get_value()),lid_action=['nothing','lock','suspend'][lid.get_selected()])),True)
            for w in (off,sleep,lid,apply): w.set_sensitive(not self.snapshot['nested'])
            self.content.append(apply)
            self.load_native(self.services.power,self.render_power,'Power service information is unavailable. Install power-profiles-daemon for power profiles.')

        def render_power(self,data):
            self.group('Hardware power controls')
            if data['profiles']:
                profile=self.selected_choice('Power profile',data['profiles'],data['profiles'],data['active'])
                self.content.append(self.button('Apply Power Profile',lambda:self.change(self.services.power_profile,data['profiles'][profile.get_selected()])))
            else: self.content.append(self.label('Power profiles are unavailable. Install and enable power-profiles-daemon on supported hardware.', 'muted'))
            for device in data['backlights']:
                value=self.row(device['name'],self.spin(device['percent'],1,100),'Brightness (%) · affects this computer')
                self.content.append(self.button('Apply Brightness',lambda d=device,v=value:self.change(self.services.brightness,d['name'],int(v.get_value()))))

        def printers_page(self):
            self.content.append(self.label('Manage CUPS printers and add driverless IPP printers. Administrative changes may require system authorization.', 'muted'))
            self.load_native(self.services.printers,self.render_printers,'CUPS is unavailable. Install CUPS client tools and enable the printing service, then try again.')

        def render_printers(self,data):
            self.group('Installed printers')
            if not data['printers']: self.content.append(self.label('No printers are configured.', 'muted'))
            for printer in data['printers']:
                controls=Gtk.FlowBox(selection_mode=Gtk.SelectionMode.NONE,max_children_per_line=3,column_spacing=8,row_spacing=8)
                for title,action in [('Make Default','default'),('Enable','enable'),('Pause','disable')]:
                    controls.append(self.button(title,lambda n=printer['name'],a=action:self.change(self.services.printer_action,n,a)))
                self.row(printer['name']+(' · Default' if printer['name']==data['default'] else ''),controls,printer['status'])
                self.content.append(self.button('Remove Printer…',lambda n=printer['name']:self.confirm('Remove '+n+'?', 'The print queue will be removed.',lambda:self.change(self.services.printer_action,n,'remove'))))
            self.group('Print queue')
            for job in data.get('jobs',[]):
                self.row(job['id'],self.button('Cancel Job…',lambda j=job:self.confirm('Cancel this print job?',j['id'],lambda:self.change(self.services.cancel_print_job,j['id']))),job['description'])
            self.group('Add a printer')
            name=Gtk.Entry(placeholder_text='office-printer')
            uri=Gtk.Entry(placeholder_text='ipps://printer.local/ipp/print')
            self.row('Queue name',name)
            self.row('Printer address',uri,'IPP or IPPS; driverless printers only')
            self.content.append(self.button('Add Printer',lambda:self.change(self.services.add_printer,name.get_text(),uri.get_text()),True))

        def accounts_page(self):
            self.content.append(self.label('Manage local Linux accounts through AccountsService. Central provisioning and browser SSO can be added independently.', 'muted'))
            self.load_native(self.services.accounts,self.render_accounts,'AccountsService is unavailable or permission was denied. Install accountsservice and a system authorization agent.')

        def render_accounts(self,data):
            self.group('Local accounts')
            for account in data:
                name=Gtk.Entry(text=account['real_name'])
                self.row(account['name'],name,'Administrator' if account['admin'] else 'Standard account')
                admin=self.switch_row('Administrator: '+account['name'],account['admin'])
                locked=self.switch_row('Account locked: '+account['name'],account['locked'])
                self.content.append(self.button('Save '+account['name'],lambda a=account,n=name,d=admin,l=locked:self.confirm('Update '+a['name']+'?', 'Account access and privileges may change. System authorization may be required.',lambda:self.change(self.services.update_account,a['path'],n.get_text(),d.get_active(),l.get_active()))))
                self.content.append(self.button('Set Password…',lambda a=account:self.operation(lambda:self.services.password_tool(a['name']),'System password tool opened')))
                if account['uid'] != os.getuid() and account['uid'] >= 1000:
                    self.content.append(self.button('Remove Account…',lambda a=account:self.confirm('Remove '+a['name']+'?', 'The account will be removed. Its home directory and files will be kept.',lambda:self.change(self.services.delete_account,a['path']))))
            self.group('Create account')
            username=Gtk.Entry(placeholder_text='username')
            name=Gtk.Entry(placeholder_text='Display name')
            admin=self.switch_row('New account is an administrator',False)
            self.row('Account name',username)
            self.row('Display name',name)
            self.content.append(self.button('Create Account…',lambda:self.confirm('Create a local account?', 'The new account starts locked. Set its password with your system account tools before use.',lambda:self.change(self.services.create_account,username.get_text(),name.get_text(),admin.get_active())),True))
            self.content.append(self.label('Passwords remain with the system password tool and PAM. Navi never stores login passwords.', 'muted'))

        def regional_page(self):
            self.content.append(self.label('Set the computer’s clock, time zone, and installed system language. Changes affect all users and may request authorization.', 'muted'))
            self.load_native(self.services.regional,self.render_regional,'Time or locale services are unavailable. Enable systemd-timedated and systemd-localed, then try again.')

        def render_regional(self,data):
            self.group('Time & language')
            zone=self.selected_choice('Time zone',data['zones'],data['zones'],data['timezone'])
            ntp=self.switch_row('Set time automatically',data['ntp'])
            ntp.set_sensitive(data['can_ntp'])
            locales=data['locales'] or ['C']
            locale=self.selected_choice('System language / locale',locales,locales,data['locale'],'Install more locales through your distribution. Apps adopt language on their next start.')
            self.content.append(self.button('Apply Time & Language',lambda:self.confirm('Apply system-wide regional settings?', 'This changes the time zone, automatic clock setting and default language for this computer.',lambda:self.change(self.services.set_regional,data['zones'][zone.get_selected()],ntp.get_active(),locales[locale.get_selected()])),True))
            self.group('Manual clock')
            value=Gtk.Entry(placeholder_text='2026-11-06T14:30:00-05:00')
            self.row('Date and time',value,'ISO date/time with an explicit time-zone offset. Turn automatic time off first.')
            button=self.button('Set Clock…',lambda:self.confirm('Change the system clock?', 'Changing time can affect running applications.',lambda:self.change(self.services.set_clock,value.get_text())))
            button.set_sensitive(not data['ntp'])
            self.content.append(button)

        def advanced_network(self,data):
            self.group('Advanced connections')
            self.content.append(self.button('Import VPN Profile…',self.import_vpn))
            for profile in data.get('profiles',[]):
                controls=Gtk.FlowBox(selection_mode=Gtk.SelectionMode.NONE,max_children_per_line=3,column_spacing=8,row_spacing=8)
                controls.append(self.button('IP, DNS & Proxy…',lambda p=profile:self.edit_network_profile(p)))
                if profile['kind'] in ('vpn','wireguard'):
                    controls.append(self.button('Connect',lambda p=profile:self.operation(lambda:self.services.call(NM,NM_ROOT,NM,'ActivateConnection','(ooo)',(p['path'],'/','/')),'VPN activation requested')))
                self.row(profile['name'],controls,profile['kind'])

        def edit_network_profile(self,profile):
            def loaded(data,error):
                if error: self.show_notice(error,True); return
                dialog=Gtk.Dialog(title='Connection: '+profile['name'],transient_for=self.window,modal=True)
                self.dialog_buttons(dialog,'Save Connection')
                box=dialog.get_content_area(); box.set_spacing(10)
                for setter in (box.set_margin_start,box.set_margin_end,box.set_margin_top,box.set_margin_bottom): setter(20)
                ipv4=data.get('ipv4',{})
                methods=['auto','manual','disabled']
                method=self.choice(['Automatic (DHCP)','Manual','Disabled']); method.set_selected(methods.index(ipv4.get('method')) if ipv4.get('method') in methods else 0)
                original=ipv4.get('address-data',[{}])
                address=original[0] if original else {}
                fields=[]
                for title,text in [('Address / prefix', str(address.get('address',''))+('/'+str(address['prefix']) if 'prefix' in address else '')),('Gateway',ipv4.get('gateway','')),('DNS servers',', '.join(ipv4.get('dns-data',[]))),('HTTPS proxy PAC URL',data.get('proxy',{}).get('pac-url',''))]:
                    box.append(self.label(title)); entry=Gtk.Entry(text=text);box.append(entry);fields.append(entry)
                box.prepend(method)
                problem=self.label('Changes apply when the connection is reactivated. IPv6 and unrelated settings are preserved.','muted');box.append(problem)
                def response(w,result):
                    if result==Gtk.ResponseType.ACCEPT:
                        values=[entry.get_text().strip() for entry in fields]
                        try: ipv4_settings(methods[method.get_selected()],*values[:3])
                        except ValueError as e: problem.set_text(str(e));return
                        self.operation(lambda:self.services.update_ipv4(profile['path'],methods[method.get_selected()],*values),'Connection saved; reconnect to apply')
                    w.destroy()
                dialog.connect('response',response);dialog.present()
            self.work(lambda:self.services.profile_settings(profile['path']),loaded)

        def import_vpn(self):
            chooser=Gtk.FileChooserNative.new('Import VPN Profile',self.window,Gtk.FileChooserAction.OPEN,'Import','Cancel')
            filter=Gtk.FileFilter();filter.set_name('OpenVPN or WireGuard profiles');filter.add_pattern('*.ovpn');filter.add_pattern('*.conf');chooser.add_filter(filter)
            def response(dialog,result):
                if result==Gtk.ResponseType.ACCEPT:
                    file=dialog.get_file();path=file.get_path() if file else None
                    if path: self.operation(lambda:self.services.import_vpn(path),'VPN profile imported')
                dialog.destroy()
            chooser.connect('response',response);chooser.show()
    return ExtraSettings
