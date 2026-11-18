//! xdg-shell: `xdg_wm_base`, `xdg_positioner`, `xdg_surface`, `xdg_toplevel`,
//! and `xdg_popup`.
//!
//! A client makes an `xdg_surface` for a surface with no role, then gives it
//! the toplevel or popup role, and commits with no buffer. The compositor
//! answers with a configure. Once the client has acknowledged one, a commit
//! with a buffer maps the surface: a toplevel becomes a window, and a popup
//! shows next to its parent. A commit with no buffer unmaps it, and the
//! client starts again from its first commit.
//!
//! What a window may ask for (moving, resizing, maximizing, full screen) is
//! recorded here and carried out with the window manager (CrowbarOS issue
//! 78).

use std::borrow::Cow;

use fulcrum_wire::{
    ObjectId,
    protocol::xdg_shell::{xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base},
};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    geometry::Rect,
    surface::{Role, SurfaceId},
};

/// The `xdg_wm_base` version Fulcrum offers.
pub const XDG_WM_BASE_VERSION: u32 = 6;

pub(crate) type PositionerId = u64;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Positioner {
    size: Option<(i32, i32)>,
    anchor_rect: Option<Rect>,
    anchor: u32,
    gravity: u32,
    offset: (i32, i32),
}

impl Positioner {
    /// Where a popup goes, relative to its parent's window geometry: the
    /// anchor point on the anchor rectangle, then the popup laid out from
    /// it in the gravity's direction, then the offset.
    fn place(&self) -> Option<Rect> {
        let (width, height) = self.size?;
        let anchor = self.anchor_rect?;
        use xdg_positioner::{anchor as a, gravity as g};
        let x = match self.anchor {
            a::LEFT | a::TOP_LEFT | a::BOTTOM_LEFT => anchor.x,
            a::RIGHT | a::TOP_RIGHT | a::BOTTOM_RIGHT => anchor.x + anchor.width,
            _ => anchor.x + anchor.width / 2,
        };
        let y = match self.anchor {
            a::TOP | a::TOP_LEFT | a::TOP_RIGHT => anchor.y,
            a::BOTTOM | a::BOTTOM_LEFT | a::BOTTOM_RIGHT => anchor.y + anchor.height,
            _ => anchor.y + anchor.height / 2,
        };
        let x = match self.gravity {
            g::LEFT | g::TOP_LEFT | g::BOTTOM_LEFT => x - width,
            g::RIGHT | g::TOP_RIGHT | g::BOTTOM_RIGHT => x,
            _ => x - width / 2,
        };
        let y = match self.gravity {
            g::TOP | g::TOP_LEFT | g::TOP_RIGHT => y - height,
            g::BOTTOM | g::BOTTOM_LEFT | g::BOTTOM_RIGHT => y,
            _ => y - height / 2,
        };
        Some(Rect::new(x + self.offset.0, y + self.offset.1, width, height))
    }
}

/// The role an `xdg_surface` was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XdgRole {
    None,
    Toplevel {
        object: u32,
    },
    /// `parent` is the parent's surface; `at` is relative to the parent's
    /// window geometry.
    Popup {
        object: u32,
        parent: Option<SurfaceId>,
        at: Rect,
    },
}

/// What a toplevel asked for.
#[derive(Clone, Debug, Default)]
pub struct ToplevelRequests {
    pub title: String,
    pub app_id: String,
    pub parent: Option<SurfaceId>,
    pub min_size: (i32, i32),
    pub max_size: (i32, i32),
    pub maximized: bool,
    pub fullscreen: bool,
}

pub(crate) struct XdgSurface {
    pub client: ClientId,
    pub object: u32,
    /// The `xdg_wm_base` it was made from.
    pub wm_base: u32,
    pub role: XdgRole,
    pub toplevel: ToplevelRequests,
    pending_geometry: Option<Rect>,
    pub geometry: Option<Rect>,
    /// Serials sent and not yet acknowledged.
    sent: Vec<u32>,
    /// The client acknowledged a configure since its first commit.
    configured: bool,
    /// The first commit, with no buffer, has happened.
    initial_commit: bool,
    pub mapped: bool,
}

impl State {
    pub(crate) fn next_serial(&mut self) -> u32 {
        self.serial_counter = self.serial_counter.wrapping_add(1).max(1);
        self.serial_counter
    }

