//! The shell's layer surfaces: `zwlr_layer_shell_v1` and
//! `zwlr_layer_surface_v1`.
//!
//! The table offers layer shell to the shell alone (ADR 0032), so
//! every layer surface is the shell's: the wallpaper, the bar, the dock, the
//! launcher. Each sits on one output, in one of four layers. The background
//! and bottom layers are drawn below the windows, and the top and overlay
//! layers above them.
//!
//! A layer surface is placed by its anchors and margins, and may reserve an
//! exclusive zone along the edge it is anchored to. What is left is the
//! output's usable area, where windows open. Layers are arranged in the
//! order they were made, so the first to reserve room keeps it; this is the
//! arrangement wlroots and Smithay use.

use std::borrow::Cow;

use fulcrum_wire::protocol::wlr_layer_shell_unstable_v1::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    geometry::Rect,
    surface::{Role, SurfaceId},
};

/// The `zwlr_layer_shell_v1` version Fulcrum offers.
pub const LAYER_SHELL_VERSION: u32 = 4;

use zwlr_layer_surface_v1::anchor as edge;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Placement {
    size: (u32, u32),
    anchor: u32,
    exclusive_zone: i32,
    /// Top, right, bottom, left.
    margin: (i32, i32, i32, i32),
    keyboard: u32,
    layer: u32,
}

pub(crate) struct LayerSurface {
    pub surface: SurfaceId,
    pub client: ClientId,
    pub object: u32,
    pub output: u64,
    pub namespace: String,
    pending: Placement,
    current: Placement,
    /// Where it is on the desktop, from the last arrangement.
    pub rect: Rect,
    sent: Vec<u32>,
    configured: bool,
    initial_commit: bool,
    pub mapped: bool,
}

impl LayerSurface {
    /// Whether it takes the keyboard when clicked.
    pub fn takes_keys(&self) -> bool {
        self.current.keyboard != zwlr_layer_surface_v1::keyboard_interactivity::NONE
    }

    /// Which of the four layers it is in.
    pub fn layer(&self) -> u32 {
        self.current.layer
    }
}

