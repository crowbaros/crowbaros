//! Drawing only when something changed.
//!
//! A still desktop draws no frames and wakes on no timer (ADR 0020). Whatever
//! changes what an output shows calls [`State::queue_redraw`]. Nothing else
//! starts a draw.
//!
//! After a draw the output waits for one frame time before it draws again:
//! for the display to show the frame, or, when nothing went to the display,
//! for a timer of the same length. A client that commits every time it is
//! told a frame was drawn is therefore held to the display's rate, whether or
//! not its commits change anything. The timer runs once and is not set again
//! until the next change.

use smithay::{
    desktop::{WindowSurfaceType, find_popup_root_surface, layer_map_for_output},
    output::Output,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::protocol::wl_surface::WlSurface,
    },
};

use crate::{State, backend::Drawn, state::Screen, windows::root_surface};

/// Where an output is in drawing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Redraw {
    /// Nothing changed since the last frame. Nothing is scheduled.
    #[default]
    Idle,
    /// Something changed, and a draw runs on the next turn of the event loop.
    Queued,
    /// A draw ran less than one frame time ago. `again` says something has
    /// changed since.
    Waiting { again: bool },
}

/// What ends the wait after a draw.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitEnd {
    /// The backend says the display showed the frame.
    Display,
    /// The timer set after the draw with this number ran out. A timer whose
    /// draw is no longer the latest ends nothing.
    Timer(u64),
}

impl State {
    fn screen_mut(&mut self, output: &Output) -> Option<&mut Screen> {
        self.screens.iter_mut().find(|screen| &screen.output == output)
    }

    /// Something that this output shows has changed.
    pub fn queue_redraw(&mut self, output: &Output) {
        let Some(screen) = self.screen_mut(output) else {
            return;
        };
        match screen.redraw {
            Redraw::Idle => {
                screen.redraw = Redraw::Queued;
                let output = output.clone();
                self.loop_handle.insert_idle(move |state| state.draw(&output));
            }
            Redraw::Queued => {}
            Redraw::Waiting { .. } => screen.redraw = Redraw::Waiting { again: true },
        }
    }

    pub fn queue_redraw_all(&mut self) {
        let outputs: Vec<Output> = self.outputs().cloned().collect();
        for output in outputs {
            self.queue_redraw(&output);
        }
    }

    /// A surface changed. Queues the outputs that show it, or every output
    /// when the compositor cannot yet say which those are.
    pub(crate) fn queue_redraw_for(&mut self, surface: &WlSurface) {
        let mut root = root_surface(surface);
        if let Some(popup) = self.popups.find_popup(&root)
            && let Ok(parent) = find_popup_root_surface(&popup)
        {
            root = parent;
        }
        let mut outputs = match self.window_for(&root) {
            Some(window) => self.space.outputs_for_element(&window),
            None => self
                .outputs()
                .filter(|output| {
                    layer_map_for_output(output)
                        .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                        .is_some()
                })
                .cloned()
                .collect(),
        };
        if outputs.is_empty() {
            outputs = self.outputs().cloned().collect();
        }
        for output in outputs {
            self.queue_redraw(&output);
        }
    }

    fn draw(&mut self, output: &Output) {
        if self.screen_mut(output).map(|screen| screen.redraw) != Some(Redraw::Queued) {
            return;
        }
        self.space.refresh();
        self.popups.cleanup();
        layer_map_for_output(output).cleanup();

        let drawn = match self.backend.take() {
            Some(mut backend) => {
                let drawn = backend.draw(self, output);
                self.backend = Some(backend);
                drawn
            }
            None => Drawn::Nothing,
        };
        self.draw_passes += 1;
        if drawn != Drawn::Nothing {
            self.frames_drawn += 1;
            tracing::debug!(output = %output.name(), frames = self.frames_drawn, "frame drawn");
        }
        let Some(screen) = self.screen_mut(output) else {
            return;
        };
        screen.redraw = Redraw::Waiting { again: false };
        screen.draws += 1;
        let draw = screen.draws;
        if drawn != Drawn::Flip {
            // No display will say when this wait is over.
            let output = output.clone();
            let timer = Timer::from_duration(Self::frame_time(&output));
            let inserted = self.loop_handle.insert_source(timer, move |_, _, state| {
                state.wait_over(&output, WaitEnd::Timer(draw));
                TimeoutAction::Drop
            });
            if let Err(error) = inserted {
                tracing::warn!(%error, "could not set the timer that ends a frame");
            }
        }
        self.send_frames(output);
        let _ = self.display_handle.flush_clients();
    }

    /// A backend says the display showed the frame it was sent, or that it
    /// never will, as after the session was paused.
    pub fn frame_shown(&mut self, output: &Output) {
        self.wait_over(output, WaitEnd::Display);
    }

    /// The wait after a draw is over. Draws again if something changed
    /// meanwhile.
    fn wait_over(&mut self, output: &Output, end: WaitEnd) {
        let Some(screen) = self.screen_mut(output) else {
            return;
        };
        if matches!(end, WaitEnd::Timer(draw) if draw != screen.draws) {
            return;
        }
        match screen.redraw {
            Redraw::Waiting { again: true } => {
                screen.redraw = Redraw::Queued;
                self.draw(output);
            }
            Redraw::Waiting { again: false } => screen.redraw = Redraw::Idle,
            Redraw::Idle | Redraw::Queued => {}
        }
    }

    /// Tells the clients shown on an output that a frame was drawn, so each
    /// may draw its next. A window on several outputs hears from the first.
    fn send_frames(&self, output: &Output) {
        let time = self.start_time.elapsed();
        for window in self.space.elements() {
            if self.space.outputs_for_element(window).first() == Some(output) {
                window.send_frame(output, time, None, |_, _| Some(output.clone()));
            }
        }
        for layer in layer_map_for_output(output).layers() {
            layer.send_frame(output, time, None, |_, _| Some(output.clone()));
        }
    }
}
