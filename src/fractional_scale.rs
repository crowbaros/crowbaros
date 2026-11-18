//! `wp_fractional_scale_manager_v1`: a surface hears the scale it should
//! draw at, in 120ths, so a client can draw for a fractional scale.
//!
//! Every output is at scale 1 until displays get their own scale (ADR 0020,
//! Milestone 7), so the preferred scale is 120. The table offers it to every
//! compartment and the shell (ADR 0020, ADR 0023).

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
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        client.create(
            id,
            Cow::Borrowed(wp_fractional_scale_v1::NAME),
            Some(&wp_fractional_scale_v1::INFO),
            1,
            ObjectKind::FractionalScale(surface_id),
        )?;
        client.send(id.0, wp_fractional_scale_v1::Event::PreferredScale { scale: 120 });
        if let Some(surface) = self.surfaces.get_mut(&surface_id) {
            surface.fractional_scale = Some(id.0);
        }
        Ok(())
    }

    /// A surface's fractional scale object is destroyed: another may be made.
    pub(crate) fn forget_fractional_scale(&mut self, surface: SurfaceId) {
        if let Some(surface) = self.surfaces.get_mut(&surface) {
            surface.fractional_scale = None;
        }
    }
}
