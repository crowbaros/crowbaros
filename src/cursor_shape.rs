//! `wp_cursor_shape_manager_v1`: a client names the shape of its cursor,
//! as "text" or "pointer", and the compositor draws the system theme's
//! image of it, so a client need not carry cursor images of its own.
//!
//! As with `wl_pointer.set_cursor`, only the client whose surface the
//! pointer is on sets the shape, and the compositor's own image comes back
//! when the pointer leaves it (see `cursor.rs`). The table offers it to
//! every compartment and the shell (ADR 0020, ADR 0023).

use std::borrow::Cow;

use fulcrum_wire::protocol::cursor_shape_v1::{wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    cursor::Cursor,
};

/// The `wp_cursor_shape_manager_v1` version Fulcrum offers.
pub const CURSOR_SHAPE_VERSION: u32 = 2;

/// The last shape of each version: version 2 added `dnd_ask` and
/// `all_resize`.
fn last_shape(version: u32) -> u32 {
    if version >= 2 {
        wp_cursor_shape_device_v1::shape::ALL_RESIZE
    } else {
        wp_cursor_shape_device_v1::shape::ZOOM_OUT
    }
}

impl State {
    pub(crate) fn cursor_shape_manager_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        version: u32,
        request: wp_cursor_shape_manager_v1::Request,
    ) -> Result<(), ProtocolError> {
        match request {
            wp_cursor_shape_manager_v1::Request::Destroy => Ok(()),
            wp_cursor_shape_manager_v1::Request::GetPointer {
                cursor_shape_device, ..
            } => {
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    cursor_shape_device,
                    Cow::Borrowed(wp_cursor_shape_device_v1::NAME),
                    Some(&wp_cursor_shape_device_v1::INFO),
                    version,
                    ObjectKind::CursorShapeDevice,
                )
            }
            // No client is offered tablets, so none holds a tool to name.
            wp_cursor_shape_manager_v1::Request::GetTabletToolV2 { .. } => Err(ProtocolError {
                object,
                code: fulcrum_wire::protocol::wayland::wl_display::error::INVALID_OBJECT,
                message: "no client has a tablet tool".into(),
            }),
        }
    }

    pub(crate) fn cursor_shape_device_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        version: u32,
        request: wp_cursor_shape_device_v1::Request,
    ) -> Result<(), ProtocolError> {
        let wp_cursor_shape_device_v1::Request::SetShape { shape, .. } = request else {
            return Ok(());
        };
        if shape == 0 || shape > last_shape(version) {
            return Err(ProtocolError {
                object,
                code: wp_cursor_shape_device_v1::error::INVALID_SHAPE,
                message: format!("{shape} is not a shape of version {version}"),
            });
        }
        // Only the client whose surface the pointer is on sets its image.
        let focused = self
            .seat
            .pointer_focus
            .and_then(|(focus, _)| self.surfaces.get(&focus))
            .is_some_and(|focus| focus.client == client_id);
        if !focused {
            return Ok(());
        }
        self.cursor_theme.load(shape);
        self.seat.cursor = Some(Cursor::Shape(shape));
        self.queue_redraw_all();
        Ok(())
    }
}

impl State {
    /// Draws cursors with a theme: `name`, looked for on `path`, at `size`.
    /// With none, every shape is the compositor's own pointer.
    pub fn use_cursor_theme(&mut self, path: Vec<std::path::PathBuf>, name: &str, size: u32) {
        self.set_cursor_theme(crate::xcursor::CursorTheme::new(path, name, size));
    }

    /// Draws cursors with the theme the environment names: `XCURSOR_THEME`,
    /// `XCURSOR_SIZE`, and `XCURSOR_PATH`.
    pub fn use_cursor_theme_from_environment(&mut self) {
        self.set_cursor_theme(crate::xcursor::CursorTheme::from_environment());
    }

    fn set_cursor_theme(&mut self, mut theme: crate::xcursor::CursorTheme) {
        theme.load(crate::xcursor::DEFAULT_SHAPE);
        if theme.image(crate::xcursor::DEFAULT_SHAPE).is_none() {
            crate::log::info!("no cursor theme: the pointer is the compositor's own square");
        }
        self.cursor_theme = theme;
        self.queue_redraw_all();
    }
}
