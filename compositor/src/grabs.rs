//! Moving and resizing a window with the pointer (ADR 0020).
//!
//! A grab starts when a client asks, by `xdg_toplevel.move` or `resize`
//! while a button is held on its window, or when the person holds Super and
//! presses the left button (move) or the right button (resize) on any
//! window. While it lasts, the pointer drives the window and no client
//! hears it. Letting go of the button ends it.
//!
//! A resize tells the client each new size with the `resizing` state, within
//! the minimum and maximum it set, and keeps the edge it did not grab where
//! it was as the client commits each size.

use fulcrum_wire::protocol::xdg_shell::xdg_toplevel::resize_edge;

use crate::{State, geometry::Rect, modes::Mode, surface::SurfaceId};

/// What the pointer is doing to a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Grab {
    Move {
        window: SurfaceId,
        button: u32,
        /// Where the pointer and the window were when it started.
        from: (f64, f64),
        location: (i32, i32),
    },
    Resize {
        window: SurfaceId,
        button: u32,
        /// Which edges move: `xdg_toplevel.resize_edge`.
        edges: u32,
        from: (f64, f64),
        /// The window's geometry on the desktop when it started.
        rect: Rect,
    },
}

impl Grab {
    pub(crate) fn window(&self) -> SurfaceId {
        match self {
            Grab::Move { window, .. } | Grab::Resize { window, .. } => *window,
        }
    }

    fn button(&self) -> u32 {
        match self {
            Grab::Move { button, .. } | Grab::Resize { button, .. } => *button,
        }
    }
}

impl State {
    /// Starts moving a window, held by `button`. A window in a mode floats
    /// again, under the pointer.
    pub(crate) fn start_move(&mut self, window: SurfaceId, button: u32) {
        if self.mode_of(window) != Mode::Floating {
            self.set_mode(window, Mode::Floating);
        }
        let Some(location) = self.windows.iter().find(|w| w.surface == window).map(|w| w.location) else {
            return;
        };
        self.end_pointer_focus();
        self.grab = Some(Grab::Move {
            window,
            button,
            from: self.pointer,
            location,
        });
    }

    /// Starts resizing a window from `edges`, held by `button`.
    pub(crate) fn start_resize(&mut self, window: SurfaceId, button: u32, edges: u32) {
        if edges == resize_edge::NONE || self.mode_of(window) == Mode::Fullscreen {
            return;
        }
        if self.mode_of(window) != Mode::Floating {
            self.set_mode(window, Mode::Floating);
        }
        let Some(rect) = self
            .windows
            .iter()
            .find(|w| w.surface == window)
            .map(|w| self.window_rect(w))
        else {
            return;
        };
        self.end_pointer_focus();
        self.grab = Some(Grab::Resize {
            window,
            button,
            edges,
            from: self.pointer,
            rect,
        });
        self.configure(window);
    }

    /// The edges a Super+right drag resizes: those of the window's quarter
    /// the pointer is in.
    pub(crate) fn nearest_edges(&self, window: SurfaceId) -> u32 {
        let Some(rect) = self
            .windows
            .iter()
            .find(|w| w.surface == window)
            .map(|w| self.window_rect(w))
        else {
            return resize_edge::BOTTOM_RIGHT;
        };
        let (x, y) = self.pointer;
        let horizontal = if x < f64::from(rect.x) + f64::from(rect.width) / 2.0 {
            resize_edge::LEFT
        } else {
            resize_edge::RIGHT
        };
        let vertical = if y < f64::from(rect.y) + f64::from(rect.height) / 2.0 {
            resize_edge::TOP
        } else {
            resize_edge::BOTTOM
        };
        horizontal | vertical
    }

    /// The pointer moved during a grab.
    pub(crate) fn grab_motion(&mut self) {
        let Some(grab) = self.grab else {
            return;
        };
        let (x, y) = self.pointer;
        match grab {
            Grab::Move {
                window, from, location, ..
            } => {
                let before = self.outputs_showing(window);
                let to = (
                    location.0 + (x - from.0).round() as i32,
                    location.1 + (y - from.1).round() as i32,
                );
                if let Some(entry) = self.windows.iter_mut().find(|w| w.surface == window) {
                    entry.location = to;
                }
                self.update_outputs_of(window);
                for output in before {
                    self.queue_redraw(output);
                }
                self.queue_redraw_for(window);
            }
            Grab::Resize {
                window,
                edges,
                from,
                rect,
                ..
            } => {
                let (dx, dy) = ((x - from.0).round() as i32, (y - from.1).round() as i32);
                let mut width = rect.width;
                let mut height = rect.height;
                if edges & resize_edge::LEFT != 0 {
                    width -= dx;
                } else if edges & resize_edge::RIGHT != 0 {
                    width += dx;
                }
                if edges & resize_edge::TOP != 0 {
                    height -= dy;
                } else if edges & resize_edge::BOTTOM != 0 {
                    height += dy;
                }
                let size = self.within_limits(window, (width, height));
                let Some(entry) = self.windows.iter_mut().find(|w| w.surface == window) else {
                    return;
                };
                if entry.requested != Some(size) {
                    entry.requested = Some(size);
                    self.configure(window);
                }
            }
        }
    }

    /// A size kept within what the client said it can take, and at least
    /// one pixel.
    fn within_limits(&self, window: SurfaceId, (width, height): (i32, i32)) -> (i32, i32) {
        let requests = self.xdg.get(&window).map(|xdg| xdg.toplevel.clone());
        let (min, max) = requests.map_or(((0, 0), (0, 0)), |requests| (requests.min_size, requests.max_size));
        let clamp = |value: i32, min: i32, max: i32| {
            let value = value.max(min).max(1);
            if max > 0 { value.min(max) } else { value }
        };
        (clamp(width, min.0, max.0), clamp(height, min.1, max.1))
    }

    /// A window under a resize grab committed a size: the edges it did not
    /// grab stay where they were.
    pub(crate) fn anchor_resized(&mut self, surface: SurfaceId) {
        let Some(Grab::Resize {
            window, edges, rect, ..
        }) = self.grab
        else {
            return;
        };
        if window != surface {
            return;
        }
        let geometry = self.window_geometry(surface);
        let x = if edges & resize_edge::LEFT != 0 {
            rect.x + rect.width - geometry.width
        } else {
            rect.x
        };
        let y = if edges & resize_edge::TOP != 0 {
            rect.y + rect.height - geometry.height
        } else {
            rect.y
        };
        if let Some(entry) = self.windows.iter_mut().find(|w| w.surface == surface) {
            entry.location = (x, y);
        }
    }

    /// A button went up. If it held the grab, the grab ends: a resized
    /// window is told its last size without the `resizing` state.
    pub(crate) fn grab_button_released(&mut self, button: u32) -> bool {
        let Some(grab) = self.grab else {
            return false;
        };
        if grab.button() != button {
            return true;
        }
        self.grab = None;
        let window = grab.window();
        if let Grab::Resize { .. } = grab
            && let Some(entry) = self.windows.iter_mut().find(|w| w.surface == window)
        {
            entry.restore = entry.requested.take();
            self.configure(window);
        }
        self.refresh_pointer_focus();
        true
    }

    /// Whether a window is being resized, for its configure's states.
    pub(crate) fn resizing(&self, window: SurfaceId) -> bool {
        matches!(self.grab, Some(Grab::Resize { window: grabbed, .. }) if grabbed == window)
    }
}
