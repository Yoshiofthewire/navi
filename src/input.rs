//! Input from either backend. navi keeps two bindings for itself, before any client sees them:
//! Ctrl+Alt+Backspace quits, and Ctrl+Alt+F1…F12 switch VTs on real hardware.

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
            KeyState, KeyboardKeyEvent, MouseButton, PointerAxisEvent, PointerButtonEvent,
            PointerMotionEvent,
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
    layout::{Action, Done, Drag, Rect},
    monitors,
    state::{Navi, identity, px},
};

/// What a key navi keeps does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Binding {
    Quit,
    Vt(i32),
}

/// navi's own bindings: Ctrl+Alt+Backspace, and Ctrl+Alt+F1…F12 as xkb gives them.
pub fn binding(mods: &ModifiersState, sym: Keysym) -> Option<Binding> {
    if !(mods.ctrl && mods.alt) {
        return None;
    }
    let vt = sym.raw().wrapping_sub(Keysym::XF86_Switch_VT_1.raw());
    match sym {
        Keysym::BackSpace | Keysym::Terminate_Server => Some(Binding::Quit),
        _ if vt < 12 => Some(Binding::Vt(vt as i32 + 1)),
        _ => None,
    }
}

impl Navi {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
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
                match binding(mods, key.modified_sym()) {
                    Some(Binding::Vt(_)) if !vt => FilterResult::Forward,
                    Some(b) => {
                        navi.suppressed.push(code);
                        FilterResult::Intercept(Some(b))
                    }
                    None => FilterResult::Forward,
                }
            },
        );
        match kept.flatten() {
            Some(Binding::Quit) => {
                info!("quit: Ctrl+Alt+Backspace");
                self.loop_signal.stop();
            }
            Some(Binding::Vt(n)) => {
                if let Some(Err(e)) = self.session.as_mut().map(|s| s.change_vt(n)) {
                    warn!(vt = n, "VT switch failed: {e}");
                }
            }
            None => {}
        }
    }

    /// Pointer motion to `pos`, from winit or the debug replay.
    pub fn pointer_motion(&mut self, pos: Point<f64, Logical>, time: u32) {
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
        self.bar_hover(pos);
    }

    /// A pointer button, from winit or the debug replay.
    pub fn pointer_button(&mut self, button: u32, left: bool, pressed: bool, time: u32) {
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
        if pressed
            && !pointer.is_grabbed()
            && let Some(layer) = self.layer_at(pos)
        {
            self.click_layer(&layer);
        } else if pressed && !pointer.is_grabbed() {
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
        self.edge_clicks = Default::default();
        if let Some(d) = self.drag.take() {
            info!("drag cancelled");
            if d.cancel(&mut self.layout) {
                self.apply_layout();
            }
        }
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
}
