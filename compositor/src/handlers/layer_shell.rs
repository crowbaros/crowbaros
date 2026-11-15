use smithay::{
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::SERIAL_COUNTER,
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{
            KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
            WlrLayerShellState,
        },
    },
};

use crate::State;

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let Some(output) = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.outputs.first().cloned())
        else {
            return;
        };
        let mut map = layer_map_for_output(&output);
        let _ = map.map_layer(&LayerSurface::new(surface, namespace));
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        for output in &self.outputs {
            let mut map = layer_map_for_output(output);
            let layer = map.layers().find(|l| l.layer_surface() == &surface).cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
        }
        self.refresh_layer_focus();
    }
}

impl State {
    /// A top or overlay layer surface that asks for the keyboard exclusively
    /// gets it. When the last one goes, the active window gets it back.
    pub fn refresh_layer_focus(&mut self) {
        let wanted = self.outputs.iter().find_map(|output| {
            let map = layer_map_for_output(output);
            map.layers()
                .rev()
                .find(|layer| {
                    matches!(layer.layer(), Layer::Overlay | Layer::Top)
                        && layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
                })
                .map(|layer| layer.wl_surface().clone())
        });
        if wanted == self.layer_focus {
            return;
        }
        self.layer_focus = wanted.clone();
        match wanted {
            Some(surface) => {
                let keyboard = self.seat.get_keyboard().unwrap();
                keyboard.set_focus(self, Some(surface), SERIAL_COUNTER.next_serial());
            }
            None => {
                let active = self
                    .windows
                    .iter()
                    .find(|m| Some(m.id) == self.active_window)
                    .map(|m| m.window.clone());
                self.focus_window(active.as_ref());
            }
        }
    }
}

/// Called on every surface commit. Arranges the layers of the output that
/// holds this surface and sends the first configure.
pub fn handle_commit(state: &mut State, surface: &WlSurface) {
    let Some(output) = state
        .outputs
        .iter()
        .find(|o| {
            layer_map_for_output(o)
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .is_some()
        })
        .cloned()
    else {
        return;
    };
    let initial_configure_sent = with_states(surface, |states| {
        states
            .data_map
            .get::<LayerSurfaceData>()
            .map(|data| data.lock().unwrap().initial_configure_sent)
            .unwrap_or(true)
    });
    let mut map = layer_map_for_output(&output);
    // Arrange before the first configure so the size the client asked for counts.
    let before = map.non_exclusive_zone();
    map.arrange();
    let after = map.non_exclusive_zone();
    if before != after {
        tracing::info!(
            output = %output.name(),
            "usable area is now {}x{} at {},{}",
            after.size.w,
            after.size.h,
            after.loc.x,
            after.loc.y
        );
    }
    if !initial_configure_sent && let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
        layer.layer_surface().send_configure();
    }
    drop(map);
    state.refresh_layer_focus();
}
