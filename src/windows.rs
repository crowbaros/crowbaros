//! Windows: where a toplevel goes when it maps, the order they are stacked
//! in, and what each output shows.
//!
//! This is the least that lets a client be seen. A new window opens on the
//! output the pointer is on, inside the room the shell's layers leave, offset
//! a little from the last. It does not take the keyboard. Frames, moving and
//! resizing, workspaces, and the two cases in which a new window takes the
//! keyboard come with the window manager (CrowbarOS issue 78).

use fulcrum_wire::protocol::wayland::wl_surface;

use crate::{
    State,
    geometry::Rect,
    identity::Identity,
    image::Image,
    surface::{Role, SurfaceId},
    xdg::XdgRole,
};

/// How far inside the usable area of an output a new window is put.
pub const NEW_WINDOW_OFFSET: (i32, i32) = (48, 48);

/// The colour behind everything, until the shell's wallpaper covers it.
pub const BACKGROUND: u32 = 0xff1a_1a1a;

pub(crate) struct Window {
    pub surface: SurfaceId,
    /// Where the window geometry's top-left corner is on the desktop.
    pub location: (i32, i32),
}

impl State {
    /// The window geometry of an xdg surface, in its own coordinates: what
    /// the client set, or else the size of what it draws.
    pub(crate) fn window_geometry(&self, surface: SurfaceId) -> Rect {
        if let Some(geometry) = self.xdg.get(&surface).and_then(|xdg| xdg.geometry) {
            return geometry;
        }
        let (width, height) = self
            .surfaces
            .get(&surface)
            .map_or((0, 0), |surface| surface.current.size());
        Rect::new(0, 0, width, height)
    }

    /// Where a window's root surface is drawn on the desktop.
    pub(crate) fn window_origin(&self, window: &Window) -> (i32, i32) {
        let geometry = self.window_geometry(window.surface);
        (window.location.0 - geometry.x, window.location.1 - geometry.y)
    }

    /// The rectangle a window covers on the desktop.
    pub(crate) fn window_rect(&self, window: &Window) -> Rect {
        let geometry = self.window_geometry(window.surface);
        Rect::new(window.location.0, window.location.1, geometry.width, geometry.height)
    }

    /// Where the surface with the given xdg role draws its window geometry's
    /// corner on the desktop: a window's location, or a popup's place next
    /// to its parent.
    pub(crate) fn xdg_location(&self, surface: SurfaceId, depth: usize) -> Option<(i32, i32)> {
        if depth > crate::surface::MAX_SUBSURFACE_DEPTH {
            return None;
        }
        match self.xdg.get(&surface)?.role {
            XdgRole::Toplevel { .. } => self
                .windows
                .iter()
                .find(|window| window.surface == surface)
                .map(|window| window.location),
            XdgRole::Popup { parent, at, .. } => {
                let parent = parent?;
                let corner = match self.layer_of(parent) {
                    Some(layer) => (layer.rect.x, layer.rect.y),
                    None => self.xdg_location(parent, depth + 1)?,
                };
                Some((corner.0 + at.x, corner.1 + at.y))
            }
            XdgRole::None => None,
        }
    }

    /// The toplevel a popup belongs to, through its parents.
    pub(crate) fn toplevel_of(&self, surface: SurfaceId) -> Option<SurfaceId> {
        let mut at = surface;
        for _ in 0..=crate::surface::MAX_SUBSURFACE_DEPTH {
            match self.xdg.get(&at)?.role {
                XdgRole::Toplevel { .. } => return Some(at),
                XdgRole::Popup { parent, .. } => at = parent?,
                XdgRole::None => return None,
            }
        }
        None
    }

    /// The output a new window opens on: the one the pointer is on, or the
    /// first.
    fn output_for_new_window(&self) -> Option<u64> {
        let (x, y) = (self.pointer.0 as i32, self.pointer.1 as i32);
        self.outputs
            .iter()
            .find(|output| output.rect().contains(x, y))
            .or_else(|| self.outputs.first())
            .map(|output| output.number)
    }

