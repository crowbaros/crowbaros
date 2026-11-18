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

use crate::State;

/// The side of the square drawn where the pointer is, when no client has
/// set an image for it (see `cursor.rs`).
pub const POINTER_SIZE: i32 = 12;
pub const POINTER_COLOUR: u32 = 0xffff_ffff;

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

    /// Shows an output in another of its modes: width, height, and refresh
    /// in millihertz.
    fn set_mode(&mut self, _state: &mut State, _output: u64, _mode: (i32, i32, i32)) -> Result<(), String> {
        Err("this backend shows each output in one mode".into())
    }
}
