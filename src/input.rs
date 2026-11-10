//! Input from either backend. navi keeps quit/VT bindings, the fullscreen escape and
//! focused-window move/expand shortcuts before clients see them.

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent,
            GestureEndEvent, GestureSwipeUpdateEvent, InputBackend, InputEvent, KeyState,
            KeyboardKeyEvent, MouseButton, PointerAxisEvent, PointerButtonEvent,
            PointerMotionEvent, Switch, SwitchState, SwitchToggleEvent,
        },
        session::Session,
    },
    input::{
        keyboard::{FilterResult, Keycode, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};

use tracing::{info, warn};

use crate::{
    layout::{Action, Done, Drag, Layout, Rect, Side, WinId},
    monitors,
    state::{Navi, identity, px},
};

/// What a key navi keeps does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Binding {
    Quit,
    Lock,
    Vt(i32),
    Move(Side),
    Expand(Side),
    LeaveFullscreen,
}

/// Quit/VT bindings, Super+L, Super+Esc, Alt+Arrow to move, and Super+Arrow to expand. Extra
/// modifiers on the window shortcuts are left for apps, including Ctrl+Alt+Arrow and
/// Alt+Shift+Arrow.
pub fn binding(mods: &ModifiersState, sym: Keysym) -> Option<Binding> {
    if mods.logo && !mods.ctrl && !mods.alt && !mods.shift {
        match sym {
            Keysym::l => return Some(Binding::Lock),
            Keysym::Escape => return Some(Binding::LeaveFullscreen),
            _ => {}
        }
    }
    if mods.ctrl && mods.alt {
        let vt = sym.raw().wrapping_sub(Keysym::XF86_Switch_VT_1.raw());
        match sym {
            Keysym::BackSpace | Keysym::Terminate_Server => return Some(Binding::Quit),
            _ if vt < 12 => return Some(Binding::Vt(vt as i32 + 1)),
            _ => {}
        }
    }
    let side = match sym {
        Keysym::Left => Side::Left,
        Keysym::Right => Side::Right,
        Keysym::Up => Side::Top,
        Keysym::Down => Side::Bottom,
        _ => return None,
    };
    match (mods.ctrl, mods.alt, mods.logo, mods.shift) {
        (false, true, false, false) => Some(Binding::Move(side)),
        (false, false, true, false) => Some(Binding::Expand(side)),
        _ => None,
    }
}

/// Whether a key press is navi's: `Some(binding to act on)` keeps it from apps, `None` forwards
/// it. `held`: navi kept this key's earlier press. `usable`: the binding can act right now.
/// While locked every binding but VT and Lock is swallowed without acting.
fn keep(
    binding: Option<Binding>,
    locked: bool,
    held: bool,
    usable: bool,
) -> Option<Option<Binding>> {
    if locked && !matches!(binding, Some(Binding::Vt(_) | Binding::Lock)) {
        return (binding.is_some() || held).then_some(None);
    }
    if held {
        return Some(binding);
    }
    binding.filter(|_| usable).map(Some)
}

/// Whether a key press lifts the fullscreen re-entry guard on the window it reached: only a
/// non-modifier key that navi forwarded and that is no navi chord. A bare Super, or any part of
/// the next Super+Esc, must not let the app back into fullscreen. A delivered button press
/// always lifts it.
pub fn lifts_guard(sym: Keysym, binding: Option<Binding>, forwarded: bool) -> bool {
    forwarded && binding.is_none() && !sym.is_modifier_key()
}

/// The output Super+Esc ends fullscreen on, if a window is fullscreen there: the focused
/// window's (`focused` is its tile, a dialog's parent's), else the one under the pointer.
pub fn escape_output(
    layout: &Layout,
    focused: Option<WinId>,
    pointer: Option<&str>,
) -> Option<String> {
    let output = match focused {
        Some(id) => &layout.tile(id)?.output,
        None => pointer?,
    };
    layout.fullscreen_on(output).then(|| output.to_string())
}

/// A lone tap of Super: pressed with no other key or button down and released with none pressed
/// in between. It opens or closes the app menu; any chord cancels it.
#[derive(Debug, Default)]
pub struct SuperTap {
    armed: Option<u32>,
    buttons: Vec<u32>,
}