impl State {
    pub(crate) fn layer_shell_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        version: u32,
        request: zwlr_layer_shell_v1::Request,
    ) -> Result<(), ProtocolError> {
        let zwlr_layer_shell_v1::Request::GetLayerSurface {
            id,
            surface,
            output,
            layer,
            namespace,
        } = request
        else {
            return Ok(());
        };
        let fail = |code, message: String| ProtocolError { object, code, message };
        if layer > zwlr_layer_shell_v1::layer::OVERLAY {
            return Err(fail(
                zwlr_layer_shell_v1::error::INVALID_LAYER,
                format!("unknown layer {layer}"),
            ));
        }
        let surface_id = match self.clients[&client_id].objects[&surface.0].kind {
            ObjectKind::Surface(surface) => surface,
            _ => unreachable!("the dispatch checked the argument's interface"),
        };
        if self.surfaces[&surface_id].role != Role::None {
            return Err(fail(
                zwlr_layer_shell_v1::error::ROLE,
                format!("wl_surface#{} already has a role", surface.0),
            ));
        }
        if self.surfaces[&surface_id].current.content.is_some() {
            return Err(fail(
                zwlr_layer_shell_v1::error::ALREADY_CONSTRUCTED,
                format!("wl_surface#{} already has a buffer", surface.0),
            ));
        }
        let output = match output {
            Some(output) => match self.clients[&client_id].objects[&output.0].kind {
                ObjectKind::Output(number) => number,
                _ => unreachable!("the dispatch checked the argument's interface"),
            },
            // The compositor chooses: the first output.
            None => match self.outputs.first() {
                Some(output) => output.number,
                None => {
                    // With no output there is nowhere to show it. It is made
                    // and closed at once.
                    0
                }
            },
        };
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        client.create(
            id,
            Cow::Borrowed(zwlr_layer_surface_v1::NAME),
            Some(&zwlr_layer_surface_v1::INFO),
            version,
            ObjectKind::Layer(surface_id),
        )?;
        self.surfaces.get_mut(&surface_id).expect("exists").role = Role::Layer;
        let placement = Placement {
            layer,
            ..Default::default()
        };
        self.layers.push(LayerSurface {
            surface: surface_id,
            client: client_id,
            object: id.0,
            output,
            namespace,
            pending: placement,
            current: placement,
            rect: Rect::default(),
            sent: Vec::new(),
            configured: false,
            initial_commit: false,
            mapped: false,
        });
        if output == 0 {
            self.close_layer(surface_id);
        }
        Ok(())
    }

    fn layer_mut(&mut self, surface: SurfaceId) -> Option<&mut LayerSurface> {
        self.layers.iter_mut().find(|layer| layer.surface == surface)
    }

    pub(crate) fn layer_of(&self, surface: SurfaceId) -> Option<&LayerSurface> {
        self.layers.iter().find(|layer| layer.surface == surface)
    }

    pub(crate) fn layer_surface_request(
        &mut self,
        object: u32,
        surface: SurfaceId,
        request: zwlr_layer_surface_v1::Request,
    ) -> Result<(), ProtocolError> {
        use zwlr_layer_surface_v1::{Request, error};
        let fail = |code, message: &str| ProtocolError {
            object,
            code,
            message: message.to_string(),
        };
        if matches!(request, Request::Destroy) {
            self.forget_layer(surface);
            return Ok(());
        }
        if let Request::GetPopup { popup } = request {
            let client = self.layer_of(surface).map(|layer| layer.client);
            let popup_surface = client.and_then(|client| match self.clients[&client].objects[&popup.0].kind {
                ObjectKind::Popup(popup) => Some(popup),
                _ => None,
            });
            if let Some(popup) = popup_surface {
                self.adopt_popup(popup, surface);
            }
            return Ok(());
        }
        let Some(layer) = self.layer_mut(surface) else {
            return Ok(());
        };
        match request {
            Request::SetSize { width, height } => layer.pending.size = (width, height),
            Request::SetAnchor { anchor } => {
                if anchor > (edge::TOP | edge::BOTTOM | edge::LEFT | edge::RIGHT) {
                    return Err(fail(error::INVALID_ANCHOR, "an unknown anchor"));
                }
                layer.pending.anchor = anchor;
            }
            Request::SetExclusiveZone { zone } => layer.pending.exclusive_zone = zone,
            Request::SetMargin {
                top,
                right,
                bottom,
                left,
            } => layer.pending.margin = (top, right, bottom, left),
            Request::SetKeyboardInteractivity { keyboard_interactivity } => {
                if keyboard_interactivity > zwlr_layer_surface_v1::keyboard_interactivity::ON_DEMAND {
                    return Err(fail(
                        error::INVALID_KEYBOARD_INTERACTIVITY,
                        "an unknown keyboard interactivity",
                    ));
                }
                layer.pending.keyboard = keyboard_interactivity;
            }
            Request::SetLayer { layer: kind } => {
                if kind > zwlr_layer_shell_v1::layer::OVERLAY {
                    return Err(ProtocolError {
                        object,
                        code: zwlr_layer_shell_v1::error::INVALID_LAYER,
                        message: format!("unknown layer {kind}"),
                    });
                }
                layer.pending.layer = kind;
            }
            Request::AckConfigure { serial } => {
                let Some(at) = layer.sent.iter().position(|sent| *sent == serial) else {
                    return Err(fail(
                        error::INVALID_SURFACE_STATE,
                        "an acknowledgement of a serial never sent",
                    ));
                };
                layer.sent.drain(..=at);
                layer.configured = true;
            }
            Request::SetExclusiveEdge { .. } => {}
            Request::Destroy | Request::GetPopup { .. } => unreachable!("handled above"),
        }
        Ok(())
    }

    /// After a commit on a layer surface: the placement becomes current,
    /// the output is arranged again, and the first commit is configured.
    pub(crate) fn layer_committed(&mut self, surface: SurfaceId) -> Result<(), ProtocolError> {
        let has_buffer = self
            .surfaces
            .get(&surface)
            .is_some_and(|surface| surface.current.content.is_some());
        let Some(layer) = self.layer_mut(surface) else {
            return Ok(());
        };
        let object = layer.object;
        let placement = layer.pending;
        let horizontal = edge::LEFT | edge::RIGHT;
        let vertical = edge::TOP | edge::BOTTOM;
        if (placement.size.0 == 0 && placement.anchor & horizontal != horizontal)
            || (placement.size.1 == 0 && placement.anchor & vertical != vertical)
        {
            return Err(ProtocolError {
                object,
                code: zwlr_layer_surface_v1::error::INVALID_SIZE,
                message: "a size of zero needs the surface anchored to both opposite edges".into(),
            });
        }
        layer.current = placement;
        if !layer.initial_commit {
            if has_buffer {
                return Err(ProtocolError {
                    object,
                    code: zwlr_layer_surface_v1::error::INVALID_SURFACE_STATE,
                    message: "a buffer was committed before the first configure".into(),
                });
            }
            layer.initial_commit = true;
            let output = layer.output;
            self.arrange(output);
            self.configure_layer(surface);
            return Ok(());
        }
        if !layer.configured && has_buffer {
            return Err(ProtocolError {
                object,
                code: zwlr_layer_surface_v1::error::INVALID_SURFACE_STATE,
                message: "a buffer was committed before a configure was acknowledged".into(),
            });
        }
        layer.mapped = has_buffer;
        if !has_buffer {
            // An unmapped layer surface starts again from its first commit.
            layer.initial_commit = false;
            layer.configured = false;
        }
        let output = layer.output;
        let before = self.layer_rects(output);
        self.arrange(output);
        if self.layer_rects(output) != before {
            self.configure_changed_layers(output, &before);
        }
        if has_buffer {
            self.lock_screen_mapped(surface);
        }
        self.queue_redraw(output);
        Ok(())
    }

    /// Places an output's layer surfaces again after the output changed, and
    /// configures each one whose size that changed.
    pub(crate) fn rearrange(&mut self, output: u64) {
        let before = self.layer_rects(output);
        self.arrange(output);
        if self.layer_rects(output) != before {
            self.configure_changed_layers(output, &before);
        }
    }

    fn layer_rects(&self, output: u64) -> Vec<(SurfaceId, Rect)> {
        self.layers
            .iter()
            .filter(|layer| layer.output == output)
            .map(|layer| (layer.surface, layer.rect))
            .collect()
    }

    fn configure_changed_layers(&mut self, output: u64, before: &[(SurfaceId, Rect)]) {
        let changed: Vec<SurfaceId> = self
            .layers
            .iter()
            .filter(|layer| layer.output == output && layer.configured)
            .filter(|layer| {
                before
                    .iter()
                    .find(|(surface, _)| *surface == layer.surface)
                    .is_none_or(|(_, rect)| (rect.width, rect.height) != (layer.rect.width, layer.rect.height))
            })
            .map(|layer| layer.surface)
            .collect();
        for surface in changed {
            self.configure_layer(surface);
        }
    }

    fn configure_layer(&mut self, surface: SurfaceId) {
        let serial = self.next_serial();
        let Some(layer) = self.layer_mut(surface) else {
            return;
        };
        layer.sent.push(serial);
        let (client, object, rect) = (layer.client, layer.object, layer.rect);
        if let Some(client) = self.clients.get_mut(&client) {
            client.send(
                object,
                zwlr_layer_surface_v1::Event::Configure {
                    serial,
                    width: rect.width.max(0) as u32,
                    height: rect.height.max(0) as u32,
                },
            );
        }
    }

    /// Places every layer surface of an output, and works out its usable
    /// area.
    pub(crate) fn arrange(&mut self, output: u64) {
        let Some(full) = self
            .outputs
            .iter()
            .find(|candidate| candidate.number == output)
            .map(|o| o.rect())
        else {
            return;
        };
        let mut usable = full;
        for kind in 0..=zwlr_layer_shell_v1::layer::OVERLAY {
            for layer in self
                .layers
                .iter_mut()
                .filter(|layer| layer.output == output && layer.initial_commit && layer.current.layer == kind)
            {
                let placement = layer.current;
                let bounds = if placement.exclusive_zone == -1 { full } else { usable };
                layer.rect = place(&placement, bounds);
                if placement.exclusive_zone > 0 {
                    usable = reserve(usable, &placement);
                }
            }
        }
        self.usable.insert(output, usable);
        // A maximized or snapped window fills the room the layers leave.
        self.refit_windows(output);
        let windows: Vec<SurfaceId> = self.windows.iter().map(|window| window.surface).collect();
        for surface in windows {
            self.update_outputs_of(surface);
        }
    }

    /// The desktop rectangle a mapped layer surface covers.
    pub(crate) fn layer_rect(&self, surface: SurfaceId) -> Option<Rect> {
        self.layer_of(surface)
            .filter(|layer| layer.mapped)
            .map(|layer| layer.rect)
    }

    /// Every mapped layer surface.
    pub(crate) fn layer_surfaces(&self) -> Vec<SurfaceId> {
        self.layers
            .iter()
            .filter(|layer| layer.mapped)
            .map(|layer| layer.surface)
            .collect()
    }

    /// A layer surface is gone, or its client is.
    pub(crate) fn forget_layer(&mut self, surface: SurfaceId) {
        let Some(index) = self.layers.iter().position(|layer| layer.surface == surface) else {
            return;
        };
        let layer = self.layers.remove(index);
        self.drag_surface_unmapped(surface);
        if let Some(surface) = self.surfaces.get_mut(&surface)
            && surface.role == Role::Layer
        {
            surface.role = Role::None;
        }
        self.drop_focus_from(surface);
        self.forget_popups_of(surface);
        self.arrange(layer.output);
        self.queue_redraw(layer.output);
    }

    /// The output a layer surface is on went away: it is closed, and the
    /// shell may make it again on another.
    pub(crate) fn close_layer(&mut self, surface: SurfaceId) {
        let Some(layer) = self.layer_mut(surface) else {
            return;
        };
        layer.mapped = false;
        let (client, object) = (layer.client, layer.object);
        if let Some(client) = self.clients.get_mut(&client) {
            client.send(object, zwlr_layer_surface_v1::Event::Closed);
        }
        self.drag_surface_unmapped(surface);
    }

    /// The part of an output that the shell's layers have not reserved, on
    /// the desktop.
    pub fn usable_area(&self, output: u64) -> Rect {
        if let Some(area) = self.usable.get(&output) {
            return *area;
        }
        self.outputs
            .iter()
            .find(|candidate| candidate.number == output)
            .map_or(Rect::default(), |output| output.rect())
    }

    /// Each layer surface's namespace and place, in the order they were made.
    pub fn layer_rects_by_namespace(&self) -> Vec<(String, Rect)> {
        self.layers
            .iter()
            .filter(|layer| layer.mapped)
            .map(|layer| (layer.namespace.clone(), layer.rect))
            .collect()
    }
}