    /// A configured xdg surface committed its first buffer.
    pub(crate) fn map_xdg(&mut self, surface: SurfaceId) {
        let role = self.xdg[&surface].role;
        self.xdg.get_mut(&surface).expect("exists").mapped = true;
        if let XdgRole::Toplevel { .. } = role {
            let Some(output) = self.output_for_new_window() else {
                return;
            };
            let area = self.usable_area(output);
            let step = 32 * (self.windows.len() as i32 % 8);
            let location = (area.x + NEW_WINDOW_OFFSET.0 + step, area.y + NEW_WINDOW_OFFSET.1 + step);
            self.windows.push(Window { surface, location });
            let client = self.surfaces[&surface].client;
            crate::log::debug!(
                "window mapped client={client} app_id={} at={},{}",
                self.xdg[&surface].toplevel.app_id,
                location.0,
                location.1
            );
        }
        self.update_outputs_of(surface);
        self.queue_redraw_for(surface);
    }

    /// An xdg surface unmapped or went away: drawn away where it was.
    pub(crate) fn unmap_xdg(&mut self, surface: SurfaceId) {
        let was_mapped = self.xdg.get(&surface).is_some_and(|xdg| xdg.mapped);
        if !was_mapped {
            return;
        }
        let outputs = self.outputs_showing(surface);
        if let Some(xdg) = self.xdg.get_mut(&surface) {
            xdg.mapped = false;
        }
        self.windows.retain(|window| window.surface != surface);
        self.drop_focus_from(surface);
        for output in outputs {
            self.queue_redraw(output);
        }
    }

    /// The desktop rectangle a mapped xdg surface covers.
    pub(crate) fn xdg_rect(&self, surface: SurfaceId) -> Option<Rect> {
        if !self.xdg.get(&surface)?.mapped {
            return None;
        }
        let corner = self.xdg_location(surface, 0)?;
        let geometry = self.window_geometry(surface);
        Some(Rect::new(
            corner.0,
            corner.1,
            geometry.width.max(1),
            geometry.height.max(1),
        ))
    }

    /// The outputs a mapped xdg surface is on, and every surface of its
    /// tree draws on, in output order.
    pub(crate) fn outputs_showing(&self, surface: SurfaceId) -> Vec<u64> {
        let Some(rect) = self.xdg_rect(surface).or_else(|| self.layer_rect(surface)) else {
            return Vec::new();
        };
        let bounds = self.tree_rect(surface, rect);
        self.outputs
            .iter()
            .filter(|output| intersects(&output.rect(), &bounds))
            .map(|output| output.number)
            .collect()
    }

    /// What a surface's whole tree covers, given where its window geometry
    /// is.
    fn tree_rect(&self, surface: SurfaceId, rect: Rect) -> Rect {
        let geometry = self.window_geometry(surface);
        let origin = (rect.x - geometry.x, rect.y - geometry.y);
        self.tree_bounds_at(surface, origin).unwrap_or(rect).union(&rect)
    }

    /// Tells a surface's client which outputs the surface is on now.
    pub(crate) fn update_outputs_of(&mut self, surface: SurfaceId) {
        let now = self.outputs_showing(surface);
        let Some(entered) = self.surfaces.get(&surface).map(|surface| surface.entered.clone()) else {
            return;
        };
        let client_id = self.surfaces[&surface].client;
        let object = self.surfaces[&surface].object;
        if let Some(client) = self.clients.get_mut(&client_id) {
            for output in &now {
                if !entered.contains(output) {
                    for bound in client.outputs_bound(*output) {
                        client.send(
                            object,
                            wl_surface::Event::Enter {
                                output: fulcrum_wire::ObjectId(bound),
                            },
                        );
                    }
                }
            }
            for output in &entered {
                if !now.contains(output) {
                    for bound in client.outputs_bound(*output) {
                        client.send(
                            object,
                            wl_surface::Event::Leave {
                                output: fulcrum_wire::ObjectId(bound),
                            },
                        );
                    }
                }
            }
        }
        if let Some(surface) = self.surfaces.get_mut(&surface) {
            surface.entered = now;
        }
    }

