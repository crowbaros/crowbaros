//! A backend with no display: it draws each output with the CPU renderer and
//! keeps the last frame. The tests use it to see what the compositor shows.

use std::collections::HashMap;

use crate::{
    State,
    backend::{Backend, Drawn},
    image::Image,
};

#[derive(Default)]
pub struct Headless {
    frames: HashMap<u64, Image>,
}

impl Backend for Headless {
    /// A headless output shows any mode it says it has.
    fn set_mode(&mut self, state: &mut State, output: u64, mode: (i32, i32, i32)) -> Result<(), String> {
        state.set_output_mode(output, mode);
        Ok(())
    }

    fn draw(&mut self, state: &State, output: u64) -> Drawn {
        let Some(image) = state.render_output(output) else {
            return Drawn::Nothing;
        };
        // A frame that is the same as the last one changes nothing on the
        // display, as damage tracking finds.
        if self.frames.get(&output) == Some(&image) {
            return Drawn::Nothing;
        }
        self.frames.insert(output, image);
        Drawn::Sent
    }
}