impl SuperTap {
    /// A key press or release, intercepted or not. `alone`, on a press: no other key is down.
    /// True on the release that completes a tap.
    pub fn key(&mut self, code: u32, sym: Keysym, pressed: bool, alone: bool) -> bool {
        if pressed {
            let super_key = matches!(sym, Keysym::Super_L | Keysym::Super_R);
            self.armed = (alone && self.buttons.is_empty() && super_key).then_some(code);
            return false;
        }
        self.armed.take_if(|c| *c == code).is_some()
    }

    /// A pointer button press or release.
    pub fn button(&mut self, code: u32, pressed: bool) {
        self.buttons.retain(|&b| b != code);
        if pressed {
            self.cancel();
            self.buttons.push(code);
        }
    }

    pub fn cancel(&mut self) {
        self.armed = None;
    }
}

/// Whether a Super tap toggles the app menu: only unlocked, with navi-bar connected, and with
/// no fullscreen window on the taskbar's output `bar_on`. `Err` is why not.
pub fn menu_tap(
    locked: bool,
    layout: &Layout,
    bar_on: Option<&str>,
    connected: bool,
) -> Result<(), &'static str> {
    if locked {
        Err("locked")
    } else if !connected {
        Err("no navi-bar")
    } else if bar_on.is_some_and(|o| layout.fullscreen_on(o)) {
        Err("fullscreen")
    } else {
        Ok(())
    }
}

impl Navi {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        self.last_activity = std::time::Instant::now();
        self.screen_off = false;
        self.sleep_requested = false;
        match event {
            InputEvent::SwitchToggle { event, .. }
                if event.switch() == Some(Switch::Lid) && event.state() == SwitchState::On =>
            {
                match self.desktop["lid_action"].as_str() {
                    Some("lock") => self.request_lock(None),
                    Some("suspend") if self.session.is_some() => {
                        crate::settings::request_sleep(self)
                    }
                    _ => {}
                }
            }
            InputEvent::GestureSwipeBegin { event, .. } => {
                self.swipe = (event.fingers() == 3
                    && self.desktop["swipe_moves"] == true
                    && !self.lock.active
                    && self.drag.is_none())
                .then_some((0.0, 0.0));
            }
            InputEvent::GestureSwipeUpdate { event, .. } => {
                if let Some((x, y)) = self.swipe.as_mut() {
                    *x += event.delta_x();
                    *y += event.delta_y();
                }
            }
            InputEvent::GestureSwipeEnd { event, .. } => {
                if let Some((x, y)) = self.swipe.take()
                    && !event.cancelled()
                    && !self.lock.active
                    && x.abs().max(y.abs()) >= 80.0
                    && let Some(id) = self.focused_tile()
                {
                    let side = if x.abs() > y.abs() {
                        if x < 0.0 { Side::Left } else { Side::Right }
                    } else if y < 0.0 {
                        Side::Top
                    } else {
                        Side::Bottom
                    };
                    if self.layout.keyboard_adjust(id, side, false).is_some() {
                        self.memory.sync(&self.layout);
                        self.apply_layout();
                    }
                }
            }
            InputEvent::Keyboard { event, .. } => {
                self.keyboard_key(event.key_code(), event.state(), Event::time_msec(&event));
            }
            InputEvent::PointerMotion { event, .. } => {
                let rects = self.output_rects();
                let p = self.pointer_location() + event.delta();
                let (x, y) = monitors::clamp((p.x, p.y), &rects);
                self.pointer_motion((x, y).into(), event.time_msec());
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                // The output under the pointer, as a tablet or touchscreen maps onto one.
                let here = self.pointer_location();
                let output = self
                    .space
                    .output_under(here)
                    .next()
                    .or(self.space.outputs().next());
                let Some(geo) = output.and_then(|o| self.space.output_geometry(o)) else {
                    return;
                };
                let pos = event.position_transformed(geo.size) + geo.loc.to_f64();
                self.pointer_motion(pos, event.time_msec());
            }
            InputEvent::PointerButton { event, .. } => {
                let left = event.button() == Some(MouseButton::Left);
                let pressed = event.state() == ButtonState::Pressed;
                self.pointer_button(event.button_code(), left, pressed, event.time_msec());
            }
            InputEvent::PointerAxis { event, .. } => {
                let Some(pointer) = self.seat.get_pointer() else {
                    return;
                };
                let source = event.source();
                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    let v120 = event.amount_v120(axis);
                    let amount = event
                        .amount(axis)
                        .unwrap_or_else(|| v120.unwrap_or(0.0) * 15.0 / 120.0);
                    if amount != 0.0 {
                        frame = frame.value(axis, amount);
                        if let Some(d) = v120 {
                            frame = frame.v120(axis, d as i32);
                        }
                    }
                    if source == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                        frame = frame.stop(axis);
                    }
                }
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }
}

