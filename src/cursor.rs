//! The pointer's image: a client's cursor surface, a shape it named, or the
//! compositor's own. A drag can also show an icon beside the pointer.
//!
//! A client sets a cursor surface or shape while it has pointer focus or
//! owns the active drag. The compositor's own image comes back when
//! control changes hands. A null cursor surface hides the pointer.
//!
//! A shape, and the compositor's own pointer, are the cursor theme's images
//! where there is a theme, and a white square where there is none.
//!
//! The cursor is drawn at the pointer less its hotspot. The drag icon follows
//! the hotspot plus the icon's committed offset. Frames are drawn above any
//! client pixels that overlap them (ADR 0020).

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
    /// Drawn with the scene: a client's surface, so that it covers no
    /// frame, or any image on an output at another scale than 1, which the
    /// scene scales.
    Composited,
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
        let at = (
            (x as i32).saturating_sub(position.0),
            (y as i32).saturating_sub(position.1),
        );
        let mut elements = Vec::new();
        if let Some(surface) = self.drag_icon()
            && let Some(icon) = self.surfaces.get(&surface)
        {
            let (dx, dy) = icon.current.drag_offset;
            self.push_tree_public(
                surface,
                (at.0.saturating_add(dx), at.1.saturating_add(dy)),
                &mut elements,
            );
        }
        let own_shape = match self.seat.cursor {
            None => Some(crate::xcursor::DEFAULT_SHAPE),
            Some(Cursor::Shape(shape)) => Some(shape),
            Some(Cursor::Hidden) => None,
            Some(Cursor::Surface { surface, hotspot }) => {
                self.push_tree_public(
                    surface,
                    (at.0.saturating_sub(hotspot.0), at.1.saturating_sub(hotspot.1)),
                    &mut elements,
                );
                None
            }
        };
        if !elements.is_empty() {
            let bounds = Rect::new(0, 0, scene.size.0, scene.size.1);
            elements.retain(|element| match element {
                Element::Surface { rect, .. } => intersects(rect, &bounds),
                Element::Solid { .. } | Element::Cursor { .. } => true,
            });
            // Only the compositor's own cursor may cover a frame.
            let covered: Vec<Rect> = elements
                .iter()
                .filter_map(|element| match element {
                    Element::Surface { rect, .. } => Some(*rect),
                    Element::Solid { .. } | Element::Cursor { .. } => None,
                })
                .collect();
            elements.extend(
                scene
                    .elements
                    .iter()
                    .filter(|element| match element {
                        Element::Solid { rect, .. } => covered.iter().any(|cursor| intersects(cursor, rect)),
                        Element::Surface { .. } | Element::Cursor { .. } => false,
                    })
                    .copied(),
            );
        }
        if let Some(shape) = own_shape {
            elements.push(self.own_pointer(at, shape));
        }
        elements
    }

    /// The pointer on one output, as a cursor plane would show it.
    pub(crate) fn pointer_image(&self, output: u64) -> PointerImage {
        let Some(output) = self.outputs.iter().find(|o| o.number == output) else {
            return PointerImage::None;
        };
        if self.drag_icon().is_some() {
            return PointerImage::Composited;
        }
        let shape = match self.seat.cursor {
            None => crate::xcursor::DEFAULT_SHAPE,
            Some(Cursor::Shape(shape)) => shape,
            Some(Cursor::Hidden) => return PointerImage::None,
            Some(Cursor::Surface { .. }) => return PointerImage::Composited,
        };
        if output.scale_120 != crate::output::SCALE_ONE {
            return PointerImage::Composited;
        }
        let cursor = self.cursor_theme.image_or_square(shape);
        let (x, y) = self.pointer;
        let at = (
            x as i32 - output.position.0 - cursor.hotspot.0,
            y as i32 - output.position.1 - cursor.hotspot.1,
        );
        let rect = Rect::new(at.0, at.1, cursor.image.width, cursor.image.height);
        let (width, height) = output.logical_size();
        if !intersects(&rect, &Rect::new(0, 0, width, height)) {
            return PointerImage::None;
        }
        PointerImage::Own { cursor, at }
    }

    /// The theme's image of a shape at the pointer, or the square where
    /// there is no theme.
    fn own_pointer(&self, at: (i32, i32), shape: u32) -> Element {
        match self.cursor_theme.image(shape) {
            Some(cursor) => Element::Cursor {
                shape,
                rect: Rect::new(
                    at.0.saturating_sub(cursor.hotspot.0),
                    at.1.saturating_sub(cursor.hotspot.1),
                    cursor.image.width,
                    cursor.image.height,
                ),
            },
            None => Element::Solid {
                rect: Rect::new(at.0, at.1, POINTER_SIZE, POINTER_SIZE),
                colour: POINTER_COLOUR,
            },
        }
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
