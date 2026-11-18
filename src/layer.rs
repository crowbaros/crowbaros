//! The shell's layer surfaces (`zwlr_layer_shell_v1`). Until they are built,
//! no surface is a layer, and every output's whole area is usable.

use crate::{State, geometry::Rect, image::Image, surface::SurfaceId};

impl State {
    /// The desktop rectangle a mapped layer surface covers.
    pub(crate) fn layer_rect(&self, _surface: SurfaceId) -> Option<Rect> {
        None
    }

    /// After a commit on a layer surface. None exist yet.
    pub(crate) fn layer_committed(&mut self, _surface: SurfaceId) -> Result<(), crate::client::ProtocolError> {
        Ok(())
    }

    /// A layer surface is gone.
    pub(crate) fn forget_layer(&mut self, _surface: SurfaceId) {}

    /// Every mapped layer surface.
    pub(crate) fn layer_surfaces(&self) -> Vec<SurfaceId> {
        Vec::new()
    }

    /// Draws an output's layers: the background and bottom layers below the
    /// windows, or the top and overlay layers above them.
    pub(crate) fn draw_layers(&self, _output: u64, _image: &mut Image, _upper: bool) {}

    /// The part of an output that the shell's layers have not reserved, on
    /// the desktop.
    pub fn usable_area(&self, output: u64) -> Rect {
        self.outputs
            .iter()
            .find(|candidate| candidate.number == output)
            .map_or(Rect::default(), |output| output.rect())
    }
}