impl Navi {
    /// The outputs' logical rects, by priority.
    fn output_rects(&self) -> Vec<Rect> {
        let mut outs: Vec<_> = self.space.outputs().cloned().collect();
        outs.sort_by_key(|o| self.priority_of(&identity(o)));
        outs.iter()
            .filter_map(|o| self.space.output_geometry(o))
            .map(|g| Rect {
                x: g.loc.x,
                y: g.loc.y,
                w: g.size.w,
                h: g.size.h,
            })
            .collect()
    }

    /// A key (an xkb keycode), from a backend or the debug replay.
    pub fn keyboard_key(&mut self, code: Keycode, state: KeyState, time: u32) {
        let (forward, releases) = if self.desktop["sticky_keys"] == true && !self.lock.active {
            self.sticky.event(code.raw(), state == KeyState::Pressed)
        } else {
            (true, Vec::new())
        };
        if forward {
            self.keyboard_key_inner(code, state, time);
        }
        for code in releases {
            self.keyboard_key_inner(code.into(), KeyState::Released, time);
        }
    }
    pub fn release_sticky(&mut self) {
        let codes = std::mem::take(&mut self.sticky).latched;
        self.swipe = None;
        // Releasing a latched Super isn't a tap.
        self.super_tap.cancel();
        for code in codes {
            self.keyboard_key_inner(code.into(), KeyState::Released, 0);
        }
    }
    fn keyboard_key_inner(&mut self, code: Keycode, state: KeyState, time: u32) {
        self.request_redraw();
        let Some(kb) = self.seat.get_keyboard() else {
            return;
        };
        let vt = self.session.is_some();
        let (mut sym, mut chord) = (Keysym::NoSymbol, None);
        let kept = kb.input::<Option<Binding>, _>(
            self,
            code,
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |navi, mods, key| {
                if state == KeyState::Released {
                    let held = navi.suppressed.iter().position(|&c| c == code);
                    return match held {
                        Some(i) => {
                            navi.suppressed.swap_remove(i);
                            FilterResult::Intercept(None)
                        }
                        None => FilterResult::Forward,
                    };
                }
                sym = key.modified_sym();
                let binding = binding(mods, sym);
                // Move/Expand are remapped; Super+Esc and Super+L never are.
                let binding = match binding {
                    Some(Binding::Move(side)) if navi.desktop["move_modifier"] == "super" => {
                        Some(Binding::Expand(side))
                    }
                    Some(Binding::Expand(side)) if navi.desktop["expand_modifier"] == "alt" => {
                        Some(Binding::Move(side))
                    }
                    other => other,
                };
                chord = binding;
                let usable = match binding {
                    Some(Binding::Vt(_)) => vt,
                    Some(Binding::Move(_) | Binding::Expand(_)) => {
                        navi.focused_tile().is_some() && navi.drag.is_none()
                    }
                    _ => true,
                };
                // Once a press is ours, repeats and release remain ours.
                let held = navi.suppressed.contains(&code);
                match keep(binding, navi.lock.active, held, usable) {
                    Some(b) => {
                        if !held {
                            navi.suppressed.push(code);
                        }
                        FilterResult::Intercept(b)
                    }
                    None => FilterResult::Forward,
                }
            },
        );
        let pressed = state == KeyState::Pressed;
        let alone = kb.pressed_keys().len() == 1;
        if self.super_tap.key(code.raw(), sym, pressed, alone) {
            self.super_tapped();
        }
        if pressed && lifts_guard(sym, chord, kept.is_none()) {
            let focus = kb.current_focus().and_then(|f| f.surface());
            self.user_pressed(focus);
        }
        match kept.flatten() {
            Some(Binding::LeaveFullscreen) => {
                let ended = self
                    .escape_target()
                    .map(|o| self.layout.unfullscreen_output(&o))
                    .unwrap_or_default();
                for &id in &ended {
                    info!(id, by = "keyboard", "fullscreen escape");
                    if let Some(m) = self.windows.iter_mut().find(|m| m.tile == Some(id)) {
                        m.want_fullscreen = false;
                        m.escaped = true;
                    }
                }
                if !ended.is_empty() {
                    self.apply_layout();
                }
            }
            Some(Binding::Lock) => self.request_lock(None),
            Some(Binding::Quit) => {
                info!("quit: Ctrl+Alt+Backspace");
                self.loop_signal.stop();
            }
            Some(Binding::Vt(n)) => {
                if let Some(Err(e)) = self.session.as_mut().map(|s| s.change_vt(n)) {
                    warn!(vt = n, "VT switch failed: {e}");
                }
            }
            Some(b @ (Binding::Move(side) | Binding::Expand(side))) => {
                if self.drag.is_none()
                    && let Some(id) = self.focused_tile()
                    && let Some(Done::Layout(what, tiles)) =
                        self.layout
                            .keyboard_adjust(id, side, matches!(b, Binding::Expand(_)))
                {
                    let cells: Vec<_> = tiles.iter().map(|t| (t.id, t.cells)).collect();
                    info!(id, by = "keyboard", ?cells, "{what}");
                    self.edge_clicks = Default::default();
                    self.memory.sync(&self.layout);
                    self.apply_layout();
                }
            }
            None => {}
        }
    }

