//! The GLES renderer's compositor half: a texture for each surface, kept
//! until its content changes, and a scene drawn into a target.
//!
//! What to draw, and in what order, is the scene's (see `scene.rs`), the
//! same scene the CPU renderer draws. The GPU calls are `fulcrum-sys`'s.

use std::{collections::HashMap, io};

use fulcrum_sys::gles::{Gles, Target, Texture};

use crate::{
    State,
    geometry::Rect,
    image::Image,
    scene::{Element, Scene},
    surface::SurfaceId,
};

pub struct Renderer {
    gles: Gles,
    /// Each surface's texture, with the content serial it was uploaded at.
    textures: HashMap<SurfaceId, (u64, Texture)>,
}

impl Renderer {
    pub fn new(gles: Gles) -> Renderer {
        Renderer {
            gles,
            textures: HashMap::new(),
        }
    }

    pub fn gles(&self) -> &Gles {
        &self.gles
    }

    /// Uploads what each element of the scene shows, where it changed.
    fn prepare(&mut self, state: &State, scene: &Scene) {
        self.textures.retain(|surface, _| state.surfaces.contains_key(surface));
        for element in &scene.elements {
            let Element::Surface { surface: id, .. } = element else {
                continue;
            };
            let Some(surface) = state.surfaces.get(id) else {
                continue;
            };
            let Some(image) = &surface.current.content else {
                continue;
            };
            let serial = surface.current.content_serial;
            if self.textures.get(id).is_some_and(|(uploaded, _)| *uploaded == serial) {
                continue;
            }
            match self.gles.upload(image.width, image.height, &image.pixels) {
                Ok(texture) => {
                    self.textures.insert(*id, (serial, texture));
                }
                Err(error) => crate::log::warning!("could not upload a surface: {error}"),
            }
        }
    }

    /// Draws a scene into a target, and the pointer as a white square where
    /// `pointer` says, then waits until the GPU is done.
    pub(crate) fn draw(&mut self, state: &State, scene: &Scene, target: &Target, pointer: Option<Rect>) {
        self.prepare(state, scene);
        let full = (0, 0, target.width, target.height);
        let mut frame = self.gles.frame(target);
        frame.clear(full, scene.background);
        for element in &scene.elements {
            match element {
                Element::Surface { surface: id, rect } => {
                    let (Some((_, texture)), Some(surface)) = (self.textures.get(id), state.surfaces.get(id)) else {
                        continue;
                    };
                    let destination = (rect.x, rect.y, rect.width, rect.height);
                    frame.draw(
                        texture,
                        surface.current.source_in_buffer(),
                        destination,
                        destination,
                        false,
                        1.0,
                    );
                }
                Element::Solid { rect, colour } => {
                    frame.clear((rect.x, rect.y, rect.width, rect.height), *colour);
                }
            }
        }
        if let Some(pointer) = pointer {
            frame.clear(
                (pointer.x, pointer.y, pointer.width, pointer.height),
                crate::backend::POINTER_COLOUR,
            );
        }
        frame.finish();
    }
}

/// Draws an output with the GLES renderer into a target of its own and
/// reads it back. For the tests, which hold the CPU renderer's frame beside
/// it.
#[doc(hidden)]
pub fn render_output_offscreen(state: &State, output: u64, renderer: &mut Renderer) -> io::Result<Image> {
    let scene = state.scene(output).ok_or_else(|| io::Error::other("no such output"))?;
    let target = renderer.gles.offscreen(scene.size.0, scene.size.1)?;
    renderer.draw(state, &scene, &target, None);
    let frame = renderer.gles.frame(&target);
    let pixels = frame.read((0, 0, scene.size.0, scene.size.1));
    frame.finish();
    Ok(Image {
        width: scene.size.0,
        height: scene.size.1,
        pixels,
    })
}
