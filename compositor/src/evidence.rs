//! What the compositor shows it did, for the checks that run it: how many
//! commits each window and layer has made, and how many frames it drew.
//!
//! The line is written after a frame is drawn, and at most every
//! [`EVERY`], so a still desktop writes nothing and no timer wakes the
//! compositor for it. CrowbarOS's desktop check reads it, as it read the
//! spike's:
//!
//! ```text
//! commits so far: layer crowbaros-bar: 3, window 0x7 foot: 12 frames=41
//! ```

use std::time::{Duration, Instant};

use crate::{State, control::window_id, log, surface::SurfaceId};

/// The least time between two reports.
pub const EVERY: Duration = Duration::from_secs(5);

impl State {
    /// A window's or a layer surface's root committed.
    pub(crate) fn count_commit(&mut self, surface: SurfaceId) {
        let root = self.root_of(surface);
        let label = if let Some(layer) = self.layer_of(root) {
            format!("layer {}", layer.namespace)
        } else if self.windows.iter().any(|window| window.surface == root) {
            let app_id = self.xdg.get(&root).map_or("", |xdg| xdg.toplevel.app_id.as_str());
            format!("window {} {app_id}", window_id(root))
        } else {
            return;
        };
        *self.commit_counts.entry(label).or_insert(0) += 1;
    }

    /// After a frame was drawn: reports, if the last report is old enough.
    pub(crate) fn report_after_frame(&mut self) {
        let now = Instant::now();
        if self.last_report.is_some_and(|last| now.duration_since(last) < EVERY) {
            return;
        }
        self.last_report = Some(now);
        log::info!("{}", self.commit_report());
    }

    /// The report's line, as it is logged.
    #[doc(hidden)]
    pub fn commit_report(&self) -> String {
        let counts: Vec<String> = self
            .commit_counts
            .iter()
            .map(|(label, count)| format!("{label}: {count}"))
            .collect();
        format!("commits so far: {} frames={}", counts.join(", "), self.frames_drawn)
    }
}
