//! `wp_viewporter`: a surface shows a crop of its buffer, scaled to a size
//! of its choosing. Video players and browsers use it to scale without
//! drawing again.
//!
//! The crop and the size are double-buffered surface state, applied by the
//! next commit (see `surface.rs`), where a crop outside the buffer is an
//! error.

use std::borrow::Cow;

use fulcrum_wire::{
    Fixed,
    protocol::viewporter::{wp_viewport, wp_viewporter},
};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    surface::{Crop, SurfaceId},
};

/// The `wp_viewporter` version Fulcrum offers.
pub const VIEWPORTER_VERSION: u32 = 1;

impl State {
    pub(crate) fn viewporter_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        request: wp_viewporter::Request,
    ) -> Result<(), ProtocolError> {
        let wp_viewporter::Request::GetViewport { id, surface } = request else {
            return Ok(());
        };
        let surface_id = self
            .surface_of(client_id, surface)
            .expect("the dispatch checked the argument's interface");
        if self.surfaces[&surface_id].viewport.is_some() {
            return Err(ProtocolError {
                object,
                code: wp_viewporter::error::VIEWPORT_EXISTS,
                message: format!("wl_surface#{} already has a viewport", surface.0),
            });
        }
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        client.create(
            id,
            Cow::Borrowed(wp_viewport::NAME),
            Some(&wp_viewport::INFO),
            1,
            ObjectKind::Viewport(surface_id),
        )?;
        if let Some(surface) = self.surfaces.get_mut(&surface_id) {
            surface.viewport = Some(id.0);
        }
        Ok(())
    }

    pub(crate) fn viewport_request(
        &mut self,
        object: u32,
        surface_id: SurfaceId,
        request: wp_viewport::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |code, message: String| ProtocolError { object, code, message };
        let Some(surface) = self.surfaces.get_mut(&surface_id) else {
            if matches!(request, wp_viewport::Request::Destroy) {
                return Ok(());
            }
            return Err(fail(
                wp_viewport::error::NO_SURFACE,
                "the viewport's surface was destroyed".into(),
            ));
        };
        match request {
            wp_viewport::Request::Destroy => {
                // Its crop and size go with the next commit.
                surface.viewport = None;
                surface.pending.source = Some(None);
                surface.pending.destination = Some(None);
            }
            wp_viewport::Request::SetSource { x, y, width, height } => {
                let unset = Fixed::from_int(-1);
                if [x, y, width, height].iter().all(|value| *value == unset) {
                    surface.pending.source = Some(None);
                    return Ok(());
                }
                let crop = Crop {
                    x: x.to_f64(),
                    y: y.to_f64(),
                    width: width.to_f64(),
                    height: height.to_f64(),
                };
                if crop.x < 0.0 || crop.y < 0.0 || crop.width <= 0.0 || crop.height <= 0.0 {
                    return Err(fail(
                        wp_viewport::error::BAD_VALUE,
                        format!("a source of {}x{} at {},{}", crop.width, crop.height, crop.x, crop.y),
                    ));
                }
                surface.pending.source = Some(Some(crop));
            }
            wp_viewport::Request::SetDestination { width, height } => {
                if (width, height) == (-1, -1) {
                    surface.pending.destination = Some(None);
                    return Ok(());
                }
                if width <= 0 || height <= 0 {
                    return Err(fail(
                        wp_viewport::error::BAD_VALUE,
                        format!("a destination of {width}x{height}"),
                    ));
                }
                surface.pending.destination = Some(Some((width, height)));
            }
        }
        Ok(())
    }
}