    fn super_tapped(&mut self) {
        let bar = self.bar_on();
        let connected = self.bar_control.is_some();
        match menu_tap(self.lock.active, &self.layout, bar.as_deref(), connected) {
            Ok(()) => {
                if self
                    .bar_control
                    .as_ref()
                    .is_some_and(crate::shell::send_toggle)
                {
                    info!(by = "keyboard", "app menu toggle");
                }
            }
            Err(reason) => info!(reason, "app menu tap ignored"),
        }
    }

    /// Pointer motion to `pos`, from winit or the debug replay.
    pub fn pointer_motion(&mut self, pos: Point<f64, Logical>, time: u32) {
        self.request_redraw();
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let inside = self
            .space
            .outputs()
            .filter_map(|o| self.space.output_geometry(o))
            .any(|g| g.to_f64().contains(pos));
        let now = self.now_ms();
        self.edge_clicks.motion(px(pos));
        if !inside {
            // Off every output the release may never arrive; give up rather than stick.
            self.cancel_drag();
        } else if let Some(drag) = self.drag.as_mut()
            && drag.motion(&mut self.layout, px(pos), now)
        {
            self.apply_layout();
        }
        let under = self.pointer_focus(pos);
        let motion = MotionEvent {
            location: pos,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        };
        pointer.motion(self, under, &motion);
        pointer.frame(self);
        if self.lock.active {
            let s = self.pointer_focus(pos).map(|(s, _)| s);
            if s.is_some() {
                self.focus_lock_surface(s);
            }
        } else {
            self.bar_hover(pos);
        }
    }

