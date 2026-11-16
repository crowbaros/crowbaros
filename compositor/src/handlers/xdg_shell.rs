use smithay::{
    desktop::{PopupKind, find_popup_root_surface, get_popup_toplevel_coords},
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_seat::WlSeat, wl_surface::WlSurface},
    utils::Serial,
    wayland::shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState},
};

use crate::State;

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.add_window(surface);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.remove_window(&surface);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.keep_popup_on_its_output(&surface);
        if let Err(error) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!(%error, "could not track a popup");
        }
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        self.queue_redraw_all();
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.keep_popup_on_its_output(&surface);
        surface.send_repositioned(token);
    }

    // A popup takes no grab yet. Moving and resizing a window, maximize, and
    // fullscreen are built with issue 78, so those requests change nothing.
    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        answer_unchanged(&surface);
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<WlOutput>) {
        answer_unchanged(&surface);
    }
}

/// Answers a request that changes nothing. The protocol wants an answer.
fn answer_unchanged(surface: &ToplevelSurface) {
    if surface.is_initial_configure_sent() {
        surface.send_configure();
    }
}

impl State {
    /// Sends a window or a popup its first configure, on its first commit.
    pub(crate) fn xdg_commit(&mut self, surface: &WlSurface) {
        if let Some(toplevel) = self.window_for(surface).and_then(|window| window.toplevel().cloned())
            && !toplevel.is_initial_configure_sent()
        {
            toplevel.send_configure();
        }
        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
            && let Err(error) = popup.send_configure()
        {
            tracing::warn!(%error, "could not configure a popup");
        }
    }

    /// Fits a popup inside the output its window is on.
    fn keep_popup_on_its_output(&self, popup: &PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let Some(window) = self.window_for(&root) else {
            return;
        };
        let Some(window_geometry) = self.space.element_geometry(&window) else {
            return;
        };
        let Some(mut target) = self
            .space
            .outputs_for_element(&window)
            .first()
            .or_else(|| self.outputs().next())
            .and_then(|output| self.space.output_geometry(output))
        else {
            return;
        };
        // The positioner works relative to the popup's parent.
        target.loc -= get_popup_toplevel_coords(&kind);
        target.loc -= window_geometry.loc;
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}
