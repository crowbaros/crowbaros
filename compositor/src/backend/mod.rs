//! Where frames go.
//!
//! The compositor asks a backend to draw an output only after something on
//! that output changed (see `redraw.rs`). A backend draws what changed and
//! says what became of it.

use smithay::{backend::allocator::dmabuf::Dmabuf, output::Output};

use crate::State;

pub mod tty;
pub mod winit;

/// The colour behind everything, until the shell's wallpaper covers it.
pub const BACKGROUND: [f32; 4] = [0.1, 0.1, 0.1, 1.0];

/// What became of one request to draw an output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drawn {
    /// A frame is on its way to the display, and the backend will call
    /// [`State::frame_shown`] when the display shows it.
    Flip,
    /// A frame went to the display, and the backend will not hear when it is
    /// shown.
    Shown,
    /// Nothing on the output had changed, or it could not be drawn. No frame
    /// went anywhere.
    Nothing,
}

pub trait Backend {
    /// Draws one output if what it shows has changed since the last frame.
    fn draw(&mut self, state: &State, output: &Output) -> Drawn;

    /// Takes a client's dmabuf into the renderer. A backend with no GPU
    /// refuses.
    fn import_dmabuf(&mut self, _dmabuf: &Dmabuf) -> bool {
        false
    }
}
