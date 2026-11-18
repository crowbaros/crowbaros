//! `wp_presentation`: a client learns when its content was shown, on which
//! output, and when the next frame will be, so it can pace itself.
//!
//! Feedback travels with a commit, as a frame callback does. When an output
//! draws a frame that shows the surface, the feedback waits on that output,
//! and when the frame is shown it is `presented`. A page flip's time comes
//! from the kernel, on the monotonic clock, with the display's vertical
//! blank count. Where no display says when a frame was shown, the time is
//! the draw's. Feedback for content that is replaced before it was drawn, or
//! whose surface or output goes, is `discarded`.

use std::borrow::Cow;

use fulcrum_wire::protocol::presentation_time::{wp_presentation, wp_presentation_feedback};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    surface::SurfaceId,
};

/// The `wp_presentation` version Fulcrum offers.
pub const PRESENTATION_VERSION: u32 = 2;
/// `CLOCK_MONOTONIC`, which the kernel stamps page flips with.
const CLOCK_MONOTONIC: u32 = 1;

/// When a frame was shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    pub seconds: u64,
    pub nanoseconds: u32,
    /// The display's vertical blank count, or 0 where none is known.
    pub sequence: u64,
    /// `wp_presentation_feedback.kind`.
    pub flags: u32,
}

impl Shown {
    /// Now, on the monotonic clock, as for a frame no display reports.
    pub fn now() -> Shown {
        let time = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        Shown {
            seconds: time.tv_sec as u64,
            nanoseconds: time.tv_nsec as u32,
            sequence: 0,
            flags: 0,
        }
    }

    /// A page flip, as the kernel reported it.
    pub fn flip(seconds: u32, microseconds: u32, sequence: u32) -> Shown {
        use wp_presentation_feedback::kind;
        Shown {
            seconds: u64::from(seconds),
            nanoseconds: microseconds.saturating_mul(1000),
            sequence: u64::from(sequence),
            flags: kind::VSYNC | kind::HW_CLOCK | kind::HW_COMPLETION,
        }
    }
}

impl State {
    /// A client bound `wp_presentation`: it learns the clock.
    pub(crate) fn presentation_bound(&mut self, client_id: ClientId, object: u32) {
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.send(
                object,
                wp_presentation::Event::ClockId {
                    clk_id: CLOCK_MONOTONIC,
                },
            );
        }
    }

    pub(crate) fn presentation_request(
        &mut self,
        client_id: ClientId,
        request: wp_presentation::Request,
    ) -> Result<(), ProtocolError> {
        let wp_presentation::Request::Feedback { surface, callback } = request else {
            return Ok(());
        };
        let surface_id = self
            .surface_of(client_id, surface)
            .expect("the dispatch checked the argument's interface");
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        client.create(
            callback,
            Cow::Borrowed(wp_presentation_feedback::NAME),
            Some(&wp_presentation_feedback::INFO),
            1,
            ObjectKind::PresentationFeedback,
        )?;
        if let Some(surface) = self.surfaces.get_mut(&surface_id) {
            surface.pending.presentation.push(callback.0);
        }
        Ok(())
    }

    /// Tells a client its feedback is discarded, and lets go of it.
    pub(crate) fn discard_feedback(&mut self, client_id: ClientId, feedbacks: Vec<u32>) {
        if let Some(client) = self.clients.get_mut(&client_id) {
            for feedback in feedbacks {
                client.send(feedback, wp_presentation_feedback::Event::Discarded);
                client.destroy(feedback);
            }
        }
    }

    /// An output drew a frame: the feedback of what it shows waits for the
    /// frame to be shown.
    pub(crate) fn wait_for_presentation(&mut self, output: u64, surfaces: &[SurfaceId]) {
        let mut waiting = Vec::new();
        for surface in surfaces {
            if let Some(state) = self.surfaces.get_mut(surface) {
                for feedback in std::mem::take(&mut state.current.presentation) {
                    waiting.push((state.client, feedback));
                }
            }
        }
        if let Some(output) = self.outputs.iter_mut().find(|candidate| candidate.number == output) {
            output.presenting.extend(waiting);
        }
    }

    /// An output's frame was shown: its waiting feedback is presented.
    pub(crate) fn present(&mut self, number: u64, shown: Shown) {
        let Some(output) = self.outputs.iter_mut().find(|output| output.number == number) else {
            return;
        };
        let presenting = std::mem::take(&mut output.presenting);
        // The time between frames, in nanoseconds.
        let refresh = u32::try_from(1_000_000_000_000u64 / u64::from(output.refresh.max(1) as u32)).unwrap_or(0);
        for (client_id, feedback) in presenting {
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            for bound in client.outputs_bound(number) {
                client.send(
                    feedback,
                    wp_presentation_feedback::Event::SyncOutput {
                        output: fulcrum_wire::ObjectId(bound),
                    },
                );
            }
            client.send(
                feedback,
                wp_presentation_feedback::Event::Presented {
                    tv_sec_hi: (shown.seconds >> 32) as u32,
                    tv_sec_lo: shown.seconds as u32,
                    tv_nsec: shown.nanoseconds,
                    refresh,
                    seq_hi: (shown.sequence >> 32) as u32,
                    seq_lo: shown.sequence as u32,
                    flags: shown.flags,
                },
            );
            client.destroy(feedback);
        }
    }
}
