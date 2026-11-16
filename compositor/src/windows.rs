//! The windows the compositor shows: where a new one goes, and which has the
//! keyboard.
//!
//! This is the least that lets a client be seen and typed into. Frames,
//! moving and resizing, workspaces, and the two cases in which a new window
//! takes the keyboard are built with issue 78 (ADR 0020). Until then a new
//! window never takes the keyboard. A click gives it.

use smithay::{
    desktop::{Window, WindowSurfaceType, layer_map_for_output},
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    utils::{Point, SERIAL_COUNTER},
    wayland::{
        compositor::get_parent,
        shell::{wlr_layer::KeyboardInteractivity, xdg::ToplevelSurface},
    },
};

use crate::{State, identity, identity::Identity, state::NEW_WINDOW_OFFSET};

/// The surface at the top of a subsurface tree.
pub(crate) fn root_surface(surface: &WlSurface) -> WlSurface {
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }
    root
}

impl State {
    /// The window whose toplevel surface this is.
    pub(crate) fn window_for(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface)
            })
            .cloned()
    }

    /// Who owns a surface: the identity of the client that made it.
    pub fn owner(&self, surface: &WlSurface) -> Option<Identity> {
        let client = self.display_handle.get_client(surface.id()).ok()?;
        identity::of(&client).cloned()
    }

    /// The owner of each window, in the order the windows are stacked.
    pub fn window_owners(&self) -> Vec<Identity> {
        self.space
            .elements()
            .filter_map(|window| self.owner(window.toplevel()?.wl_surface()))
            .collect()
    }

    /// Shows a new window on the output the pointer is on. It does not take
    /// the keyboard.
    pub(crate) fn add_window(&mut self, toplevel: ToplevelSurface) {
        let pointer = self.pointer().current_location();
        let Some(output) = self
            .space
            .output_under(pointer)
            .next()
            .cloned()
            .or_else(|| self.outputs().next().cloned())
        else {
            return;
        };
        let area = self.usable_area(&output);
        let step = 32 * (self.space.elements().count() as i32 % 8);
        let location = area.loc + NEW_WINDOW_OFFSET + Point::from((step, step));
        self.space
            .map_element(Window::new_wayland_window(toplevel), location, false);
        self.queue_redraw(&output);
    }

    pub(crate) fn remove_window(&mut self, toplevel: &ToplevelSurface) {
        let Some(window) = self.window_for(toplevel.wl_surface()) else {
            return;
        };
        self.space.unmap_elem(&window);
        self.drop_focus_from(toplevel.wl_surface());
        self.queue_redraw_all();
    }

    /// Takes the keyboard from a surface that is going away. Nothing else
    /// takes it: the next click decides.
    pub(crate) fn drop_focus_from(&mut self, surface: &WlSurface) {
        let keyboard = self.keyboard();
        if keyboard.current_focus().as_ref() == Some(surface) {
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        }
    }

    /// A click gives the keyboard to what is under the pointer: a window, or
    /// a layer surface of the shell's that takes keys. A click on anything
    /// else leaves the keyboard where it is.
    pub(crate) fn focus_under_pointer(&mut self) {
        let position = self.pointer().current_location();
        let Some((surface, _)) = self.surface_under(position) else {
            return;
        };
        let root = root_surface(&surface);
        let takes_keys = match self.window_for(&root) {
            Some(window) => {
                self.space.raise_element(&window, true);
                for other in self.space.elements() {
                    other.set_activated(other == &window);
                    if let Some(toplevel) = other.toplevel() {
                        toplevel.send_pending_configure();
                    }
                }
                self.queue_redraw_all();
                true
            }
            None => self.outputs().any(|output| {
                layer_map_for_output(output)
                    .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                    .is_some_and(|layer| layer.cached_state().keyboard_interactivity != KeyboardInteractivity::None)
            }),
        };
        if takes_keys {
            let keyboard = self.keyboard();
            keyboard.set_focus(self, Some(root), SERIAL_COUNTER.next_serial());
        }
    }
}
