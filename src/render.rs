//! The GLES renderer's compositor half: a texture for each surface, kept
//! until its content changes, and a scene drawn into a target. A dma-buf the
//! GPU imported is its own texture, drawn where it is.
//!
//! Each draw waits until the GPU is done, so no buffer a client gets back
//! is still being read. A draw touches only the damage it is given (see
//! `damage.rs`): the rest of the target keeps the frame it held.
//!
//! What to draw, and in what order, is the scene's (see `scene.rs`), the
//! same scene the CPU renderer draws. The GPU calls are `fulcrum-sys`'s.

use std::{collections::HashMap, io, rc::Rc};

use fulcrum_sys::gles::{Gles, Target, Texture};

use crate::{
    State,
    damage::DamageTracker,
    geometry::Rect,
    image::Image,
    scene::{Element, Scene},
    surface::{Content, SurfaceId},
    xcursor::CursorImage,
};

pub struct Renderer {
    gles: Gles,
    /// Each surface's texture of copied pixels, with the content serial it
    /// was uploaded at.
    textures: HashMap<SurfaceId, (u64, Texture)>,
    /// Each cursor shape's texture, with the image it was uploaded from.
    cursors: HashMap<u32, (Rc<CursorImage>, Texture)>,
}

impl Renderer {
    pub fn new(gles: Gles) -> Renderer {
        Renderer {
            gles,
            textures: HashMap::new(),
            cursors: HashMap::new(),
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
            let Element::Cursor { shape, .. } = element else {
                continue;
            };
            let Some(image) = state.cursor_theme.image(*shape) else {
                continue;
            };
            if self
                .cursors
                .get(shape)
                .is_some_and(|(uploaded, _)| Rc::ptr_eq(uploaded, &image))
            {
                continue;
            }
            match self
                .gles
                .upload(image.image.width, image.image.height, &image.image.pixels)
            {
                Ok(texture) => {
                    self.cursors.insert(*shape, (image, texture));
                }
                Err(error) => crate::log::warning!("could not upload a cursor: {error}"),
            }
        }
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

    /// Draws a scene into a target, only inside `damage`, then waits until
    /// the GPU is done. Outside it, the target keeps what it held.
    pub(crate) fn draw(&mut self, state: &State, scene: &Scene, target: &Target, damage: &[Rect]) {
        self.prepare(state, scene);
        let mut frame = self.gles.frame(target);
        for clip in damage {
            let clip_box = (clip.x, clip.y, clip.width, clip.height);
            frame.clear(clip_box, scene.background);
            for element in &scene.elements {
                match element {
                    Element::Surface { surface: id, rect } => {
                        if !rect.overlaps(clip) {
                            continue;
                        }
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
                        frame.draw(
                            texture,
                            surface.current.source_in_buffer(),
                            (rect.x, rect.y, rect.width, rect.height),
                            clip_box,
                            false,
                            1.0,
                        );
                    }
                    Element::Solid { rect, colour } => {
                        let inside = rect.intersection(clip);
                        frame.clear((inside.x, inside.y, inside.width, inside.height), *colour);
                    }
                    Element::Cursor { shape, rect } => {
                        let Some((_, texture)) = self.cursors.get(shape).filter(|_| rect.overlaps(clip)) else {
                            continue;
                        };
                        frame.draw(
                            texture,
                            (0.0, 0.0, texture.width as f32, texture.height as f32),
                            (rect.x, rect.y, rect.width, rect.height),
                            clip_box,
                            false,
                            1.0,
                        );
                    }
                }
            }
        }
        frame.finish();
    }

    /// Draws a scene whole into a target of its own and reads it back, for
    /// a display the GPU cannot draw into.
    pub(crate) fn draw_to_image(&mut self, state: &State, scene: &Scene) -> io::Result<Image> {
        let target = self.gles.offscreen(scene.size.0, scene.size.1)?;
        self.draw(state, scene, &target, &[Rect::new(0, 0, scene.size.0, scene.size.1)]);
        Ok(self.read(&target))
    }

    fn read(&self, target: &Target) -> Image {
        let frame = self.gles.frame(target);
        let pixels = frame.read((0, 0, target.width, target.height));
        frame.finish();
        Image {
            width: target.width,
            height: target.height,
            pixels,
        }
    }
}

/// An output the GLES renderer draws into one target of its own, frame
/// after frame, only where it changed. For the tests, which hold each frame
/// beside one drawn whole.
#[doc(hidden)]
#[derive(Default)]
pub struct OffscreenOutput {
    target: Option<Target>,
    tracker: DamageTracker,
}

impl OffscreenOutput {
    /// Draws the output's next frame where it changed, and reads it back
    /// with the damage that was drawn.
    pub fn draw(&mut self, state: &State, output: u64, renderer: &mut Renderer) -> io::Result<(Image, Vec<Rect>)> {
        let scene = state.scene(output).ok_or_else(|| io::Error::other("no such output"))?;
        let fits = self
            .target
            .as_ref()
            .is_some_and(|target| (target.width, target.height) == scene.size);
        if !fits {
            self.target = Some(renderer.gles.offscreen(scene.size.0, scene.size.1)?);
        }
        let target = self.target.as_ref().expect("made above");
        let damage = self.tracker.damage(state, &scene, if fits { 1 } else { 0 });
        renderer.draw(state, &scene, target, &damage);
        Ok((renderer.read(target), damage))
    }
}

/// Draws an output with the pointer, as a backend that draws the pointer
/// does, with the GLES renderer into a target of its own, and reads it back.
/// For the tests.
#[doc(hidden)]
pub fn render_output_with_cursor_offscreen(state: &State, output: u64, renderer: &mut Renderer) -> io::Result<Image> {
    let scene = state
        .scene_with_cursor(output)
        .ok_or_else(|| io::Error::other("no such output"))?;
    renderer.draw_to_image(state, &scene)
}

/// Draws an output with the GLES renderer into a target of its own and
/// reads it back. For the tests, which hold the CPU renderer's frame beside
/// it.
#[doc(hidden)]
pub fn render_output_offscreen(state: &State, output: u64, renderer: &mut Renderer) -> io::Result<Image> {
    let scene = state.scene(output).ok_or_else(|| io::Error::other("no such output"))?;
    renderer.draw_to_image(state, &scene)
}
