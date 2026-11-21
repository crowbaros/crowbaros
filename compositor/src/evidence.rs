//! What the compositor shows it did, for the checks that run it: how many
//! commits each window and layer has made, and how many frames it drew.
//!
//! The line is written after a commit or a frame, and at most every
//! [`EVERY`], so a still desktop, which commits nothing and draws nothing,
//! writes nothing, and no timer wakes the compositor for it. CrowbarOS's
//! desktop check reads it, as it read the spike's:
//!
//! ```text
//! commits so far: layer crowbaros-bar: 3, window 0x7 foot: 12 frames=41
//! ```
//!
//! After the workspace showing changes, or a window moves to another, each
//! layer's first commit that changes what it shows is logged with the time
//! since, as the bar's showing it (#273):
//!
//! ```text
//! layer crowbaros-bar drew 9.6 ms after the workspaces changed
//! ```

use std::time::{Duration, Instant};

use crate::{State, control::window_id, log, surface::SurfaceId};

/// The least time between two reports.
pub const EVERY: Duration = Duration::from_secs(5);

/// The last change to the workspaces, and the layers that drew since.
pub(crate) struct WorkspacesChanged {
    at: Instant,
    drawn: Vec<(String, Duration)>,
}

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
        if label.starts_with("layer ")
            && let Some(changed) = &mut self.workspaces_changed
            && !changed.drawn.iter().any(|(drawn, _)| *drawn == label)
            && self
                .surfaces
                .get(&root)
                .is_some_and(|surface| surface.damaged_by_last_commit())
        {
            let after = changed.at.elapsed();
            log::info!(
                "{label} drew {:.1} ms after the workspaces changed",
                after.as_secs_f64() * 1000.0
            );
            changed.drawn.push((label.clone(), after));
        }
        *self.commit_counts.entry(label).or_insert(0) += 1;
        self.report();
    }

    /// The workspace showing changed, or a window moved to another.
    pub(crate) fn note_workspaces_changed(&mut self) {
        self.workspaces_changed = Some(WorkspacesChanged {
            at: Instant::now(),
            drawn: Vec::new(),
        });
    }

    /// The layers that drew since the workspaces last changed, and how long
    /// after the change each did.
    #[doc(hidden)]
    pub fn drawn_since_workspaces_changed(&self) -> Vec<(String, Duration)> {
        self.workspaces_changed
            .as_ref()
            .map_or_else(Vec::new, |changed| changed.drawn.clone())
    }

    /// After a commit or a frame: reports, if the last report is old enough.
    pub(crate) fn report(&mut self) {
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
