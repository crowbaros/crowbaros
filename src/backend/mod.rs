//! Where frames go.
//!
//! The compositor asks a backend to draw an output only after something on
//! that output changed (see `redraw.rs`). A backend draws and says what
//! became of the frame.

pub mod headless;

use crate::State;

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
}
