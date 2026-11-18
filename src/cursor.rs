//! The pointer's image: a client's cursor surface, a shape it named, or the
//! compositor's own.
//!
//! A client sets the cursor with `wl_pointer.set_cursor`, or names a shape
//! with `wp_cursor_shape_device_v1` (see `cursor_shape.rs`), and only while
//! the pointer is on one of its surfaces. When the pointer moves to another
//! client's surface, or to none, the compositor's own image comes back until
//! that client sets one. A null surface hides the pointer.
//!
//! A shape, and the compositor's own pointer, are the cursor theme's images
//! where there is a theme, and a white square where there is none.
//!
//! The image is drawn above everything, at the pointer less its hotspot. A
//! client's surface is a client's pixels, so the compositor clips it so that
//! it covers no frame (ADR 0020): a frame under it is drawn again above it.
//! The theme's images are the compositor's own, as the square is.

use crate::{
    State,
    backend::{POINTER_COLOUR, POINTER_SIZE},
    geometry::Rect,
    scene::{Element, Scene},
    surface::SurfaceId,
    windows::intersects,
};

/// The pointer on one output, for a backend that shows it on a plane of its
/// own.
pub(crate) enum PointerImage {
    /// It is not on the output, or a client hid it.
    None,
    /// The compositor's own image, with its top-left corner on the output.
    Own {
        cursor: std::rc::Rc<crate::xcursor::CursorImage>,
        at: (i32, i32),
    },
    /// A client's surface, which is drawn with the scene so that it covers
    /// no frame.
    Surface,
}

/// What the pointer looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cursor {
    /// A client hid it.
    Hidden,
    /// A client's surface, with the hotspot in its own coordinates.
    Surface { surface: SurfaceId, hotspot: (i32, i32) },
    /// A shape a client named: `wp_cursor_shape_device_v1.shape`.
    Shape(u32),
}

impl State {
    /// The pointer's image on an output, to draw after `scene`.
    pub(crate) fn cursor_elements(&self, output: u64, scene: &Scene) -> Vec<Element> {
        let Some(position) = self.outputs.iter().find(|o| o.number == output).map(|o| o.position) else {
            return Vec::new();
        };
        let (x, y) = self.pointer;
        let at = (x as i32 - position.0, y as i32 - position.1);
        let mut elements = Vec::new();
        match self.seat.cursor {
            None => return self.own_pointer(at, crate::xcursor::DEFAULT_SHAPE),
            Some(Cursor::Shape(shape)) => return self.own_pointer(at, shape),
            Some(Cursor::Hidden) => return elements,
            Some(Cursor::Surface { surface, hotspot }) => {
                self.push_cursor_tree(surface, (at.0 - hotspot.0, at.1 - hotspot.1), &mut elements);
            }
        }
        // No frame is covered by a client's pixels.
        let covered: Vec<Rect> = elements
            .iter()
            .filter_map(|element| match element {
                Element::Surface { rect, .. } => Some(*rect),
                Element::Solid { .. } | Element::Cursor { .. } => None,
            })
            .collect();
        let frames: Vec<Element> = scene
            .elements
            .iter()
            .filter(|element| match element {
                Element::Solid { rect, .. } => covered.iter().any(|cursor| intersects(cursor, rect)),
                Element::Surface { .. } | Element::Cursor { .. } => false,
            })
            .copied()
            .collect();
        elements.extend(frames);
        elements
    }

    /// The pointer on one output, as a cursor plane would show it.
    pub(crate) fn pointer_image(&self, output: u64) -> PointerImage {
        let Some(output) = self.outputs.iter().find(|o| o.number == output) else {
            return PointerImage::None;
        };
        let shape = match self.seat.cursor {
            None => crate::xcursor::DEFAULT_SHAPE,
            Some(Cursor::Shape(shape)) => shape,
            Some(Cursor::Hidden) => return PointerImage::None,
            Some(Cursor::Surface { .. }) => return PointerImage::Surface,
        };
        let cursor = self.cursor_theme.image_or_square(shape);
        let (x, y) = self.pointer;
        let at = (
            x as i32 - output.position.0 - cursor.hotspot.0,
            y as i32 - output.position.1 - cursor.hotspot.1,
        );
        let rect = Rect::new(at.0, at.1, cursor.image.width, cursor.image.height);
        if !intersects(&rect, &Rect::new(0, 0, output.size.0, output.size.1)) {
            return PointerImage::None;
        }
        PointerImage::Own { cursor, at }
    }

    /// The theme's image of a shape at the pointer, or the square where
    /// there is no theme.
    fn own_pointer(&self, at: (i32, i32), shape: u32) -> Vec<Element> {
        match self.cursor_theme.image(shape) {
            Some(cursor) => vec![Element::Cursor {
                shape,
                rect: Rect::new(
                    at.0 - cursor.hotspot.0,
                    at.1 - cursor.hotspot.1,
                    cursor.image.width,
                    cursor.image.height,
                ),
            }],
            None => vec![Element::Solid {
                rect: Rect::new(at.0, at.1, POINTER_SIZE, POINTER_SIZE),
                colour: POINTER_COLOUR,
            }],
        }
    }

    fn push_cursor_tree(&self, surface: SurfaceId, at: (i32, i32), elements: &mut Vec<Element>) {
        let mut scene = Vec::new();
        self.push_tree_public(surface, at, &mut scene);
        elements.extend(scene);
    }

    /// The client's cursor surface lost the pointer, or is gone: the
    /// compositor's own image is shown.
    pub(crate) fn reset_cursor(&mut self) {
        if self.seat.cursor.take().is_some() {
            self.queue_redraw_all();
        }
    }

    /// The cursor surface, for its frame callbacks.
    pub(crate) fn cursor_surface(&self) -> Option<SurfaceId> {
        match self.seat.cursor {
            Some(Cursor::Surface { surface, .. }) => Some(surface),
            _ => None,
        }
    }
}
