//! The pointer's image: a client's cursor surface, or the compositor's own.
//!
//! A client sets the cursor with `wl_pointer.set_cursor`, and only while the
//! pointer is on one of its surfaces. When the pointer moves to another
//! client's surface, or to none, the compositor's own image comes back until
//! that client sets one. A null surface hides the pointer.
//!
//! The image is drawn above everything, at the pointer less its hotspot. It
//! is a client's pixels, so the compositor clips it so that it covers no
//! frame (ADR 0020): a frame under it is drawn again above it.

use crate::{
    State,
    backend::{POINTER_COLOUR, POINTER_SIZE},
    geometry::Rect,
    scene::{Element, Scene},
    surface::SurfaceId,
    windows::intersects,
};

/// What the pointer looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cursor {
    /// A client hid it.
    Hidden,
    /// A client's surface, with the hotspot in its own coordinates.
    Surface { surface: SurfaceId, hotspot: (i32, i32) },
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
            None => {
                return vec![Element::Solid {
                    rect: Rect::new(at.0, at.1, POINTER_SIZE, POINTER_SIZE),
                    colour: POINTER_COLOUR,
                }];
            }
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
                Element::Solid { .. } => None,
            })
            .collect();
        let frames: Vec<Element> = scene
            .elements
            .iter()
            .filter(|element| match element {
                Element::Solid { rect, .. } => covered.iter().any(|cursor| intersects(cursor, rect)),
                Element::Surface { .. } => false,
            })
            .copied()
            .collect();
        elements.extend(frames);
        elements
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
