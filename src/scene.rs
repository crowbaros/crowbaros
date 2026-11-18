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
    modes::Mode,
    surface::{MAX_SUBSURFACE_DEPTH, SurfaceId},
    windows::{BACKGROUND, Window},
};

/// The width of the frame the compositor draws around every window, in
/// pixels (ADR 0020).
pub const FRAME_WIDTH: i32 = 2;
/// The frame of the window that holds the keyboard. Its look is issue 22's
/// to decide; until then, a plain colour.
pub const FRAME_FOCUSED: u32 = 0xff4d_90fe;
/// The frame of every other window.
pub const FRAME_UNFOCUSED: u32 = 0xff3a_3a3a;

/// One thing an output shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Element {
    /// A surface, drawn into `rect` at the size it is shown, in the output's
    /// pixels.
    Surface { surface: SurfaceId, rect: Rect },
    /// A rectangle of one opaque colour: part of a frame.
    Solid { rect: Rect, colour: u32 },
    /// The cursor theme's image of a shape, drawn into `rect` (see
    /// `xcursor.rs`).
    Cursor { shape: u32, rect: Rect },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scene {
    pub size: (i32, i32),
    pub background: u32,
    pub elements: Vec<Element>,
}

impl State {
    /// What an output shows now.
    ///
    /// From the bottom: the background and bottom layers, the windows with
    /// their frames and popups, the top layers, and the overlay layers with
    /// the layers' popups. A fullscreen window covers the top layers, the
    /// bar and the dock, and nothing else the shell draws: it and every
    /// window above it go between the top layers and the overlay.
    pub(crate) fn scene(&self, number: u64) -> Option<Scene> {
        let scene = self.logical_scene(number)?;
        Some(self.in_pixels(number, scene))
    }

