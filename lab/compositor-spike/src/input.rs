use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, InputTime, KeyState,
        KeyboardKeyEvent, Keycode, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, Keysym},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{state::State, windows::root_surface};

impl State {
    /// Every key from every backend enters here. `keycode` is the XKB
    /// keycode, which is the evdev code plus 8.
    ///
    /// The confirm chord's key is taken here and never given to Smithay's
    /// keyboard. Intercepting it in Smithay's filter is not enough: Smithay
    /// still records the key as pressed, and tells a keyboard object that a
    /// client creates while it is held.
    pub fn handle_key(&mut self, keycode: Keycode, state: KeyState, time: InputTime) {
        if state == KeyState::Pressed {
            // A key that goes down again was released, whether or not its
            // release arrived. It is no longer withheld.
            self.withheld_keys.remove(&keycode);
        }
        match state {
            KeyState::Pressed if self.completes_confirm_chord(keycode) => {
                self.withheld_keys.insert(keycode);
                self.confirms_taken += 1;
                self.send_event(crate::control::Event::Confirm);
                tracing::info!("confirm chord taken by the compositor");
                return;
            }
            // A withheld press withholds its release, even if the modifiers
            // went up first.
            KeyState::Released if self.withheld_keys.remove(&keycode) => return,
            _ => {}
        }
        let serial = SERIAL_COUNTER.next_serial();
        self.seat
            .get_keyboard()
            .unwrap()
            .input::<(), _>(self, keycode, state, serial, time, |_, _, _| FilterResult::Forward);
    }

    /// Whether pressing this key now makes the confirm chord: Enter while
    /// Ctrl and Alt are down.
    fn completes_confirm_chord(&mut self, keycode: Keycode) -> bool {
        let keyboard = self.seat.get_keyboard().unwrap();
        let modifiers = keyboard.modifier_state();
        if !(modifiers.ctrl && modifiers.alt) {
            return false;
        }
        keyboard.with_xkb_state(self, |context| {
            let xkb = context.xkb().lock().unwrap();
            // Safety: the state is read here and not kept past the lock.
            unsafe { xkb.state() }.key_get_one_sym(keycode) == Keysym::Return
        })
    }

    fn clamp_to_outputs(&self, pos: Point<f64, Logical>) -> Point<f64, Logical> {
        let Some(bounds) = self.desktop_bounds() else {
            return pos;
        };
        Point::from((
            pos.x
                .clamp(bounds.loc.x as f64, (bounds.loc.x + bounds.size.w - 1) as f64),
            pos.y
                .clamp(bounds.loc.y as f64, (bounds.loc.y + bounds.size.h - 1) as f64),
        ))
    }

    fn move_pointer(&mut self, pos: Point<f64, Logical>, time: InputTime) {
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();
        let under = self.surface_under(pos);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// A click gives the keyboard to the window under the pointer.
    fn focus_under_pointer(&mut self) {
        let pos = self.seat.get_pointer().unwrap().current_location();
        let Some((surface, _)) = self.surface_under(pos) else {
            return;
        };
        let root = root_surface(&surface);
        if let Some(window) = self.managed(&root).map(|m| m.window.clone()) {
            self.focus_window(Some(&window));
        }
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                self.handle_key(event.key_code(), event.state(), Event::time(&event));
            }
            InputEvent::PointerMotion { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                let pos = self.clamp_to_outputs(pointer.current_location() + event.delta());
                self.move_pointer(pos, event.time());
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                // An absolute device covers the whole desktop.
                let Some(bounds) = self.desktop_bounds() else {
                    return;
                };
                let pos = event.position_transformed(bounds.size) + bounds.loc.to_f64();
                self.move_pointer(pos, event.time());
            }
            InputEvent::PointerButton { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                let serial = SERIAL_COUNTER.next_serial();
                let button_state = event.state();

                if ButtonState::Pressed == button_state && !pointer.is_grabbed() {
                    self.focus_under_pointer();
                }

                pointer.button(
                    self,
                    &ButtonEvent {
                        button: event.button_code(),
                        state: button_state,
                        serial,
                        time: event.time(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event, .. } => {
                let source = event.source();

                let horizontal_amount = event
                    .amount(Axis::Horizontal)
                    .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.);
                let vertical_amount = event
                    .amount(Axis::Vertical)
                    .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.);
                let horizontal_amount_discrete = event.amount_v120(Axis::Horizontal);
                let vertical_amount_discrete = event.amount_v120(Axis::Vertical);

                let mut frame = AxisFrame::new(event.time()).source(source);
                if horizontal_amount != 0.0 {
                    frame = frame.value(Axis::Horizontal, horizontal_amount);
                    if let Some(discrete) = horizontal_amount_discrete {
                        frame = frame.v120(Axis::Horizontal, discrete as i32);
                    }
                }
                if vertical_amount != 0.0 {
                    frame = frame.value(Axis::Vertical, vertical_amount);
                    if let Some(discrete) = vertical_amount_discrete {
                        frame = frame.v120(Axis::Vertical, discrete as i32);
                    }
                }

                if source == AxisSource::Finger {
                    if event.amount(Axis::Horizontal) == Some(0.0) {
                        frame = frame.stop(Axis::Horizontal);
                    }
                    if event.amount(Axis::Vertical) == Some(0.0) {
                        frame = frame.stop(Axis::Vertical);
                    }
                }

                let pointer = self.seat.get_pointer().unwrap();
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }
}