    /// Who owns each window, bottom to top.
    pub fn window_owners(&self) -> Vec<Identity> {
        self.windows
            .iter()
            .filter_map(|window| {
                let client = self.surfaces.get(&window.surface)?.client;
                Some(self.clients.get(&client)?.identity.clone())
            })
            .collect()
    }

    /// Where each window is on the desktop, bottom to top.
    pub fn window_rects(&self) -> Vec<Rect> {
        self.windows.iter().map(|window| self.window_rect(window)).collect()
    }

    /// Draws everything an output shows with the CPU renderer: the
    /// background, the shell's lower layers, the windows bottom to top with
    /// their popups, and the shell's upper layers.
    pub fn render_output(&self, number: u64) -> Option<Image> {
        let scene = self.scene(number)?;
        Some(self.draw_scene(&scene))
    }

    /// The layer surface a popup belongs to, through its parents.
    fn layer_of_popup(&self, surface: SurfaceId) -> Option<SurfaceId> {
        let mut at = surface;
        for _ in 0..=crate::surface::MAX_SUBSURFACE_DEPTH {
            if self.layer_of(at).is_some() {
                return Some(at);
            }
            match self.xdg.get(&at)?.role {
                XdgRole::Popup { parent, .. } => at = parent?,
                _ => return None,
            }
        }
        None
    }

    /// The mapped popups of the layer surfaces on an output, drawn above
    /// every layer.
    pub(crate) fn layer_popups(&self, output: u64) -> Vec<SurfaceId> {
        self.popups
            .iter()
            .copied()
            .filter(|popup| {
                self.xdg[popup].mapped
                    && self
                        .layer_of_popup(*popup)
                        .and_then(|layer| self.layer_of(layer))
                        .is_some_and(|layer| layer.output == output)
            })
            .collect()
    }

    /// Every surface of a tree that has frame callbacks waiting, with the
    /// tree's popups.
    pub(crate) fn surfaces_of_window(&self, root: SurfaceId) -> Vec<SurfaceId> {
        let mut found = Vec::new();
        self.collect_tree(root, &mut found, 0);
        for popup in &self.popups {
            if self.toplevel_of(*popup) == Some(root) || self.layer_of_popup(*popup) == Some(root) {
                self.collect_tree(*popup, &mut found, 0);
            }
        }
        found
    }

    fn collect_tree(&self, id: SurfaceId, found: &mut Vec<SurfaceId>, depth: usize) {
        let Some(surface) = self.surfaces.get(&id) else {
            return;
        };
        found.push(id);
        if depth < crate::surface::MAX_SUBSURFACE_DEPTH {
            for child in surface.stack.iter().filter(|child| **child != id) {
                self.collect_tree(*child, found, depth + 1);
            }
        }
    }

    /// The surface at the top of a surface's subsurface tree.
    pub(crate) fn root_of(&self, surface: SurfaceId) -> SurfaceId {
        let mut at = surface;
        for _ in 0..=crate::surface::MAX_SUBSURFACE_DEPTH {
            match self.surfaces.get(&at).and_then(|surface| surface.parent) {
                Some(parent)
                    if self
                        .surfaces
                        .get(&at)
                        .is_some_and(|surface| surface.role == Role::Subsurface) =>
                {
                    at = parent
                }
                _ => break,
            }
        }
        at
    }
}

pub(crate) fn intersects(a: &Rect, b: &Rect) -> bool {
    !a.is_empty()
        && !b.is_empty()
        && i64::from(a.x) < i64::from(b.x) + i64::from(b.width)
        && i64::from(b.x) < i64::from(a.x) + i64::from(a.width)
        && i64::from(a.y) < i64::from(b.y) + i64::from(b.height)
        && i64::from(b.y) < i64::from(a.y) + i64::from(a.height)
}