    /// A pointer button, from winit or the debug replay.
    pub fn pointer_button(&mut self, button: u32, left: bool, pressed: bool, time: u32) {
        self.request_redraw();
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let pos = pointer.current_location();
        self.super_tap.button(button, pressed);
        if !left {
            self.edge_clicks = Default::default();
        }
        if let Some(drag) = self.drag.take() {
            if left && !pressed {
                let now = self.now_ms();
                let edge = drag.edge_click(&self.layout, px(pos));
                let done = drag.release(&mut self.layout, px(pos), now);
                let edge = edge.filter(|_| done.is_none());
                let expanded = self
                    .edge_clicks
                    .release(&mut self.layout, edge, px(pos), now);
                let done = done.or(expanded);
                match &done {
                    Some(Done::Layout(what, tiles)) => {
                        let tiles: Vec<_> = tiles.iter().map(|t| (t.id, &t.cells)).collect();
                        info!(?tiles, "{what}");
                        self.memory.sync(&self.layout);
                    }
                    Some(Done::Click(action, id)) => info!(id, by = "navi", "{}", action.log()),
                    None => {}
                }
                // Also hands the pointer back to whatever is under it now.
                self.apply_layout();
                match done {
                    // The app decides; navi never kills it.
                    Some(Done::Click(Action::Close, id)) => {
                        self.close_window(id);
                    }
                    Some(Done::Click(Action::Activate | Action::Restore, id)) => {
                        let shown = self.window_of(id).filter(|_| self.layout.visible(id));
                        if shown.is_some() {
                            self.focus(shown);
                        }
                    }
                    _ => {}
                }
            } else {
                self.drag = Some(drag);
            }
            return;
        }
        if !self.lock.active
            && pressed
            && !pointer.is_grabbed()
            && let Some(layer) = self.layer_at(pos)
        {
            self.click_layer(&layer);
        } else if !self.lock.active && pressed && !pointer.is_grabbed() {
            let hit = self.layout.hit(px(pos));
            // Keyboard focus follows click, in a tile's zones too, to the window shown there.
            let clicked = match hit {
                Some(h) => self
                    .layout
                    .active(h.id())
                    .filter(|&id| self.layout.visible(id))
                    .and_then(|id| self.window_of(id)),
                None => self.space.element_under(pos).map(|(w, _)| w.clone()),
            };
            self.focus(clicked);
            // Other buttons go on with no pointer focus over a zone, so neither the press nor
            // its release reaches a client.
            if hit.is_some() && left {
                self.drag = Drag::press(&self.layout, px(pos));
                return;
            }
        }
        self.edge_clicks = Default::default();
        if pressed {
            self.user_pressed(pointer.current_focus());
        }
        let state = match pressed {
            true => ButtonState::Pressed,
            false => ButtonState::Released,
        };
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state,
                // After any focus change above: a click that focuses a surface carries a serial
                // from its new focus period, so it can ask for an activation token.
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    /// The window losing focus mid-drag may swallow the release: cancel the drag.
    pub fn cancel_drag(&mut self) {
        self.request_redraw();
        self.edge_clicks = Default::default();
        if let Some(d) = self.drag.take() {
            info!("drag cancelled");
            if d.cancel(&mut self.layout) {
                self.apply_layout();
            }
        }
    }
}

/// One-shot modifiers: tap a modifier, then press a key. Tap it again to release it.
#[derive(Default)]
pub struct Sticky {
    held: Vec<(u32, bool)>,
    latched: Vec<u32>,
    ignored: Vec<u32>,
}
impl Sticky {
    fn event(&mut self, code: u32, pressed: bool) -> (bool, Vec<u32>) {
        if [50, 62, 37, 105, 64, 108, 133, 134].contains(&code) {
            if pressed {
                if let Some(at) = self.latched.iter().position(|c| *c == code) {
                    self.latched.remove(at);
                    self.ignored.push(code);
                    return (false, vec![code]);
                }
                if !self.held.iter().any(|(c, _)| *c == code) {
                    self.held.push((code, false));
                }
            } else {
                if let Some(at) = self.ignored.iter().position(|c| *c == code) {
                    self.ignored.remove(at);
                    return (false, Vec::new());
                }
                if let Some(at) = self.held.iter().position(|(c, _)| *c == code) {
                    let (_, used) = self.held.remove(at);
                    if !used {
                        self.latched.push(code);
                        return (false, Vec::new());
                    }
                }
            }
            (true, Vec::new())
        } else if pressed {
            for (_, used) in &mut self.held {
                *used = true;
            }
            (true, Vec::new())
        } else {
            (true, std::mem::take(&mut self.latched))
        }
    }
}
#[cfg(test)]
mod accessibility_tests {
    use super::*;
    #[test]
    fn sticky_keys_latch_once_toggle_and_preserve_held_chords() {
        let mut s = Sticky::default();
        assert_eq!(s.event(50, true), (true, vec![]));
        assert_eq!(s.event(50, false), (false, vec![]));
        assert_eq!(s.event(38, true), (true, vec![]));
        assert_eq!(s.event(38, false), (true, vec![50]));
        s.event(50, true);
        s.event(50, false);
        assert_eq!(s.event(50, true), (false, vec![50]));
        assert_eq!(s.event(50, false), (false, vec![]));
        s.event(37, true);
        s.event(38, true);
        assert_eq!(s.event(37, false), (true, vec![]));
        assert_eq!(s.event(38, false), (true, vec![]));
    }

