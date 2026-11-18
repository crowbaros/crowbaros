//! What an output shows, back to front: the background, then each surface
//! in the order it is drawn, where it is drawn, at the size it is shown.
//!
//! The CPU renderer and the GLES renderer both draw a scene, so they agree on
//! what is on screen and in what order. The order is the shell's lower
//! layers, the windows bottom to top with their popups, the shell's upper
//! layers, and the popups of layers.

use fulcrum_wire::protocol::wlr_layer_shell_unstable_v1::zwlr_layer_shell_v1;

use crate::{
    State,
    geometry::Rect,
    image::Image,
    surface::{MAX_SUBSURFACE_DEPTH, SurfaceId},
    windows::BACKGROUND,
};

/// One surface, as an output shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Element {
    pub surface: SurfaceId,
    /// Where it is drawn, and at what size, in the output's pixels.
    pub rect: Rect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scene {
    pub size: (i32, i32),
    pub background: u32,
    pub elements: Vec<Element>,
}

impl State {
    /// What an output shows now.
    pub(crate) fn scene(&self, number: u64) -> Option<Scene> {
        let output = self.outputs.iter().find(|output| output.number == number)?;
        let origin = output.position;
        let to_output = |(x, y): (i32, i32)| (x - origin.0, y - origin.1);
        let mut elements = Vec::new();
        let layers = |kinds: [u32; 2], elements: &mut Vec<Element>| {
            for kind in kinds {
                for layer in self
                    .layers
                    .iter()
                    .filter(|layer| layer.output == number && layer.mapped && layer.layer() == kind)
                {
                    self.push_tree(layer.surface, to_output((layer.rect.x, layer.rect.y)), elements, 0);
                }
            }
        };
        layers(
            [
                zwlr_layer_shell_v1::layer::BACKGROUND,
                zwlr_layer_shell_v1::layer::BOTTOM,
            ],
            &mut elements,
        );
        for window in self.windows.iter().filter(|window| window.workspace == self.workspace) {
            self.push_tree(window.surface, to_output(self.window_origin(window)), &mut elements, 0);
            for popup in &self.popups {
                if self.toplevel_of(*popup) != Some(window.surface) || !self.xdg[popup].mapped {
                    continue;
                }
                if let Some(corner) = self.xdg_location(*popup, 0) {
                    let geometry = self.window_geometry(*popup);
                    self.push_tree(
                        *popup,
                        to_output((corner.0 - geometry.x, corner.1 - geometry.y)),
                        &mut elements,
                        0,
                    );
                }
            }
        }
        layers(
            [zwlr_layer_shell_v1::layer::TOP, zwlr_layer_shell_v1::layer::OVERLAY],
            &mut elements,
        );
        for popup in self.layer_popups(number) {
            if let Some(corner) = self.xdg_location(popup, 0) {
                let geometry = self.window_geometry(popup);
                self.push_tree(
                    popup,
                    to_output((corner.0 - geometry.x, corner.1 - geometry.y)),
                    &mut elements,
                    0,
                );
            }
        }
        Some(Scene {
            size: output.size,
            background: BACKGROUND,
            elements,
        })
    }

    /// A surface and its subsurfaces, in their stacking order, with the
    /// surface's corner at `at`. A surface that shows nothing hides its
    /// subsurfaces too.
    fn push_tree(&self, id: SurfaceId, at: (i32, i32), elements: &mut Vec<Element>, depth: usize) {
        let Some(surface) = self.surfaces.get(&id) else {
            return;
        };
        if surface.current.content.is_none() {
            return;
        }
        for member in &surface.stack {
            if *member == id {
                let (width, height) = surface.current.size();
                elements.push(Element {
                    surface: id,
                    rect: Rect::new(at.0, at.1, width, height),
                });
            } else if depth < MAX_SUBSURFACE_DEPTH
                && let Some(child) = self.surfaces.get(member)
            {
                self.push_tree(
                    *member,
                    (at.0 + child.position.0, at.1 + child.position.1),
                    elements,
                    depth + 1,
                );
            }
        }
    }

    /// Draws a scene with the CPU renderer.
    pub(crate) fn draw_scene(&self, scene: &Scene) -> Image {
        let mut image = Image::new(scene.size.0, scene.size.1);
        image.fill(Rect::new(0, 0, scene.size.0, scene.size.1), scene.background);
        for element in &scene.elements {
            if let Some(shown) = self
                .surfaces
                .get(&element.surface)
                .and_then(|surface| surface.current.shown())
            {
                image.draw(&shown, (element.rect.x, element.rect.y));
            }
        }
        image
    }
}
