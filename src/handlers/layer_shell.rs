//! Layer surfaces. The table offers layer shell to the shell alone, so every
//! surface here is the shell's: the bar, the dock, the launcher, and the
//! wallpaper.

use smithay::{
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{
            Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState,
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
            .or_else(|| self.outputs().next().cloned())
        else {
            return;
        };
        if let Err(error) = layer_map_for_output(&output).map_layer(&LayerSurface::new(surface, namespace)) {
            tracing::warn!(%error, "could not show a layer surface");
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        for output in self.outputs() {
            let mut map = layer_map_for_output(output);
            let layer = map.layers().find(|layer| layer.layer_surface() == &surface).cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
        }
        self.drop_focus_from(surface.wl_surface());
        self.queue_redraw_all();
    }
}

impl State {
    /// Arranges the layers of the output that holds this layer surface, and
    /// sends the surface its first configure.
    pub(crate) fn layer_commit(&mut self, surface: &WlSurface) {
        let Some(output) = self
            .outputs()
            .find(|output| {
                layer_map_for_output(output)
                    .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .cloned()
        else {
            return;
        };
        let mut map = layer_map_for_output(&output);
        // Arrange before the first configure, so the size the shell asked
        // for counts.
        map.arrange();
        let configured = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .is_none_or(|data| data.lock().expect("no panic holds this lock").initial_configure_sent)
        });
        if !configured && let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
            layer.layer_surface().send_configure();
        }
    }
}