/// Where a layer surface goes inside `bounds`, by its anchors, margins, and
/// size. A size of zero stretches between the anchored edges.
fn place(placement: &Placement, bounds: Rect) -> Rect {
    let (top, right, bottom, left) = placement.margin;
    let anchored = |side| placement.anchor & side != 0;
    let width = if placement.size.0 == 0 {
        bounds.width - left - right
    } else {
        placement.size.0 as i32
    };
    let height = if placement.size.1 == 0 {
        bounds.height - top - bottom
    } else {
        placement.size.1 as i32
    };
    let x = match (anchored(edge::LEFT), anchored(edge::RIGHT)) {
        (true, false) => bounds.x + left,
        (false, true) => bounds.x + bounds.width - right - width,
        (true, true) if placement.size.0 == 0 => bounds.x + left,
        _ => bounds.x + (bounds.width - width) / 2,
    };
    let y = match (anchored(edge::TOP), anchored(edge::BOTTOM)) {
        (true, false) => bounds.y + top,
        (false, true) => bounds.y + bounds.height - bottom - height,
        (true, true) if placement.size.1 == 0 => bounds.y + top,
        _ => bounds.y + (bounds.height - height) / 2,
    };
    Rect::new(x, y, width.max(0), height.max(0))
}

/// The usable area after a layer reserves its exclusive zone along the one
/// edge it is anchored to. A layer anchored to two opposite edges, or to no
/// edge, or to a corner, reserves nothing, as in wlroots.
fn reserve(usable: Rect, placement: &Placement) -> Rect {
    let (top, right, bottom, left) = placement.margin;
    let zone = placement.exclusive_zone;
    let all = edge::TOP | edge::BOTTOM | edge::LEFT | edge::RIGHT;
    let edge_of = match placement.anchor {
        a if a == edge::TOP || a == all & !edge::BOTTOM => Some(edge::TOP),
        a if a == edge::BOTTOM || a == all & !edge::TOP => Some(edge::BOTTOM),
        a if a == edge::LEFT || a == all & !edge::RIGHT => Some(edge::LEFT),
        a if a == edge::RIGHT || a == all & !edge::LEFT => Some(edge::RIGHT),
        _ => None,
    };
    match edge_of {
        Some(edge::TOP) => Rect::new(
            usable.x,
            usable.y + zone + top,
            usable.width,
            usable.height - zone - top,
        ),
        Some(edge::BOTTOM) => Rect::new(usable.x, usable.y, usable.width, usable.height - zone - bottom),
        Some(edge::LEFT) => Rect::new(
            usable.x + zone + left,
            usable.y,
            usable.width - zone - left,
            usable.height,
        ),
        Some(edge::RIGHT) => Rect::new(usable.x, usable.y, usable.width - zone - right, usable.height),
        _ => usable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(size: (u32, u32), anchor: u32, zone: i32) -> Placement {
        Placement {
            size,
            anchor,
            exclusive_zone: zone,
            ..Default::default()
        }
    }

    #[test]
    fn a_bar_stretches_across_the_top_and_reserves_its_height() {
        let output = Rect::new(0, 0, 1280, 800);
        let bar = placement((0, 44), edge::TOP | edge::LEFT | edge::RIGHT, 44);
        assert_eq!(place(&bar, output), Rect::new(0, 0, 1280, 44));
        assert_eq!(reserve(output, &bar), Rect::new(0, 44, 1280, 756));
    }

    #[test]
    fn a_dock_anchored_to_the_bottom_alone_is_centred_and_reserves_its_height() {
        let output = Rect::new(0, 0, 1280, 800);
        let dock = placement((400, 96), edge::BOTTOM, 96);
        assert_eq!(place(&dock, output), Rect::new(440, 704, 400, 96));
        assert_eq!(reserve(output, &dock), Rect::new(0, 0, 1280, 704));
    }

    #[test]
    fn a_corner_or_a_whole_screen_reserves_nothing() {
        let output = Rect::new(0, 0, 100, 100);
        for anchor in [
            edge::TOP | edge::LEFT,
            edge::TOP | edge::BOTTOM | edge::LEFT | edge::RIGHT,
            0,
        ] {
            assert_eq!(reserve(output, &placement((10, 10), anchor, 10)), output);
        }
    }

    #[test]
    fn margins_move_a_layer_in_from_its_edge() {
        let output = Rect::new(0, 0, 100, 100);
        let mut layer = placement((10, 10), edge::RIGHT, 0);
        layer.margin = (0, 5, 0, 0);
        assert_eq!(place(&layer, output), Rect::new(85, 45, 10, 10));
    }
}