    pub(crate) fn wm_base_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        version: u32,
        request: xdg_wm_base::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |code, message: String| ProtocolError { object, code, message };
        match request {
            xdg_wm_base::Request::Destroy => {
                if self
                    .xdg
                    .values()
                    .any(|xdg| xdg.client == client_id && xdg.wm_base == object)
                {
                    return Err(fail(
                        xdg_wm_base::error::DEFUNCT_SURFACES,
                        "xdg_wm_base destroyed before its surfaces".into(),
                    ));
                }
                Ok(())
            }
            xdg_wm_base::Request::CreatePositioner { id } => {
                self.next_resource += 1;
                let positioner = self.next_resource;
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(xdg_positioner::NAME),
                    Some(&xdg_positioner::INFO),
                    version,
                    ObjectKind::Positioner(positioner),
                )?;
                self.positioners.insert(positioner, Positioner::default());
                Ok(())
            }
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                let surface_id = self.surface_object(client_id, surface);
                let target = &self.surfaces[&surface_id];
                if target.role != Role::None || self.xdg.contains_key(&surface_id) {
                    return Err(fail(
                        xdg_wm_base::error::ROLE,
                        format!("wl_surface#{} already has a role", surface.0),
                    ));
                }
                if target.current.content.is_some() {
                    return Err(fail(
                        xdg_wm_base::error::INVALID_SURFACE_STATE,
                        format!("wl_surface#{} already has a buffer", surface.0),
                    ));
                }
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(xdg_surface::NAME),
                    Some(&xdg_surface::INFO),
                    version,
                    ObjectKind::XdgSurface(surface_id),
                )?;
                self.xdg.insert(
                    surface_id,
                    XdgSurface {
                        client: client_id,
                        object: id.0,
                        wm_base: object,
                        role: XdgRole::None,
                        toplevel: ToplevelRequests::default(),
                        pending_geometry: None,
                        geometry: None,
                        sent: Vec::new(),
                        configured: false,
                        initial_commit: false,
                        mapped: false,
                    },
                );
                Ok(())
            }
            xdg_wm_base::Request::Pong { .. } => Ok(()),
        }
    }

    fn surface_object(&self, client_id: ClientId, object: ObjectId) -> SurfaceId {
        match self.clients[&client_id].objects[&object.0].kind {
            ObjectKind::Surface(surface) => surface,
            _ => unreachable!("the dispatch checked the argument's interface"),
        }
    }

    pub(crate) fn positioner_request(
        &mut self,
        object: u32,
        positioner: PositionerId,
        request: xdg_positioner::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |message: &str| ProtocolError {
            object,
            code: xdg_positioner::error::INVALID_INPUT,
            message: message.to_string(),
        };
        let state = self
            .positioners
            .get_mut(&positioner)
            .expect("a positioner lives as long as its object");
        match request {
            xdg_positioner::Request::Destroy => {
                self.positioners.remove(&positioner);
            }
            xdg_positioner::Request::SetSize { width, height } => {
                if width <= 0 || height <= 0 {
                    return Err(fail("a positioner's size must be positive"));
                }
                state.size = Some((width, height));
            }
            xdg_positioner::Request::SetAnchorRect { x, y, width, height } => {
                if width < 0 || height < 0 {
                    return Err(fail("an anchor rectangle's size must not be negative"));
                }
                state.anchor_rect = Some(Rect::new(x, y, width, height));
            }
            xdg_positioner::Request::SetAnchor { anchor } => {
                if anchor > xdg_positioner::anchor::BOTTOM_RIGHT {
                    return Err(fail("an unknown anchor"));
                }
                state.anchor = anchor;
            }
            xdg_positioner::Request::SetGravity { gravity } => {
                if gravity > xdg_positioner::gravity::BOTTOM_RIGHT {
                    return Err(fail("an unknown gravity"));
                }
                state.gravity = gravity;
            }
            xdg_positioner::Request::SetOffset { x, y } => state.offset = (x, y),
            xdg_positioner::Request::SetConstraintAdjustment { .. }
            | xdg_positioner::Request::SetReactive
            | xdg_positioner::Request::SetParentSize { .. }
            | xdg_positioner::Request::SetParentConfigure { .. } => {}
        }
        Ok(())
    }

    pub(crate) fn xdg_surface_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        version: u32,
        surface: SurfaceId,
        request: xdg_surface::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |code, message: String| ProtocolError { object, code, message };
        match request {
            xdg_surface::Request::Destroy => {
                if self.xdg.get(&surface).is_some_and(|xdg| xdg.role != XdgRole::None) {
                    return Err(fail(
                        xdg_surface::error::DEFUNCT_ROLE_OBJECT,
                        "xdg_surface destroyed before its role object".into(),
                    ));
                }
                self.xdg.remove(&surface);
                Ok(())
            }
            xdg_surface::Request::GetToplevel { id } => {
                self.give_role(surface, object)?;
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(xdg_toplevel::NAME),
                    Some(&xdg_toplevel::INFO),
                    version,
                    ObjectKind::Toplevel(surface),
                )?;
                self.surfaces.get_mut(&surface).expect("exists").role = Role::Toplevel;
                self.xdg.get_mut(&surface).expect("exists").role = XdgRole::Toplevel { object: id.0 };
                Ok(())
            }
            xdg_surface::Request::GetPopup { id, parent, positioner } => {
                self.give_role(surface, object)?;
                let parent = match parent {
                    Some(parent) => match self.clients[&client_id].objects[&parent.0].kind {
                        ObjectKind::XdgSurface(parent) => Some(parent),
                        _ => unreachable!("the dispatch checked the argument's interface"),
                    },
                    None => None,
                };
                let positioner = match self.clients[&client_id].objects[&positioner.0].kind {
                    ObjectKind::Positioner(positioner) => self.positioners[&positioner],
                    _ => unreachable!("the dispatch checked the argument's interface"),
                };
                let Some(at) = positioner.place() else {
                    return Err(ProtocolError {
                        object,
                        code: xdg_wm_base::error::INVALID_POSITIONER,
                        message: "a popup's positioner needs a size and an anchor rectangle".into(),
                    });
                };
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(xdg_popup::NAME),
                    Some(&xdg_popup::INFO),
                    version,
                    ObjectKind::Popup(surface),
                )?;
                self.surfaces.get_mut(&surface).expect("exists").role = Role::Popup;
                self.xdg.get_mut(&surface).expect("exists").role = XdgRole::Popup {
                    object: id.0,
                    parent,
                    at,
                };
                self.popups.push(surface);
                Ok(())
            }
            xdg_surface::Request::SetWindowGeometry { x, y, width, height } => {
                if width <= 0 || height <= 0 {
                    return Err(fail(
                        xdg_surface::error::INVALID_SIZE,
                        "a window geometry's size must be positive".into(),
                    ));
                }
                self.xdg.get_mut(&surface).expect("exists").pending_geometry = Some(Rect::new(x, y, width, height));
                Ok(())
            }
            xdg_surface::Request::AckConfigure { serial } => {
                let xdg = self.xdg.get_mut(&surface).expect("exists");
                let Some(at) = xdg.sent.iter().position(|sent| *sent == serial) else {
                    return Err(fail(
                        xdg_surface::error::INVALID_SERIAL,
                        format!("serial {serial} was never sent"),
                    ));
                };
                xdg.sent.drain(..=at);
                xdg.configured = true;
                Ok(())
            }
        }
    }

    fn give_role(&mut self, surface: SurfaceId, object: u32) -> Result<(), ProtocolError> {
        let xdg = self.xdg.get(&surface).expect("exists");
        if xdg.role != XdgRole::None {
            return Err(ProtocolError {
                object,
                code: xdg_surface::error::ALREADY_CONSTRUCTED,
                message: "the xdg_surface already has a role".into(),
            });
        }
        if self.surfaces[&surface].role != Role::None {
            return Err(ProtocolError {
                object,
                code: xdg_wm_base::error::ROLE,
                message: "the surface already has a role".into(),
            });
        }
        Ok(())
    }

    pub(crate) fn toplevel_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        surface: SurfaceId,
        request: xdg_toplevel::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |code, message: &str| ProtocolError {
            object,
            code,
            message: message.to_string(),
        };
        match request {
            xdg_toplevel::Request::Destroy => {
                self.unmap_xdg(surface);
                if let Some(xdg) = self.xdg.get_mut(&surface) {
                    xdg.role = XdgRole::None;
                }
            }
            xdg_toplevel::Request::SetParent { parent } => {
                let parent = parent.map(|parent| match self.clients[&client_id].objects[&parent.0].kind {
                    ObjectKind::Toplevel(parent) => parent,
                    _ => unreachable!("the dispatch checked the argument's interface"),
                });
                // A toplevel may not be its own ancestor.
                let mut at = parent;
                while let Some(ancestor) = at {
                    if ancestor == surface {
                        return Err(fail(
                            xdg_toplevel::error::INVALID_PARENT,
                            "a toplevel would be its own ancestor",
                        ));
                    }
                    at = self.xdg.get(&ancestor).and_then(|xdg| xdg.toplevel.parent);
                }
                self.xdg_mut(surface).toplevel.parent = parent;
            }
            xdg_toplevel::Request::SetTitle { title } => self.xdg_mut(surface).toplevel.title = title,
            xdg_toplevel::Request::SetAppId { app_id } => self.xdg_mut(surface).toplevel.app_id = app_id,
            xdg_toplevel::Request::SetMaxSize { width, height }
            | xdg_toplevel::Request::SetMinSize { width, height }
                if width < 0 || height < 0 =>
            {
                return Err(fail(
                    xdg_toplevel::error::INVALID_SIZE,
                    "a size limit must not be negative",
                ));
            }
            xdg_toplevel::Request::SetMaxSize { width, height } => {
                self.xdg_mut(surface).toplevel.max_size = (width, height);
            }
            xdg_toplevel::Request::SetMinSize { width, height } => {
                self.xdg_mut(surface).toplevel.min_size = (width, height);
            }
            xdg_toplevel::Request::SetMaximized => {
                self.xdg_mut(surface).toplevel.maximized = true;
                if self.mode_of(surface) != crate::modes::Mode::Fullscreen {
                    self.set_mode(surface, crate::modes::Mode::Maximized);
                }
            }
            xdg_toplevel::Request::UnsetMaximized => {
                self.xdg_mut(surface).toplevel.maximized = false;
                if self.mode_of(surface) == crate::modes::Mode::Maximized {
                    self.set_mode(surface, crate::modes::Mode::Floating);
                }
            }
            // The output a client names is not taken: it goes fullscreen on
            // the output it is on.
            xdg_toplevel::Request::SetFullscreen { .. } => {
                self.xdg_mut(surface).toplevel.fullscreen = true;
                self.set_mode(surface, crate::modes::Mode::Fullscreen);
            }
            xdg_toplevel::Request::UnsetFullscreen => {
                self.xdg_mut(surface).toplevel.fullscreen = false;
                if self.mode_of(surface) == crate::modes::Mode::Fullscreen {
                    self.set_mode(surface, crate::modes::Mode::Floating);
                }
            }
            xdg_toplevel::Request::Resize { edges, .. } if edges > xdg_toplevel::resize_edge::BOTTOM_RIGHT => {
                return Err(fail(xdg_toplevel::error::INVALID_RESIZE_EDGE, "an unknown edge"));
            }
            // A move or a resize is taken only while a button is held on
            // the window, as after the press that asked for it.
            xdg_toplevel::Request::Move { .. } => {
                if let Some(button) = self.button_held_on(surface) {
                    self.start_move(surface, button);
                }
            }
            xdg_toplevel::Request::Resize { edges, .. } => {
                if let Some(button) = self.button_held_on(surface) {
                    self.start_resize(surface, button, edges);
                }
            }
            // No window menu, and no minimized state: the bar and
            // workspaces stand for them.
            xdg_toplevel::Request::ShowWindowMenu { .. } | xdg_toplevel::Request::SetMinimized => {}
        }
        Ok(())
    }

    fn xdg_mut(&mut self, surface: SurfaceId) -> &mut XdgSurface {
        self.xdg
            .get_mut(&surface)
            .expect("an xdg surface lives as long as its objects")
    }

    pub(crate) fn popup_request(
        &mut self,
        object: u32,
        surface: SurfaceId,
        request: xdg_popup::Request,
    ) -> Result<(), ProtocolError> {
        match request {
            xdg_popup::Request::Destroy => {
                let has_children = self.popups.iter().any(|popup| {
                    matches!(self.xdg.get(popup).map(|xdg| xdg.role), Some(XdgRole::Popup { parent: Some(parent), .. }) if parent == surface)
                });
                if has_children {
                    return Err(ProtocolError {
                        object,
                        code: xdg_wm_base::error::NOT_THE_TOPMOST_POPUP,
                        message: "a popup destroyed before the popups above it".into(),
                    });
                }
                self.unmap_xdg(surface);
                self.popups.retain(|popup| *popup != surface);
                if let Some(xdg) = self.xdg.get_mut(&surface) {
                    xdg.role = XdgRole::None;
                }
            }
            // Grabs come with the seat.
            xdg_popup::Request::Grab { .. } => {}
            xdg_popup::Request::Reposition { positioner, token } => {
                let client = self.xdg[&surface].client;
                let placed = match self.clients[&client].objects[&positioner.0].kind {
                    ObjectKind::Positioner(positioner) => self.positioners[&positioner].place(),
                    _ => None,
                };
                if let Some(at) = placed
                    && let Some(xdg) = self.xdg.get_mut(&surface)
                    && let XdgRole::Popup { at: current, .. } = &mut xdg.role
                {
                    *current = at;
                    let (popup_object, xdg_object) = (Self::popup_object(xdg), xdg.object);
                    let serial = self.next_serial();
                    self.xdg_mut(surface).sent.push(serial);
                    if let Some(client) = self.clients.get_mut(&client) {
                        client.send(popup_object, xdg_popup::Event::Repositioned { token });
                        client.send(
                            popup_object,
                            xdg_popup::Event::Configure {
                                x: at.x,
                                y: at.y,
                                width: at.width,
                                height: at.height,
                            },
                        );
                        client.send(xdg_object, xdg_surface::Event::Configure { serial });
                    }
                }
            }
        }
        Ok(())
    }

    fn popup_object(xdg: &XdgSurface) -> u32 {
        match xdg.role {
            XdgRole::Popup { object, .. } => object,
            _ => 0,
        }
    }

    /// After a commit on a surface with an `xdg_surface`: the first commit
    /// asks for a configure, and a commit after the configure maps or unmaps.
    pub(crate) fn xdg_committed(&mut self, surface: SurfaceId) -> Result<(), ProtocolError> {
        let Some(xdg) = self.xdg.get_mut(&surface) else {
            return Ok(());
        };
        let object = xdg.object;
        if xdg.role == XdgRole::None {
            return Err(ProtocolError {
                object,
                code: xdg_surface::error::NOT_CONSTRUCTED,
                message: "an xdg_surface committed before it had a role".into(),
            });
        }
        if let Some(geometry) = xdg.pending_geometry.take() {
            xdg.geometry = Some(geometry);
        }
        let has_buffer = self.surfaces[&surface].current.content.is_some();
        let xdg = self.xdg.get_mut(&surface).expect("exists");
        if !xdg.initial_commit {
            if has_buffer {
                return Err(ProtocolError {
                    object,
                    code: xdg_surface::error::UNCONFIGURED_BUFFER,
                    message: "a buffer was committed before the first configure".into(),
                });
            }
            xdg.initial_commit = true;
            self.configure(surface);
            return Ok(());
        }
        if !xdg.configured {
            if has_buffer {
                return Err(ProtocolError {
                    object,
                    code: xdg_surface::error::UNCONFIGURED_BUFFER,
                    message: "a buffer was committed before a configure was acknowledged".into(),
                });
            }
            return Ok(());
        }
        let mapped = xdg.mapped;
        self.anchor_resized(surface);
        match (has_buffer, mapped) {
            (true, false) => self.map_xdg(surface),
            (false, true) => {
                self.unmap_xdg(surface);
                // An unmapped surface starts again from its first commit.
                let xdg = self.xdg_mut(surface);
                xdg.initial_commit = false;
                xdg.configured = false;
            }
            _ => {}
        }
        Ok(())
    }

    /// Sends a configure for the surface's role, then its serial.
    pub(crate) fn configure(&mut self, surface: SurfaceId) {
        let serial = self.next_serial();
        let activated = self.keyboard_focus_surface() == Some(surface);
        let size = if matches!(
            self.xdg.get(&surface).map(|xdg| xdg.role),
            Some(XdgRole::Toplevel { .. })
        ) {
            self.configure_size(surface)
        } else {
            (0, 0)
        };
        let mode = self.mode_of(surface);
        let resizing = self.resizing(surface);
        let pending_mode = {
            let requests = &self.xdg[&surface].toplevel;
            match mode {
                crate::modes::Mode::Floating if !self.windows.iter().any(|w| w.surface == surface) => {
                    if requests.fullscreen {
                        crate::modes::Mode::Fullscreen
                    } else if requests.maximized {
                        crate::modes::Mode::Maximized
                    } else {
                        mode
                    }
                }
                _ => mode,
            }
        };
        let xdg = self.xdg_mut(surface);
        xdg.sent.push(serial);
        let (client, object, role) = (xdg.client, xdg.object, xdg.role);
        let Some(client) = self.clients.get_mut(&client) else {
            return;
        };
        match role {
            XdgRole::Toplevel { object: toplevel } => {
                let version = client.objects.get(&toplevel).map_or(1, |object| object.version);
                let mut states = Vec::new();
                let mut state = |value: u32| states.extend_from_slice(&value.to_ne_bytes());
                use crate::modes::Mode;
                use xdg_toplevel::state;
                match pending_mode {
                    Mode::Floating => {}
                    Mode::Maximized => state(state::MAXIMIZED),
                    Mode::Fullscreen => state(state::FULLSCREEN),
                    // The tiled states came with version 2.
                    Mode::Left | Mode::Right if version >= 2 => {
                        let side = if pending_mode == Mode::Left {
                            state::TILED_LEFT
                        } else {
                            state::TILED_RIGHT
                        };
                        for value in [side, state::TILED_TOP, state::TILED_BOTTOM] {
                            state(value);
                        }
                    }
                    Mode::Left | Mode::Right => state(state::MAXIMIZED),
                }
                if activated {
                    state(state::ACTIVATED);
                }
                if resizing {
                    state(state::RESIZING);
                }
                if version >= 5 {
                    // What a client may ask the window manager for.
                    let capabilities: Vec<u8> = [
                        xdg_toplevel::wm_capabilities::MAXIMIZE,
                        xdg_toplevel::wm_capabilities::FULLSCREEN,
                    ]
                    .iter()
                    .flat_map(|capability| capability.to_ne_bytes())
                    .collect();
                    client.send(toplevel, xdg_toplevel::Event::WmCapabilities { capabilities });
                }
                client.send(
                    toplevel,
                    xdg_toplevel::Event::Configure {
                        width: size.0,
                        height: size.1,
                        states,
                    },
                );
            }
            XdgRole::Popup { object: popup, at, .. } => {
                client.send(
                    popup,
                    xdg_popup::Event::Configure {
                        x: at.x,
                        y: at.y,
                        width: at.width,
                        height: at.height,
                    },
                );
            }
            XdgRole::None => {}
        }
        client.send(object, xdg_surface::Event::Configure { serial });
    }

    /// The xdg surfaces of a client that is gone, or a surface destroyed:
    /// their windows and popups go, and child popups are told they are done.
    pub(crate) fn forget_xdg(&mut self, surface: SurfaceId) {
        self.unmap_xdg(surface);
        self.popups.retain(|popup| *popup != surface);
        self.xdg.remove(&surface);
        self.forget_popups_of(surface);
    }

    /// A layer surface takes a popup made with no parent as its own.
    pub(crate) fn adopt_popup(&mut self, popup: SurfaceId, layer: SurfaceId) {
        if let Some(xdg) = self.xdg.get_mut(&popup)
            && let XdgRole::Popup { parent, .. } = &mut xdg.role
            && parent.is_none()
        {
            *parent = Some(layer);
        }
    }

    /// The popups whose parent is gone are told they are done.
    pub(crate) fn forget_popups_of(&mut self, parent_surface: SurfaceId) {
        let children: Vec<SurfaceId> = self
            .popups
            .iter()
            .copied()
            .filter(|popup| {
                matches!(self.xdg.get(popup).map(|xdg| xdg.role),
                    Some(XdgRole::Popup { parent: Some(parent), .. }) if parent == parent_surface)
            })
            .collect();
        for child in children {
            let xdg = &self.xdg[&child];
            let (client, object) = (xdg.client, Self::popup_object(xdg));
            if let Some(client) = self.clients.get_mut(&client) {
                client.send(object, xdg_popup::Event::PopupDone);
            }
            self.unmap_xdg(child);
        }
    }

    /// What each window is, bottom to top: for the tests, and for the shell
    /// once it can ask.
    pub fn window_requests(&self) -> Vec<ToplevelRequests> {
        self.windows
            .iter()
            .filter_map(|window| self.xdg.get(&window.surface))
            .map(|xdg| xdg.toplevel.clone())
            .collect()
    }
}
