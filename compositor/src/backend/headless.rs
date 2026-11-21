//! A backend with no display: it draws each output with the CPU renderer and
//! keeps the last frame. The tests use it to see what the compositor shows.
//!
//! It decides whether to draw as the TTY backend does on a GPU: from the
//! output's damage, not from the pixels, so the tests count the frames a
//! display would be sent.

use std::collections::HashMap;

use crate::{
    State,
    backend::{Backend, Drawn},
    damage::DamageTracker,
    image::Image,
};

#[derive(Default)]
pub struct Headless {
    frames: HashMap<u64, Image>,
    damage: HashMap<u64, DamageTracker>,
}

impl Backend for Headless {
    /// A headless output shows any mode it says it has.
    fn set_mode(&mut self, state: &mut State, output: u64, mode: (i32, i32, i32)) -> Result<(), String> {
        state.set_output_mode(output, mode);
        Ok(())
    }

    fn draw(&mut self, state: &State, output: u64) -> Drawn {
        let Some(scene) = state.scene(output) else {
            return Drawn::Nothing;
        };
        let tracker = self.damage.entry(output).or_default();
        if !tracker.changes(state, &scene) {
            return Drawn::Nothing;
        }
        let Some(image) = state.render_output(output) else {
            return Drawn::Nothing;
        };
        tracker.damage(state, &scene, 1);
        self.frames.insert(output, image);
        Drawn::Sent
    }
}
