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
//! until the next change. This is the baseline's design, kept as it was.

use std::time::{Duration, Instant};

use fulcrum_wire::protocol::wayland::wl_callback;

use crate::{State, backend::Drawn, event_loop::TimerKey, surface::SurfaceId};

/// Where an output is in drawing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Redraw {
    /// Nothing changed since the last frame. Nothing is scheduled.
    #[default]
    Idle,
    /// Something changed, and a draw runs at the end of this turn of the
    /// loop.
    Queued,
    /// A draw ran less than one frame time ago. `again` says something has
    /// changed since.
    Waiting { again: bool },
}

impl State {
    /// Something that this output shows has changed.
    pub fn queue_redraw(&mut self, number: u64) {
        let Some(output) = self.outputs.iter_mut().find(|output| output.number == number) else {
            return;
        };
        match output.redraw {
            Redraw::Idle => {
                output.redraw = Redraw::Queued;
                self.draws_due.push(number);
            }
            Redraw::Queued => {}
            Redraw::Waiting { .. } => output.redraw = Redraw::Waiting { again: true },
        }
    }

    pub fn queue_redraw_all(&mut self) {
        let numbers: Vec<u64> = self.outputs.iter().map(|output| output.number).collect();
        for number in numbers {
            self.queue_redraw(number);
        }
    }

    /// A surface changed: queues the outputs that showed its window or layer
    /// before, and the ones that show it now.
    pub(crate) fn queue_redraw_for(&mut self, surface: SurfaceId) {
        let root = self.root_of(surface);
        let before = self
            .surfaces
            .get(&root)
            .map(|surface| surface.entered.clone())
            .unwrap_or_default();
        if self.xdg.get(&root).is_some_and(|xdg| xdg.mapped) || self.layer_rect(root).is_some() {
            self.update_outputs_of(root);
        }
        let now = self
            .surfaces
            .get(&root)
            .map(|surface| surface.entered.clone())
            .unwrap_or_default();
        for output in before.into_iter().chain(now) {
            self.queue_redraw(output);
        }
    }

    /// Runs the draws queued in this turn. The event loop calls it once a
    /// turn, after every event was handled.
    pub(crate) fn run_due_draws(&mut self) {
        for number in std::mem::take(&mut self.draws_due) {
            self.draw(number);
        }
    }

    fn draw(&mut self, number: u64) {
        if self
            .outputs
            .iter()
            .find(|output| output.number == number)
            .map(|output| output.redraw)
            != Some(Redraw::Queued)
        {
            return;
        }
        let drawn = match self.backend.take() {
            Some(mut backend) => {
                let drawn = backend.draw(self, number);
                self.backend = Some(backend);
                drawn
            }
            None => Drawn::Nothing,
        };
        self.draw_passes += 1;
        if drawn != Drawn::Nothing {
            self.frames_drawn += 1;
            crate::log::debug!("frame drawn on output {number}, {} in all", self.frames_drawn);
        }
        let Some(output) = self.outputs.iter_mut().find(|output| output.number == number) else {
            return;
        };
        output.redraw = Redraw::Waiting { again: false };
        output.draws += 1;
        let draw = output.draws;
        let frame = Duration::from_secs_f64(1_000.0 / f64::from(output.refresh.max(1_000)));
        if drawn != Drawn::Flip {
            // No display will say when this wait is over.
            self.event_loop
                .set_timer(Instant::now() + frame, TimerKey::FrameWait(number, draw));
        }
        self.send_frames(number);
    }

    /// A backend says the display showed the frame it was sent, or that it
    /// never will, as after the session was paused.
    pub fn frame_shown(&mut self, number: u64) {
        self.wait_over(number, None);
    }

    /// The wait after a draw is over. Draws again if something changed
    /// meanwhile.
    pub(crate) fn wait_over(&mut self, number: u64, timer_of_draw: Option<u64>) {
        let Some(output) = self.outputs.iter_mut().find(|output| output.number == number) else {
            return;
        };
        if timer_of_draw.is_some_and(|draw| draw != output.draws) {
            return;
        }
        match output.redraw {
            Redraw::Waiting { again: true } => {
                output.redraw = Redraw::Queued;
                self.draw(number);
            }
            Redraw::Waiting { again: false } => output.redraw = Redraw::Idle,
            Redraw::Idle | Redraw::Queued => {}
        }
    }

    /// Tells the clients shown on an output that a frame was drawn, so each
    /// may draw its next. A window on several outputs hears from the first.
    fn send_frames(&mut self, number: u64) {
        let time = self.start_time.elapsed().as_millis() as u32;
        let shown: Vec<SurfaceId> = self
            .windows
            .iter()
            .map(|window| window.surface)
            .chain(self.layer_surfaces())
            .filter(|surface| self.surfaces.get(surface).and_then(|s| s.entered.first()) == Some(&number))
            .collect();
        for root in shown {
            for surface in self.surfaces_of_window(root) {
                let Some(state) = self.surfaces.get_mut(&surface) else {
                    continue;
                };
                let callbacks = std::mem::take(&mut state.current.frames);
                let client = state.client;
                if let Some(client) = self.clients.get_mut(&client) {
                    for callback in callbacks {
                        client.send(callback, wl_callback::Event::Done { callback_data: time });
                        client.destroy(callback);
                    }
                }
            }
        }
    }

    /// Frames that reached a display, and times the compositor looked for
    /// something to draw. On a still desktop neither moves.
    pub fn drawing(&self) -> (u64, u64) {
        (self.frames_drawn, self.draw_passes)
    }
}
