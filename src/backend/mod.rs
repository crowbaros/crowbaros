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
const POINTER_COLOUR: u32 = 0xffff_ffff;

/// Draws the pointer into a frame of an output, as a white square, until
/// cursor images come in Milestone 3.
pub fn draw_pointer(state: &State, output: u64, image: &mut Image) {
    let Some(position) = state
        .outputs()
        .iter()
        .find(|candidate| candidate.number == output)
        .map(|output| output.position)
    else {
        return;
    };
    let (x, y) = state.pointer_position();
    let at = (x as i32 - position.0, y as i32 - position.1);
    if at.0 >= 0 && at.1 >= 0 && at.0 < image.width && at.1 < image.height {
        image.fill(Rect::new(at.0, at.1, POINTER_SIZE, POINTER_SIZE), POINTER_COLOUR);
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
