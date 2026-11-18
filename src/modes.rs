//! How a window fills its output: floating, maximized, fullscreen, or
//! snapped to a half (ADR 0020).
//!
//! A window leaves floating at the client's request or by one of the
//! compositor's keys, and the compositor tells it the size its mode gives it
//! and moves it there. Leaving a mode puts it back where it floated, at the
//! size it had. A maximized or snapped window fills the room the shell's
//! layers leave, inside its frame; a fullscreen window fills its output,
//! and its frame is drawn inside the output's edge.

use crate::{State, geometry::Rect, scene::FRAME_WIDTH, surface::SurfaceId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Floating,
    Maximized,
    Fullscreen,
    /// The left half of the output's usable area.
    Left,
    /// The right half.
    Right,
}

impl State {
    /// The output a window is on: the one that holds its corner, or the
    /// first.
    pub(crate) fn output_of_window(&self, surface: SurfaceId) -> Option<u64> {
        let window = self.windows.iter().find(|window| window.surface == surface)?;
        self.outputs
            .iter()
            .find(|output| output.rect().contains(window.location.0, window.location.1))
            .or_else(|| self.outputs.first())
            .map(|output| output.number)
    }

    /// Where a mode puts a window's geometry on an output, or `None` for
    /// floating.
    pub(crate) fn mode_rect(&self, output: u64, mode: Mode) -> Option<Rect> {
        let area = self.usable_area(output);
        let inside = |rect: Rect| {
            Rect::new(
                rect.x + FRAME_WIDTH,
                rect.y + FRAME_WIDTH,
                (rect.width - 2 * FRAME_WIDTH).max(1),
                (rect.height - 2 * FRAME_WIDTH).max(1),
            )
        };
        match mode {
            Mode::Floating => None,
            Mode::Maximized => Some(inside(area)),
            Mode::Fullscreen => self.outputs.iter().find(|o| o.number == output).map(|o| o.rect()),
            Mode::Left => Some(inside(Rect::new(area.x, area.y, area.width / 2, area.height))),
            Mode::Right => {
                let half = area.width / 2;
                Some(inside(Rect::new(area.x + half, area.y, area.width - half, area.height)))
            }
        }
    }

    pub(crate) fn mode_of(&self, surface: SurfaceId) -> Mode {
        self.windows
            .iter()
            .find(|window| window.surface == surface)
            .map_or(Mode::Floating, |window| window.mode)
    }

    /// Puts a mapped window into a mode, and tells it.
    pub(crate) fn set_mode(&mut self, surface: SurfaceId, mode: Mode) {
        let Some(output) = self.output_of_window(surface) else {
            return;
        };
        let target = self.mode_rect(output, mode);
        let current = self
            .windows
            .iter()
            .find(|window| window.surface == surface)
            .map(|window| (window.mode, self.window_rect(window)));
        let Some((old, rect)) = current else {
            return;
        };
        if old == mode {
            return;
        }
        let before = self.outputs_showing(surface);
        let window = self
            .windows
            .iter_mut()
            .find(|window| window.surface == surface)
            .expect("found above");
        if old == Mode::Floating {
            window.floating = Some(rect);
        }
        window.mode = mode;
        window.fitted = target;
        match target {
            Some(target) => window.location = (target.x, target.y),
            None => {
                if let Some(floating) = window.floating {
                    window.location = (floating.x, floating.y);
                    window.restore = Some((floating.width, floating.height));
                }
            }
        }
        crate::log::debug!("window mode surface={surface} mode={mode:?}");
        self.configure(surface);
        self.update_outputs_of(surface);
        for output in before {
            self.queue_redraw(output);
        }
        self.queue_redraw_for(surface);
    }

    /// The size a toplevel's configure asks for: its mode's, or the size it
    /// floated at when it leaves a mode, or else none, which leaves it to
    /// the client.
    pub(crate) fn configure_size(&mut self, surface: SurfaceId) -> (i32, i32) {
        let window = self.windows.iter().find(|window| window.surface == surface);
        let mode = match window {
            Some(window) => window.mode,
            // Not placed yet: what the client asked for before its first
            // commit decides.
            None => match self.xdg.get(&surface).map(|xdg| &xdg.toplevel) {
                Some(requests) if requests.fullscreen => Mode::Fullscreen,
                Some(requests) if requests.maximized => Mode::Maximized,
                _ => Mode::Floating,
            },
        };
        let output = match window {
            Some(_) => self.output_of_window(surface),
            None => self.output_for_new_window(),
        };
        if let Some(rect) = output.and_then(|output| self.mode_rect(output, mode)) {
            return (rect.width, rect.height);
        }
        self.windows
            .iter_mut()
            .find(|window| window.surface == surface)
            .and_then(|window| window.requested.or_else(|| window.restore.take()))
            .unwrap_or((0, 0))
    }

    /// After an output, or the room its layers leave, changed: each window in
    /// a mode on it is moved and told its size again.
    pub(crate) fn refit_windows(&mut self, output: u64) {
        let fitted: Vec<(SurfaceId, Mode)> = self
            .windows
            .iter()
            .filter(|window| window.mode != Mode::Floating)
            .map(|window| (window.surface, window.mode))
            .collect();
        for (surface, mode) in fitted {
            if self.output_of_window(surface) != Some(output) {
                continue;
            }
            let Some(rect) = self.mode_rect(output, mode) else {
                continue;
            };
            let Some(window) = self.windows.iter_mut().find(|window| window.surface == surface) else {
                continue;
            };
            if window.fitted == Some(rect) {
                continue;
            }
            window.fitted = Some(rect);
            window.location = (rect.x, rect.y);
            self.configure(surface);
            self.queue_redraw_for(surface);
        }
    }

    /// One of the compositor's keys asks the window with the keyboard to
    /// take a mode, or to leave it if it is in it already.
    pub(crate) fn toggle_mode(&mut self, mode: Mode) {
        let Some(focus) = self.seat.keyboard_focus else {
            return;
        };
        let Some(window) = self
            .toplevel_of(self.root_of(focus))
            .filter(|toplevel| self.windows.iter().any(|window| window.surface == *toplevel))
        else {
            return;
        };
        let next = if self.mode_of(window) == mode {
            Mode::Floating
        } else {
            mode
        };
        self.set_mode(window, next);
    }
}
