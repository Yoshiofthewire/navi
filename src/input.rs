//! Input from either backend. navi keeps quit/VT bindings and focused-window move/expand
//! shortcuts before clients see them.

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
    layout::{Action, Done, Drag, Rect, Side},
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
}

/// Quit/VT bindings, Alt+Arrow to move, and Super+Arrow to expand. Extra modifiers on the
/// window shortcuts are left for apps, including Ctrl+Alt+Arrow and Alt+Shift+Arrow.
pub fn binding(mods: &ModifiersState, sym: Keysym) -> Option<Binding> {
    if mods.logo && !mods.ctrl && !mods.alt && !mods.shift && sym == Keysym::l {
        return Some(Binding::Lock);
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
                let binding = binding(mods, key.modified_sym());
                let binding = match binding {
                    Some(Binding::Move(side)) if navi.desktop["move_modifier"] == "super" => {
                        Some(Binding::Expand(side))
                    }
                    Some(Binding::Expand(side)) if navi.desktop["expand_modifier"] == "alt" => {
                        Some(Binding::Move(side))
                    }
                    other => other,
                };
                if navi.lock.active && !matches!(binding, Some(Binding::Vt(_) | Binding::Lock)) {
                    if binding.is_some() || navi.suppressed.contains(&code) {
                        if !navi.suppressed.contains(&code) {
                            navi.suppressed.push(code);
                        }
                        return FilterResult::Intercept(None);
                    }
                    return FilterResult::Forward;
                }
                // Once a press is ours, repeats and release remain ours.
                if navi.suppressed.contains(&code) {
                    return FilterResult::Intercept(binding);
                }
                match binding {
                    Some(Binding::Vt(_)) if !vt => FilterResult::Forward,
                    Some(Binding::Move(_) | Binding::Expand(_))
                        if navi.focused_tile().is_none() || navi.drag.is_some() =>
                    {
                        FilterResult::Forward
                    }
                    Some(b) => {
                        navi.suppressed.push(code);
                        FilterResult::Intercept(Some(b))
                    }
                    None => FilterResult::Forward,
                }
            },
        );
        match kept.flatten() {
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
        let serial = SERIAL_COUNTER.next_serial();
        let pos = pointer.current_location();
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
        let state = match pressed {
            true => ButtonState::Pressed,
            false => ButtonState::Released,
        };
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state,
                serial,
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
}