    #[test]
    fn sticky_super_tap_latches_and_never_opens_the_menu() {
        let (mut s, mut tap) = (Sticky::default(), SuperTap::default());
        // keyboard_key: the detector sees what Sticky forwards, then the releases it hands back.
        let mut feed = |code: u32, sym, pressed| {
            let (forward, releases) = s.event(code, pressed);
            let mut fired = forward && tap.key(code, sym, pressed, true);
            for c in releases {
                fired |= tap.key(c, Keysym::NoSymbol, false, true);
            }
            (forward, fired)
        };
        assert_eq!(feed(133, Keysym::Super_L, true), (true, false));
        assert_eq!(
            feed(133, Keysym::NoSymbol, false),
            (false, false),
            "latched"
        );
        // The latched Super then applies to the next key and is released after it.
        assert_eq!(feed(38, Keysym::a, true), (true, false));
        assert_eq!(feed(38, Keysym::NoSymbol, false), (true, false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, alt: bool) -> ModifiersState {
        ModifiersState {
            ctrl,
            alt,
            ..Default::default()
        }
    }

    const SUPER_L: u32 = 133;
    const SUPER_R: u32 = 134;

    /// Super press, the events in between, then Super release: whether the tap fired.
    fn tap(between: impl FnOnce(&mut SuperTap)) -> bool {
        let mut t = SuperTap::default();
        assert!(!t.key(SUPER_L, Keysym::Super_L, true, true));
        between(&mut t);
        t.key(SUPER_L, Keysym::NoSymbol, false, true)
    }

    #[test]
    fn a_lone_super_tap_fires_once() {
        let mut t = SuperTap::default();
        for (code, sym) in [(SUPER_L, Keysym::Super_L), (SUPER_R, Keysym::Super_R)] {
            assert!(!t.key(code, sym, true, true));
            assert!(t.key(code, Keysym::NoSymbol, false, true), "{sym:?}");
            assert!(!t.key(code, Keysym::NoSymbol, false, true), "once");
        }
        // Pointer motion and scrolling never reach it, and there is no time limit.
        assert!(tap(|_| {}));
    }

    #[test]
    fn any_other_press_cancels_the_tap() {
        for (code, sym) in [
            (38, Keysym::a),
            (46, Keysym::l),
            (9, Keysym::Escape),
            (113, Keysym::Left),
            (SUPER_R, Keysym::Super_R),
        ] {
            assert!(
                !tap(|t| {
                    t.key(code, sym, true, false);
                    t.key(code, Keysym::NoSymbol, false, false);
                }),
                "{sym:?}"
            );
        }
        // Super+L released in the other order: Super first, then L.
        let mut t = SuperTap::default();
        t.key(SUPER_L, Keysym::Super_L, true, true);
        t.key(46, Keysym::l, true, false);
        assert!(!t.key(SUPER_L, Keysym::NoSymbol, false, false));
        assert!(!t.key(46, Keysym::NoSymbol, false, true));
        assert!(!tap(|t| t.button(0x110, true)), "pointer button");
    }

    #[test]
    fn super_with_something_already_down_is_no_tap() {
        let mut t = SuperTap::default();
        assert!(!t.key(SUPER_L, Keysym::Super_L, true, false), "a key");
        assert!(!t.key(SUPER_L, Keysym::NoSymbol, false, true));
        t.button(0x110, true);
        assert!(!t.key(SUPER_L, Keysym::Super_L, true, true), "a button");
        assert!(!t.key(SUPER_L, Keysym::NoSymbol, false, true));
        // Its release clears it.
        t.button(0x110, false);
        t.key(SUPER_L, Keysym::Super_L, true, true);
        assert!(t.key(SUPER_L, Keysym::NoSymbol, false, true));
    }

    #[test]
    fn stray_releases_never_fire_and_a_new_tap_works() {
        let mut t = SuperTap::default();
        assert!(
            !t.key(SUPER_L, Keysym::NoSymbol, false, true),
            "nothing armed"
        );
        t.key(SUPER_L, Keysym::Super_L, true, true);
        assert!(!t.key(38, Keysym::NoSymbol, false, true), "another keycode");
        assert!(t.key(SUPER_L, Keysym::NoSymbol, false, true), "still armed");
        // After a cancelled tap.
        t.key(SUPER_L, Keysym::Super_L, true, true);
        t.key(38, Keysym::a, true, false);
        assert!(!t.key(SUPER_L, Keysym::NoSymbol, false, false));
        t.key(SUPER_L, Keysym::Super_L, true, true);
        assert!(t.key(SUPER_L, Keysym::NoSymbol, false, true));
    }

    #[test]
    fn menu_tap_needs_unlocked_bar_and_no_fullscreen_under_the_bar() {
        let (mut l, uw, k) = two_outputs();
        let bar = Some("uw");
        assert_eq!(menu_tap(false, &l, bar, true), Ok(()));
        assert_eq!(menu_tap(true, &l, bar, true), Err("locked"));
        assert_eq!(menu_tap(false, &l, bar, false), Err("no navi-bar"));
        // Fullscreen on another output than the taskbar's: still a toggle.
        assert!(l.fullscreen(k));
        assert_eq!(menu_tap(false, &l, bar, true), Ok(()));
        assert!(l.fullscreen(uw));
        assert_eq!(menu_tap(false, &l, bar, true), Err("fullscreen"));
        assert_eq!(menu_tap(true, &l, bar, true), Err("locked"));
    }

    #[test]
    fn ac10_bindings() {
        let ca = mods(true, true);
        assert_eq!(binding(&ca, Keysym::BackSpace), Some(Binding::Quit));
        assert_eq!(binding(&ca, Keysym::XF86_Switch_VT_3), Some(Binding::Vt(3)));
        assert_eq!(binding(&ca, Keysym::XF86_Switch_VT_1), Some(Binding::Vt(1)));
        assert_eq!(
            binding(&ca, Keysym::XF86_Switch_VT_12),
            Some(Binding::Vt(12))
        );
        assert_eq!(binding(&mods(false, false), Keysym::BackSpace), None);
        assert_eq!(binding(&mods(true, false), Keysym::BackSpace), None);
        assert_eq!(binding(&mods(false, true), Keysym::BackSpace), None);
        assert_eq!(binding(&mods(false, false), Keysym::XF86_Switch_VT_3), None);
        for other in [Keysym::F3, Keysym::Delete, Keysym::a, Keysym::XF86_Ungrab] {
            assert_eq!(binding(&ca, other), None, "{other:?}");
        }
    }

    #[test]
    fn window_shortcuts_use_only_alt_or_super_and_all_four_arrows() {
        for (sym, side) in [
            (Keysym::Left, Side::Left),
            (Keysym::Right, Side::Right),
            (Keysym::Up, Side::Top),
            (Keysym::Down, Side::Bottom),
        ] {
            for mask in 0..16 {
                let mods = ModifiersState {
                    ctrl: mask & 1 != 0,
                    alt: mask & 2 != 0,
                    logo: mask & 4 != 0,
                    shift: mask & 8 != 0,
                    ..Default::default()
                };
                let expected = match mask {
                    2 => Some(Binding::Move(side)),
                    4 => Some(Binding::Expand(side)),
                    _ => None,
                };
                assert_eq!(binding(&mods, sym), expected, "{sym:?}, mask {mask}");
            }
        }
    }

    fn masks() -> impl Iterator<Item = (u8, ModifiersState)> {
        (0..16).map(|mask| {
            let mods = ModifiersState {
                ctrl: mask & 1 != 0,
                alt: mask & 2 != 0,
                logo: mask & 4 != 0,
                shift: mask & 8 != 0,
                ..Default::default()
            };
            (mask, mods)
        })
    }

    #[test]
    fn super_escape_is_only_super_escape() {
        for (mask, mods) in masks() {
            let expected = (mask == 4).then_some(Binding::LeaveFullscreen);
            assert_eq!(binding(&mods, Keysym::Escape), expected, "mask {mask}");
        }
        let logo = masks().nth(4).unwrap().1;
        assert_eq!(binding(&logo, Keysym::l), Some(Binding::Lock));
    }

    /// Yoshi's ultrawide and the 4K above it, with a window on each.
    fn two_outputs() -> (Layout, WinId, WinId) {
        let out = |name: &str, y, w, h| crate::layout::Output {
            name: name.into(),
            area: Rect { x: 0, y, w, h },
            full: Rect { x: 0, y, w, h },
            priority: 0,
        };
        let mut l = Layout::default();
        l.set_outputs(vec![out("uw", 2160, 3440, 1440), out("4k", 0, 3840, 2160)]);
        let (uw, _) = l.place("a", (100, 2200)).unwrap();
        let (k, _) = l.place("b", (100, 100)).unwrap();
        assert_eq!(l.tile(uw).unwrap().output, "uw");
        assert_eq!(l.tile(k).unwrap().output, "4k");
        (l, uw, k)
    }

    #[test]
    fn escape_acts_on_the_focused_monitor() {
        let (mut l, uw, k) = two_outputs();
        assert!(l.fullscreen(k));
        // Focused window (or a dialog's parent tile) on the fullscreen output: intercept.
        assert_eq!(escape_output(&l, Some(k), Some("uw")), Some("4k".into()));
        // Focused window elsewhere: forward, whatever the pointer says.
        assert_eq!(escape_output(&l, Some(uw), Some("4k")), None);
        assert!(l.is_fullscreen(k));
        // No focused toplevel: the pointer's output decides.
        assert_eq!(escape_output(&l, None, Some("4k")), Some("4k".into()));
        assert_eq!(escape_output(&l, None, Some("uw")), None);
        assert_eq!(escape_output(&l, None, None), None);
        // Nothing fullscreen: forward.
        assert!(l.unfullscreen(k));
        assert_eq!(escape_output(&l, Some(k), Some("4k")), None);
    }

    #[test]
    fn locked_super_escape_is_swallowed_and_does_nothing() {
        let esc = Some(Binding::LeaveFullscreen);
        // Unlocked: acts when usable, else reaches the app.
        assert_eq!(keep(esc, false, false, true), Some(esc));
        assert_eq!(keep(esc, false, false, false), None);
        // Locked: kept from every client, acted on by nobody.
        for usable in [true, false] {
            assert_eq!(keep(esc, true, false, usable), Some(None));
            assert_eq!(keep(esc, true, true, usable), Some(None));
        }
        // The lock still lets VT and Lock act, and other keys through.
        assert_eq!(
            keep(Some(Binding::Lock), true, false, true),
            Some(Some(Binding::Lock))
        );
        assert_eq!(keep(None, true, false, true), None);
        // A held key stays navi's until released.
        assert_eq!(keep(None, false, true, true), Some(None));
    }

    #[test]
    fn only_a_forwarded_non_modifier_key_lifts_the_escape_guard() {
        // Bare modifiers reach the app but never lift it: Super (Super+Esc's first half, and
        // a bare tap that may open the menu), Alt, Ctrl, Shift, AltGr and the locks.
        for sym in [
            Keysym::Super_L,
            Keysym::Super_R,
            Keysym::Alt_L,
            Keysym::Control_R,
            Keysym::Shift_L,
            Keysym::ISO_Level3_Shift,
            Keysym::Caps_Lock,
            Keysym::Num_Lock,
        ] {
            assert!(!lifts_guard(sym, None, true), "{sym:?}");
        }
        // Keys navi kept: Super+Esc itself and the window shortcuts.
        let esc = Some(Binding::LeaveFullscreen);
        assert!(!lifts_guard(Keysym::Escape, esc, false));
        assert!(!lifts_guard(
            Keysym::Right,
            Some(Binding::Move(Side::Right)),
            false
        ));
        assert!(!lifts_guard(Keysym::l, Some(Binding::Lock), false));
        // A navi chord that reached the app anyway (Alt+Right with no focused tile).
        assert!(!lifts_guard(
            Keysym::Right,
            Some(Binding::Move(Side::Right)),
            true
        ));
        assert!(!lifts_guard(Keysym::Escape, esc, true));
        // An ordinary key the app received, a bare Esc included.
        assert!(lifts_guard(Keysym::a, None, true));
        assert!(lifts_guard(Keysym::Escape, None, true));
    }
}
