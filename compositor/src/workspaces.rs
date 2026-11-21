//! Workspaces: nine of them, each spanning every output, one showing at a
//! time (ADR 0020).
//!
//! A window belongs to one workspace. A window on a workspace that is not
//! showing is nowhere: it is not drawn, takes no input, and its client is
//! told it left every output, so it gets no frame callbacks and stops
//! drawing. A window that held the keyboard lets go of it when its
//! workspace is hidden.

use crate::{State, log, surface::SurfaceId};

/// How many workspaces there are.
pub const WORKSPACES: u32 = 9;

impl State {
    /// The workspace showing, from 1 to [`WORKSPACES`].
    pub fn workspace(&self) -> u32 {
        self.workspace
    }

    /// Shows a workspace. Its windows come back where they were.
    pub fn show_workspace(&mut self, workspace: u32) {
        if !(1..=WORKSPACES).contains(&workspace) || workspace == self.workspace {
            return;
        }
        self.workspace = workspace;
        log::debug!("workspace shown workspace={workspace}");
        self.workspace_changed();
    }

    /// Moves a window to another workspace. The workspace showing stays.
    pub(crate) fn move_to_workspace(&mut self, surface: SurfaceId, workspace: u32) {
        if !(1..=WORKSPACES).contains(&workspace) {
            return;
        }
        let Some(window) = self.windows.iter_mut().find(|window| window.surface == surface) else {
            return;
        };
        if window.workspace == workspace {
            return;
        }
        window.workspace = workspace;
        self.workspace_changed();
    }

    /// After what shows changed: the keyboard leaves a hidden window, every
    /// window's client is told its outputs, the pointer looks again at what
    /// is under it, and every output is drawn again.
    fn workspace_changed(&mut self) {
        self.note_workspaces_changed();
        if let Some(focus) = self.seat.desktop_keyboard_focus {
            let root = self.root_of(focus);
            let hidden = self
                .windows
                .iter()
                .any(|window| window.surface == root && window.workspace != self.workspace);
            let popup_of_hidden = self.toplevel_of(root).is_some_and(|toplevel| {
                self.windows
                    .iter()
                    .any(|window| window.surface == toplevel && window.workspace != self.workspace)
            });
            if hidden || popup_of_hidden {
                self.seat.desktop_keyboard_focus = None;
                self.refresh_keyboard_focus();
            }
        }
        let windows: Vec<SurfaceId> = self.windows.iter().map(|window| window.surface).collect();
        for surface in windows {
            self.update_outputs_of(surface);
        }
        self.refresh_pointer_focus();
        self.queue_redraw_all();
    }

    /// The workspace each window is on, as the shell is told it.
    pub(crate) fn workspace_of(&self, surface: SurfaceId) -> Option<u32> {
        self.windows
            .iter()
            .find(|window| window.surface == surface)
            .map(|window| window.workspace)
    }
}
