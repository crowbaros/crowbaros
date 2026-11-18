//! Where frames go.
//!
//! The compositor asks a backend to draw an output only after something on
//! that output changed (see `redraw.rs`). A backend draws and says what
//! became of the frame.

pub mod headless;
#[cfg(feature = "nested")]
pub mod nested;
pub mod session;
pub mod tty;
pub mod uevent;

use crate::{State, geometry::Rect, image::Image};

/// The side of the square drawn where the pointer is.
pub const POINTER_SIZE: i32 = 12;
pub const POINTER_COLOUR: u32 = 0xffff_ffff;

/// Where the pointer's square is on an output, in the output's pixels, if
/// the pointer is on it.
pub fn pointer_rect(state: &State, output: u64) -> Option<Rect> {
    let output = state.outputs().iter().find(|candidate| candidate.number == output)?;
    let (x, y) = state.pointer_position();
    let at = (x as i32 - output.position.0, y as i32 - output.position.1);
    (at.0 >= 0 && at.1 >= 0 && at.0 < output.size.0 && at.1 < output.size.1)
        .then(|| Rect::new(at.0, at.1, POINTER_SIZE, POINTER_SIZE))
}

/// Draws the pointer into a frame of an output, as a white square, until
/// cursor images come in Milestone 3.
pub fn draw_pointer(state: &State, output: u64, image: &mut Image) {
    if let Some(rect) = pointer_rect(state, output) {
        image.fill(rect, POINTER_COLOUR);
    }
}
/// What became of one request to draw an output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drawn {
    /// A frame is on its way to the display, and the backend will call
    /// [`State::frame_shown`] when the display shows it.
    Flip,
    /// A frame went to the display, and the backend will not hear when it is
    /// shown.
    Sent,
    /// Nothing on the output had changed, or it could not be drawn. No frame
    /// went anywhere.
    Nothing,
}

pub trait Backend {
    /// Draws one output if what it shows has changed since the last frame.
    fn draw(&mut self, state: &State, output: u64) -> Drawn;

    /// A descriptor the backend watches with [`State::watch`] is ready.
    fn ready(&mut self, _state: &mut State, _source: u64) {}
}
