//! The windows the compositor manages: where they go and which has focus.

use smithay::{
    desktop::{Window, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::shell::xdg::ToplevelSurface,
};

use crate::State;

/// A toplevel window and the compositor's own facts about it.
pub struct Managed {
    /// The compositor's name for the window. Clients never choose it.
    pub id: u32,
    pub window: Window,
    pub workspace: u32,
    /// Where the window sits. Kept while its workspace is not shown.
    pub location: Point<i32, Logical>,
    /// Larger means focused more recently.
    pub focus_stamp: u64,
}

/// Workspaces are numbered from 1 to this.
pub const WORKSPACES: u32 = 9;

impl State {
    /// The part of an output that layer surfaces have not reserved, in
    /// global coordinates.
    pub fn usable_area(&self, output: &Output) -> Rectangle<i32, Logical> {
        let mut area = layer_map_for_output(output).non_exclusive_zone();
        if let Some(geometry) = self.space.output_geometry(output) {
            area.loc += geometry.loc;
        }
        area
    }

    pub fn managed(&self, surface: &WlSurface) -> Option<&Managed> {
        self.windows
            .iter()
            .find(|m| m.window.toplevel().is_some_and(|t| t.wl_surface() == surface))
    }

    pub fn add_window(&mut self, toplevel: ToplevelSurface) {
        let window = Window::new_wayland_window(toplevel);
        let Some(output) = self.outputs.first().cloned() else {
            return;
        };
        let area = self.usable_area(&output);
        let step = 32 * (self.windows.len() as i32 % 8);
        let location = Point::from((area.loc.x + 48 + step, area.loc.y + 48 + step));
        self.space.map_element(window.clone(), location, true);
        self.next_window_id += 1;
        self.windows.push(Managed {
            id: self.next_window_id,
            window: window.clone(),
            workspace: self.active_workspace,
            location,
            focus_stamp: 0,
        });
        self.focus_window(Some(&window));
    }

    pub fn remove_window(&mut self, toplevel: &ToplevelSurface) {
        self.windows
            .retain(|m| m.window.toplevel().is_some_and(|t| t != toplevel));
        let next = self
            .windows
            .iter()
            .filter(|m| m.workspace == self.active_workspace)
            .max_by_key(|m| m.focus_stamp)
            .map(|m| m.window.clone());
        self.focus_window(next.as_ref());
    }

    /// Gives the keyboard to a window and raises it, or clears the focus.
    pub fn focus_window(&mut self, window: Option<&Window>) {
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.seat.get_keyboard().unwrap();
        for managed in &self.windows {
            let active = Some(&managed.window) == window;
            managed.window.set_activated(active);
        }
        if let Some(window) = window {
            self.space.raise_element(window, true);
        }
        self.focus_counter += 1;
        let stamp = self.focus_counter;
        self.active_window = self
            .windows
            .iter_mut()
            .find(|m| Some(&m.window) == window)
            .map(|m| {
                m.focus_stamp = stamp;
                m.id
            });
        let surface = window.and_then(|w| w.toplevel()).map(|t| t.wl_surface().clone());
        keyboard.set_focus(self, surface, serial);
        for managed in &self.windows {
            if let Some(toplevel) = managed.window.toplevel() {
                toplevel.send_pending_configure();
            }
        }
    }

    /// Shows one workspace's windows and hides the rest. The keyboard goes
    /// to the window of that workspace that had it last.
    pub fn show_workspace(&mut self, workspace: u32) {
        if workspace == self.active_workspace {
            return;
        }
        self.active_workspace = workspace;
        for managed in &mut self.windows {
            if managed.workspace == workspace {
                self.space.map_element(managed.window.clone(), managed.location, false);
            } else {
                if let Some(location) = self.space.element_location(&managed.window) {
                    managed.location = location;
                }
                self.space.unmap_elem(&managed.window);
            }
        }
        let next = self
            .windows
            .iter()
            .filter(|m| m.workspace == workspace)
            .max_by_key(|m| m.focus_stamp)
            .map(|m| m.window.clone());
        self.focus_window(next.as_ref());
    }

    /// Sizes a window to the usable area of its output.
    pub fn maximize(&mut self, toplevel: &ToplevelSurface) {
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as ToplevelState;
        let Some(window) = self.managed(toplevel.wl_surface()).map(|m| m.window.clone()) else {
            return;
        };
        let Some(output) = self
            .space
            .outputs_for_element(&window)
            .first()
            .cloned()
            .or_else(|| self.outputs.first().cloned())
        else {
            return;
        };
        let area = self.usable_area(&output);
        toplevel.with_pending_state(|state| {
            state.states.set(ToplevelState::Maximized);
            state.size = Some(area.size);
        });
        self.space.map_element(window, area.loc, true);
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
    }
}
