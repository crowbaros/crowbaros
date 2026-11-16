//! Input from every backend enters the compositor here.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, InputTime, KeyState,
        KeyboardKeyEvent, Keycode, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    desktop::{WindowSurfaceType, layer_map_for_output},
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::shell::wlr_layer::Layer,
};

use crate::State;

impl State {
    /// Every key from every backend enters here. `keycode` is the XKB
    /// keycode, which is the evdev code plus 8.
    ///
    /// The confirm chord and the compositor's own keys are taken here, before
    /// Smithay's keyboard sees them, when issues 71 and 78 build them.
    pub fn handle_key(&mut self, keycode: Keycode, state: KeyState, time: InputTime) {
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.keyboard();
        keyboard.input::<(), _>(self, keycode, state, serial, time, |_, _, _| FilterResult::Forward);
    }

    /// The pointer is now at this place on the desktop.
    pub fn pointer_moved(&mut self, position: Point<f64, Logical>, time: InputTime) {
        let position = self.clamp_to_outputs(position);
        let pointer = self.pointer();
        let under = self.surface_under(position);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: position,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
        // A backend that draws the pointer has something new to draw.
        self.queue_redraw_all();
    }

    /// A pointer button went down or up. `button` is the evdev code. A press
    /// gives the keyboard to what is under the pointer.
    pub fn pointer_button(&mut self, button: u32, state: ButtonState, time: InputTime) {
        let pointer = self.pointer();
        if state == ButtonState::Pressed && !pointer.is_grabbed() {
            self.focus_under_pointer();
        }
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    fn clamp_to_outputs(&self, position: Point<f64, Logical>) -> Point<f64, Logical> {
        let Some(bounds) = self.desktop_bounds() else {
            return position;
        };
        let (left, top) = (f64::from(bounds.loc.x), f64::from(bounds.loc.y));
        Point::from((
            position.x.clamp(left, left + f64::from(bounds.size.w - 1)),
            position.y.clamp(top, top + f64::from(bounds.size.h - 1)),
        ))
    }

    /// The surface under a point: the shell's overlay and top layers, then
    /// windows, then its bottom and background layers.
    pub(crate) fn surface_under(&self, position: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.space.output_under(position).next()?;
        let output_origin = self.space.output_geometry(output)?.loc;
        let layers = layer_map_for_output(output);
        let on_layers = |kinds: [Layer; 2]| {
            kinds.into_iter().find_map(|kind| {
                let layer = layers.layer_under(kind, position - output_origin.to_f64())?;
                let layer_origin = layers.layer_geometry(layer)?.loc + output_origin;
                layer
                    .surface_under(position - layer_origin.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, at)| (surface, (at + layer_origin).to_f64()))
            })
        };
        on_layers([Layer::Overlay, Layer::Top])
            .or_else(|| {
                let (window, origin) = self.space.element_under(position)?;
                window
                    .surface_under(position - origin.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, at)| (surface, (at + origin).to_f64()))
            })
            .or_else(|| on_layers([Layer::Bottom, Layer::Background]))
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                self.handle_key(event.key_code(), event.state(), Event::time(&event));
            }
            InputEvent::PointerMotion { event, .. } => {
                let pointer = self.pointer();
                self.pointer_moved(pointer.current_location() + event.delta(), event.time());
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                // An absolute device covers the whole desktop.
                let Some(bounds) = self.desktop_bounds() else {
                    return;
                };
                let position = event.position_transformed(bounds.size) + bounds.loc.to_f64();
                self.pointer_moved(position, event.time());
            }
            InputEvent::PointerButton { event, .. } => {
                self.pointer_button(event.button_code(), event.state(), event.time());
            }
            InputEvent::PointerAxis { event, .. } => self.pointer_axis::<I>(event),
            _ => {}
        }
    }

    fn pointer_axis<I: InputBackend>(&mut self, event: I::PointerAxisEvent) {
        let mut frame = AxisFrame::new(event.time()).source(event.source());
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let steps = event.amount_v120(axis);
            let amount = event
                .amount(axis)
                .unwrap_or_else(|| steps.unwrap_or(0.0) * 15.0 / 120.0);
            if amount != 0.0 {
                frame = frame.value(axis, amount);
                if let Some(steps) = steps {
                    frame = frame.v120(axis, steps as i32);
                }
            }
            if event.source() == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                frame = frame.stop(axis);
            }
        }
        let pointer = self.pointer();
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}