    /// The same, in the output's logical units: what the scene is before it
    /// is scaled to the output's pixels.
    fn logical_scene(&self, number: u64) -> Option<Scene> {
        let output = self.outputs.iter().find(|output| output.number == number)?;
        let origin = output.position;
        let to_output = |(x, y): (i32, i32)| (x - origin.0, y - origin.1);
        let mut elements = Vec::new();
        // Locked, an output shows the lock screen on it and nothing else:
        // blank where there is none (see `lock.rs`).
        if self.locked() {
            for layer in self
                .layers
                .iter()
                .filter(|layer| layer.output == number && layer.mapped && self.is_lock_screen(layer.surface))
            {
                self.push_tree(layer.surface, to_output((layer.rect.x, layer.rect.y)), &mut elements, 0);
            }
            return Some(Scene {
                size: output.logical_size(),
                background: crate::lock::LOCKED_BACKGROUND,
                elements,
            });
        }
        let layers = |kinds: &[u32], elements: &mut Vec<Element>| {
            for kind in kinds {
                for layer in self
                    .layers
                    .iter()
                    .filter(|layer| layer.output == number && layer.mapped && layer.layer() == *kind)
                {
                    self.push_tree(layer.surface, to_output((layer.rect.x, layer.rect.y)), elements, 0);
                }
            }
        };
        let showing: Vec<&Window> = self
            .windows
            .iter()
            .filter(|window| window.workspace == self.workspace)
            .collect();
        let split = showing
            .iter()
            .rposition(|window| window.mode == Mode::Fullscreen)
            .unwrap_or(showing.len());
        let focused = self.seat.keyboard_focus.map(|focus| self.root_of(focus));
        let windows = |windows: &[&Window], elements: &mut Vec<Element>| {
            for window in windows {
                self.push_window(window, focused, origin, elements);
            }
        };
        layers(
            &[
                zwlr_layer_shell_v1::layer::BACKGROUND,
                zwlr_layer_shell_v1::layer::BOTTOM,
            ],
            &mut elements,
        );
        windows(&showing[..split], &mut elements);
        layers(&[zwlr_layer_shell_v1::layer::TOP], &mut elements);
        windows(&showing[split..], &mut elements);
        layers(&[zwlr_layer_shell_v1::layer::OVERLAY], &mut elements);
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
            size: output.logical_size(),
            background: BACKGROUND,
            elements,
        })
    }

    /// A scene in an output's logical units, scaled to its pixels. Each
    /// edge is rounded on its own, so that what meets still meets.
    fn in_pixels(&self, number: u64, mut scene: Scene) -> Scene {
        let Some(output) = self.outputs.iter().find(|output| output.number == number) else {
            return scene;
        };
        scene.size = output.size;
        if output.scale_120 != crate::output::SCALE_ONE {
            for element in &mut scene.elements {
                match element {
                    Element::Surface { rect, .. } | Element::Solid { rect, .. } | Element::Cursor { rect, .. } => {
                        *rect = scale_rect(*rect, output.scale_120);
                    }
                }
            }
        }
        scene
    }

    /// A window, its frame, and its popups.
    fn push_window(
        &self,
        window: &Window,
        focused: Option<SurfaceId>,
        origin: (i32, i32),
        elements: &mut Vec<Element>,
    ) {
        let to_output = |(x, y): (i32, i32)| (x - origin.0, y - origin.1);
        let has_keyboard =
            focused.is_some_and(|focus| focus == window.surface || self.toplevel_of(focus) == Some(window.surface));
        let colour = if has_keyboard { FRAME_FOCUSED } else { FRAME_UNFOCUSED };
        let rect = self.window_rect(window);
        let (x, y) = to_output((rect.x, rect.y));
        let rect = Rect::new(x, y, rect.width, rect.height);
        let fullscreen = window.mode == Mode::Fullscreen;
        if !fullscreen {
            for side in frame_outside(rect) {
                elements.push(Element::Solid { rect: side, colour });
            }
        }
        self.push_tree(window.surface, to_output(self.window_origin(window)), elements, 0);
        for popup in &self.popups {
            if self.toplevel_of(*popup) != Some(window.surface) || !self.xdg[popup].mapped {
                continue;
            }
            if let Some(corner) = self.xdg_location(*popup, 0) {
                let geometry = self.window_geometry(*popup);
                self.push_tree(
                    *popup,
                    to_output((corner.0 - geometry.x, corner.1 - geometry.y)),
                    elements,
                    0,
                );
            }
        }
        // A fullscreen window keeps its frame, inside the output's edge, so
        // a window that draws a picture of the desktop still carries one.
        if fullscreen {
            for side in frame_inside(rect) {
                elements.push(Element::Solid { rect: side, colour });
            }
        }
    }

    /// A surface tree's elements, as for the cursor.
    pub(crate) fn push_tree_public(&self, id: SurfaceId, at: (i32, i32), elements: &mut Vec<Element>) {
        self.push_tree(id, at, elements, 0);
    }

    /// What an output shows, with the pointer's image above it, for a
    /// backend that draws the pointer.
    pub(crate) fn scene_with_cursor(&self, number: u64) -> Option<Scene> {
        let mut scene = self.logical_scene(number)?;
        let cursor = self.cursor_elements(number, &scene);
        scene.elements.extend(cursor);
        Some(self.in_pixels(number, scene))
    }

    /// An output's frame with the pointer, drawn by the CPU renderer.
    pub fn render_output_with_cursor(&self, number: u64) -> Option<Image> {
        let scene = self.scene_with_cursor(number)?;
        Some(self.draw_scene(&scene))
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
                elements.push(Element::Surface {
                    surface: id,
                    rect: Rect::new(at.0, at.1, width, height),
                });
            } else if depth < MAX_SUBSURFACE_DEPTH
                && let Some(child) = self.surfaces.get(member)
            {
                self.push_tree(
                    *member,
                    (
                        at.0.saturating_add(child.position.0),
                        at.1.saturating_add(child.position.1),
                    ),
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
            match element {
                Element::Surface { surface, rect } => {
                    if let Some(shown) = self.surfaces.get(surface).and_then(|surface| surface.current.shown()) {
                        image.draw(&shown.resized(rect.width, rect.height), (rect.x, rect.y));
                    }
                }
                Element::Solid { rect, colour } => image.fill(*rect, *colour),
                Element::Cursor { shape, rect } => {
                    if let Some(cursor) = self.cursor_theme.image(*shape) {
                        image.draw(&cursor.image.resized(rect.width, rect.height), (rect.x, rect.y));
                    }
                }
            }
        }
        image
    }
}

/// A rectangle in logical units, in the pixels of an output at a scale in
/// 120ths: each edge rounded to the nearest pixel.
pub(crate) fn scale_rect(rect: Rect, scale_120: u32) -> Rect {
    let scale = i64::from(scale_120);
    let pixel = |value: i64| (value * scale + 60).div_euclid(120) as i32;
    let (left, top) = (pixel(i64::from(rect.x)), pixel(i64::from(rect.y)));
    let right = pixel(i64::from(rect.x) + i64::from(rect.width));
    let bottom = pixel(i64::from(rect.y) + i64::from(rect.height));
    Rect::new(left, top, right - left, bottom - top)
}

/// A frame's four sides around a rectangle.
fn frame_outside(rect: Rect) -> [Rect; 4] {
    let width = FRAME_WIDTH;
    [
        Rect::new(rect.x - width, rect.y - width, rect.width + 2 * width, width),
        Rect::new(rect.x - width, rect.y + rect.height, rect.width + 2 * width, width),
        Rect::new(rect.x - width, rect.y, width, rect.height),
        Rect::new(rect.x + rect.width, rect.y, width, rect.height),
    ]
}

/// A frame's four sides just inside a rectangle.
fn frame_inside(rect: Rect) -> [Rect; 4] {
    let width = FRAME_WIDTH;
    [
        Rect::new(rect.x, rect.y, rect.width, width),
        Rect::new(rect.x, rect.y + rect.height - width, rect.width, width),
        Rect::new(rect.x, rect.y + width, width, rect.height - 2 * width),
        Rect::new(
            rect.x + rect.width - width,
            rect.y + width,
            width,
            rect.height - 2 * width,
        ),
    ]
}
