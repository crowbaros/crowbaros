//! `wp_fractional_scale_manager_v1`: a surface hears the scale it should
//! draw at, in 120ths, so a client can draw for a fractional scale.
//!
//! A surface's preferred scale is the largest of the scales of the outputs
//! it is on, or 1 where it is on none, and it hears it again whenever that
//! changes. The table offers it to every compartment and the shell (ADR 0020,
//! ADR 0023).

use std::borrow::Cow;

use fulcrum_wire::protocol::fractional_scale_v1::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    surface::SurfaceId,
};

/// The `wp_fractional_scale_manager_v1` version Fulcrum offers.
pub const FRACTIONAL_SCALE_VERSION: u32 = 1;

impl State {
    pub(crate) fn fractional_scale_manager_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        request: wp_fractional_scale_manager_v1::Request,
    ) -> Result<(), ProtocolError> {
        let wp_fractional_scale_manager_v1::Request::GetFractionalScale { id, surface } = request else {
            return Ok(());
        };
        let surface_id = self
            .surface_of(client_id, surface)
            .expect("the dispatch checked the argument's interface");
        if self.surfaces[&surface_id].fractional_scale.is_some() {
            return Err(ProtocolError {
                object,
                code: wp_fractional_scale_manager_v1::error::FRACTIONAL_SCALE_EXISTS,
                message: format!("wl_surface#{} already has a fractional scale", surface.0),
            });
        }
        let scale = self.preferred_scale(surface_id);
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        client.create(
            id,
            Cow::Borrowed(wp_fractional_scale_v1::NAME),
            Some(&wp_fractional_scale_v1::INFO),
            1,
            ObjectKind::FractionalScale(surface_id),
        )?;
        client.send(id.0, wp_fractional_scale_v1::Event::PreferredScale { scale });
        if let Some(surface) = self.surfaces.get_mut(&surface_id) {
            surface.fractional_scale = Some(id.0);
            surface.fractional_sent = scale;
        }
        Ok(())
    }

    /// The scale a surface should draw at, in 120ths: the largest of the
    /// outputs its window or layer is on.
    pub(crate) fn preferred_scale(&self, surface: SurfaceId) -> u32 {
        let root = self.root_of(surface);
        let root = self.toplevel_of(root).unwrap_or(root);
        let entered = self.surfaces.get(&root).map_or(&[][..], |surface| &surface.entered[..]);
        self.outputs
            .iter()
            .filter(|output| entered.contains(&output.number))
            .map(|output| output.scale_120)
            .max()
            .unwrap_or(crate::output::SCALE_ONE)
    }

    /// Tells each surface with a fractional scale object its preferred scale,
    /// where it changed.
    pub(crate) fn update_fractional_scales(&mut self) {
        let surfaces: Vec<SurfaceId> = self
            .surfaces
            .iter()
            .filter(|(_, surface)| surface.fractional_scale.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in surfaces {
            let scale = self.preferred_scale(id);
            let Some(surface) = self.surfaces.get_mut(&id) else {
                continue;
            };
            let (Some(object), client_id) = (surface.fractional_scale, surface.client) else {
                continue;
            };
            if surface.fractional_sent == scale {
                continue;
            }
            surface.fractional_sent = scale;
            if let Some(client) = self.clients.get_mut(&client_id) {
                client.send(object, wp_fractional_scale_v1::Event::PreferredScale { scale });
            }
        }
    }

    /// A surface's fractional scale object is destroyed: another may be made.
    pub(crate) fn forget_fractional_scale(&mut self, surface: SurfaceId) {
        if let Some(surface) = self.surfaces.get_mut(&surface) {
            surface.fractional_scale = None;
        }
    }
}
