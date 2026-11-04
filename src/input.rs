use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyboardKeyEvent, MouseButton, PointerAxisEvent, PointerButtonEvent,
    },
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};

use tracing::info;

use crate::{
    layout::Drag,
    state::{Navi, px},
};

impl Navi {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                let Some(kb) = self.seat.get_keyboard() else {
                    return;
                };
                kb.input::<(), _>(
                    self,
                    event.key_code(),
                    event.state(),
                    SERIAL_COUNTER.next_serial(),
                    Event::time_msec(&event),
                    |_, _, _| FilterResult::Forward,
                );
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let Some(geo) = self
                    .space
                    .outputs()
                    .next()
                    .and_then(|o| self.space.output_geometry(o))
                else {
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
    }

    /// A pointer button, from winit or the debug replay.
    pub fn pointer_button(&mut self, button: u32, left: bool, pressed: bool, time: u32) {
        let (Some(pointer), Some(kb)) = (self.seat.get_pointer(), self.seat.get_keyboard()) else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let pos = pointer.current_location();
        if let Some(drag) = self.drag.take() {
            if left && !pressed {
                let now = self.now_ms();
                if let Some((what, tiles)) = drag.release(&mut self.layout, px(pos), now) {
                    let tiles: Vec<_> = tiles.iter().map(|t| (t.id, &t.cells)).collect();
                    info!(?tiles, "{what}");
                }
                // Also hands the pointer back to whatever is under it now.
                self.apply_layout();
            } else {
                self.drag = Some(drag);
            }
            return;
        }
        if pressed && !pointer.is_grabbed() {
            let hit = self.layout.hit(px(pos));
            // Keyboard focus follows click, in a tile's zones too.
            let clicked = match hit {
                Some(h) => self
                    .windows
                    .iter()
                    .find(|m| m.tile == Some(h.id()))
                    .map(|m| m.window.clone()),
                None => self.space.element_under(pos).map(|(w, _)| w.clone()),
            };
            let focus = clicked.as_ref().and_then(|w| {
                self.space.raise_element(w, true);
                w.toplevel().map(|t| t.wl_surface().clone())
            });
            if clicked.is_none() {
                self.space.elements().for_each(|w| {
                    w.set_activated(false);
                });
            }
            kb.set_focus(self, focus, serial);
            self.space.elements().for_each(|w| {
                if let Some(t) = w.toplevel() {
                    t.send_pending_configure();
                }
            });
            // Other buttons go on with no pointer focus over a zone, so neither the press nor
            // its release reaches a client.
            if hit.is_some() && left {
                self.drag = Drag::press(&self.layout, px(pos));
                return;
            }
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
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// The window losing focus mid-drag may swallow the release: cancel the drag.
    pub fn cancel_drag(&mut self) {
        if let Some(d) = self.drag.take() {
            info!("drag cancelled");
            if d.cancel(&mut self.layout) {
                self.apply_layout();
            }
        }
    }
}
