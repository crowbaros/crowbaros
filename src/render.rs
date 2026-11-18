//! The GLES renderer's compositor half: a texture for each surface, kept
//! until its content changes, and a scene drawn into a target. A dma-buf the
//! GPU imported is its own texture, drawn where it is.
//!
//! Each draw waits until the GPU is done, so no buffer a client gets back
//! is still being read.
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
    surface::{Content, SurfaceId},
};

pub struct Renderer {
    gles: Gles,
    /// Each surface's texture of copied pixels, with the content serial it
    /// was uploaded at.
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
        self.textures.retain(|surface, _| {
            state
                .surfaces
                .get(surface)
                .is_some_and(|surface| matches!(surface.current.content, Some(Content::Copied(_))))
        });
        for element in &scene.elements {
            let Element::Surface { surface: id, .. } = element else {
                continue;
            };
            let Some(surface) = state.surfaces.get(id) else {
                continue;
            };
            let Some(Content::Copied(image)) = &surface.current.content else {
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
                    let Some(surface) = state.surfaces.get(id) else {
                        continue;
                    };
                    let texture = match &surface.current.content {
                        Some(Content::Gpu(gpu)) => &*gpu.texture,
                        Some(Content::Copied(_)) => match self.textures.get(id) {
                            Some((_, texture)) => texture,
                            None => continue,
                        },
                        None => continue,
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

impl Renderer {
    /// Draws a scene into a target of its own and reads it back, for a
    /// display the GPU cannot draw into.
    pub(crate) fn draw_to_image(&mut self, state: &State, scene: &Scene) -> io::Result<Image> {
        let target = self.gles.offscreen(scene.size.0, scene.size.1)?;
        self.draw(state, scene, &target, None);
        let frame = self.gles.frame(&target);
        let pixels = frame.read((0, 0, scene.size.0, scene.size.1));
        frame.finish();
        Ok(Image {
            width: scene.size.0,
            height: scene.size.1,
            pixels,
        })
    }
}

/// Draws an output with the GLES renderer into a target of its own and
/// reads it back. For the tests, which hold the CPU renderer's frame beside
/// it.
#[doc(hidden)]
pub fn render_output_offscreen(state: &State, output: u64, renderer: &mut Renderer) -> io::Result<Image> {
    let scene = state.scene(output).ok_or_else(|| io::Error::other("no such output"))?;
    renderer.draw_to_image(state, &scene)
}
