//! Surfaces: `wl_compositor`, `wl_surface`, `wl_region`, `wl_subcompositor`,
//! and `wl_subsurface`.
//!
//! A surface's state is double-buffered: requests change its pending state,
//! and `commit` makes it current all at once. A synchronized subsurface's
//! commit is held, and applied when its parent's state is. A subsurface's
//! position and stacking are its parent's state, and change on the parent's
//! commit. This is the model Smithay's `wayland::compositor` and Weston
//! follow.
//!
//! When a buffer is committed, its pixels are copied and the buffer is
//! released at once (see `shm.rs`).

use std::borrow::Cow;

use fulcrum_wire::{
    ObjectId,
    protocol::{
        viewporter::wp_viewport,
        wayland::{wl_callback, wl_compositor, wl_region, wl_subcompositor, wl_subsurface, wl_surface},
    },
};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    geometry::{Rect, Region},
    image::Image,
    shm::BufferId,
};

pub(crate) type SurfaceId = u64;
pub(crate) type RegionId = u64;

/// How deep subsurfaces may nest.
pub const MAX_SUBSURFACE_DEPTH: usize = 32;

/// How many damage rectangles one commit may carry before they are merged
/// into one.
const MAX_DAMAGE: usize = 64;

/// What a surface is for. A surface has at most one role in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    None,
    Subsurface,
    Toplevel,
    Popup,
    Layer,
}

/// Requests since the last commit.
#[derive(Debug, Default)]
pub(crate) struct Pending {
    /// `Some(None)` attaches no buffer, which unmaps the surface.
    buffer: Option<Option<BufferId>>,
    offset: Option<(i32, i32)>,
    damage: Vec<Rect>,
    frames: Vec<u32>,
    opaque: Option<Option<Region>>,
    input: Option<Option<Region>>,
    transform: Option<i32>,
    scale: Option<i32>,
    /// `wp_presentation.feedback` objects for this commit.
    pub(crate) presentation: Vec<u32>,
    /// `wp_viewport.set_source`: `Some(None)` unsets it.
    pub(crate) source: Option<Option<Crop>>,
    /// `wp_viewport.set_destination`: `Some(None)` unsets it.
    pub(crate) destination: Option<Option<(i32, i32)>>,
}

/// The part of a buffer a viewport shows, in surface coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Crop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Pending {
    /// Newer state over older, as when a held commit is followed by another.
    fn over(self, mut older: Pending) -> Pending {
        if self.buffer.is_some() {
            older.buffer = self.buffer;
        }
        if self.offset.is_some() {
            older.offset = self.offset;
        }
        older.damage.extend(self.damage);
        older.frames.extend(self.frames);
        older.presentation.extend(self.presentation);
        if self.opaque.is_some() {
            older.opaque = self.opaque;
        }
        if self.input.is_some() {
            older.input = self.input;
        }
        older.transform = self.transform.or(older.transform);
        older.scale = self.scale.or(older.scale);
        older.source = self.source.or(older.source);
        older.destination = self.destination.or(older.destination);
        older
    }

    fn damage(&mut self, rect: Rect) {
        if self.damage.len() >= MAX_DAMAGE {
            let all = self.damage.iter().fold(rect, |all, rect| all.union(rect));
            self.damage = vec![all];
        } else {
            self.damage.push(rect);
        }
    }
}

/// The state the compositor shows.
#[derive(Debug)]
pub(crate) struct Current {
    /// The committed buffer's pixels. None means the surface is unmapped.
    pub content: Option<Image>,
    /// Frame callbacks waiting for the surface to be drawn.
    pub frames: Vec<u32>,
    /// Presentation feedback waiting for the surface to be drawn.
    pub(crate) presentation: Vec<u32>,
    pub opaque: Option<Region>,
    pub input: Option<Region>,
    pub transform: i32,
    pub scale: i32,
    /// The last `wl_surface.offset`, or the offset given to `attach`. A role
    /// reads it.
    pub offset: (i32, i32),
    /// The viewport's crop and the size it scales to.
    pub(crate) source: Option<Crop>,
    pub(crate) destination: Option<(i32, i32)>,
    /// Counts the times `content` was replaced, so a renderer knows when
    /// what it uploaded is stale.
    pub(crate) content_serial: u64,
}

impl Default for Current {
    fn default() -> Self {
        Current {
            content: None,
            frames: Vec::new(),
            presentation: Vec::new(),
            opaque: None,
            input: None,
            transform: 0,
            scale: 1,
            offset: (0, 0),
            source: None,
            destination: None,
            content_serial: 0,
        }
    }
}

impl Current {
    /// The size in surface coordinates: the viewport's destination, or the
    /// size of its crop, or else the buffer's over its scale.
    pub fn size(&self) -> (i32, i32) {
        let Some(image) = &self.content else {
            return (0, 0);
        };
        match (self.destination, self.source) {
            (Some(size), _) => size,
            (None, Some(crop)) => (crop.width as i32, crop.height as i32),
            (None, None) => (image.width / self.scale, image.height / self.scale),
        }
    }

    /// The part of the buffer that is shown, in the buffer's pixels: the
    /// viewport's crop at the buffer's scale, or all of it.
    pub(crate) fn source_in_buffer(&self) -> (f32, f32, f32, f32) {
        let Some(image) = &self.content else {
            return (0.0, 0.0, 0.0, 0.0);
        };
        match self.source {
            Some(crop) => {
                let scale = self.scale as f32;
                (
                    crop.x as f32 * scale,
                    crop.y as f32 * scale,
                    crop.width as f32 * scale,
                    crop.height as f32 * scale,
                )
            }
            None => (0.0, 0.0, image.width as f32, image.height as f32),
        }
    }

    /// The pixels as they are shown: cropped and scaled by the viewport and
    /// the buffer scale, the size of [`Current::size`].
    pub(crate) fn shown(&self) -> Option<Cow<'_, Image>> {
        let image = self.content.as_ref()?;
        if self.scale == 1 && self.source.is_none() && self.destination.is_none() {
            return Some(Cow::Borrowed(image));
        }
        let scale = f64::from(self.scale);
        let crop = self.source.unwrap_or(Crop {
            x: 0.0,
            y: 0.0,
            width: f64::from(image.width) / scale,
            height: f64::from(image.height) / scale,
        });
        let (width, height) = self.size();
        let mut out = Image::new(width, height);
        for y in 0..height {
            // Each pixel takes the buffer's pixel at its top-left corner.
            let from_y = ((crop.y + f64::from(y) * crop.height / f64::from(height)) * scale) as i32;
            for x in 0..width {
                let from_x = ((crop.x + f64::from(x) * crop.width / f64::from(width)) * scale) as i32;
                out.pixels[(y * width + x) as usize] = image.pixel(from_x, from_y).unwrap_or(0);
            }
        }
        Some(Cow::Owned(out))
    }
}

pub(crate) struct Surface {
    pub client: ClientId,
    pub object: u32,
    pub pending: Pending,
    /// What a synchronized subsurface committed, held for its parent.
    cached: Option<Pending>,
    pub current: Current,
    pub role: Role,
    pub parent: Option<SurfaceId>,
    /// This surface and its subsurfaces, bottom to top, as committed.
    pub stack: Vec<SurfaceId>,
    /// The same, as changed since the last commit.
    pending_stack: Option<Vec<SurfaceId>>,
    /// As a subsurface: where it is relative to its parent, as committed.
    pub position: (i32, i32),
    pending_position: Option<(i32, i32)>,
    /// As a subsurface: whether its commits wait for its parent's.
    pub sync: bool,
    /// How many commits have been applied: for tests, and for redraws.
    pub commits: u64,
    /// The outputs it is on, as its client was last told.
    pub entered: Vec<u64>,
    /// Its `wp_viewport`, if it has one.
    pub viewport: Option<u32>,
    /// Its `wp_fractional_scale_v1`, if it has one.
    pub fractional_scale: Option<u32>,
}

impl State {
    fn next_surface_resource(&mut self) -> u64 {
        self.next_resource += 1;
        self.next_resource
    }

    pub(crate) fn compositor_request(
        &mut self,
        client_id: ClientId,
        version: u32,
        request: wl_compositor::Request,
    ) -> Result<(), ProtocolError> {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                let surface = self.next_surface_resource();
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(wl_surface::NAME),
                    Some(&wl_surface::INFO),
                    version,
                    ObjectKind::Surface(surface),
                )?;
                self.surfaces.insert(
                    surface,
                    Surface {
                        client: client_id,
                        object: id.0,
                        pending: Pending::default(),
                        cached: None,
                        current: Current::default(),
                        role: Role::None,
                        parent: None,
                        stack: vec![surface],
                        pending_stack: None,
                        position: (0, 0),
                        pending_position: None,
                        sync: true,
                        commits: 0,
                        entered: Vec::new(),
                        viewport: None,
                        fractional_scale: None,
                    },
                );
                Ok(())
            }
            wl_compositor::Request::CreateRegion { id } => {
                let region = self.next_surface_resource();
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(wl_region::NAME),
                    Some(&wl_region::INFO),
                    version,
                    ObjectKind::Region(region),
                )?;
                self.regions.insert(region, Region::default());
                Ok(())
            }
            // Version 7, which is not offered; the dispatch refuses it first.
            wl_compositor::Request::Release => Ok(()),
        }
    }

    pub(crate) fn region_request(
        &mut self,
        object: u32,
        region: RegionId,
        request: wl_region::Request,
    ) -> Result<(), ProtocolError> {
        let (add, x, y, width, height) = match request {
            wl_region::Request::Destroy => {
                self.regions.remove(&region);
                return Ok(());
            }
            wl_region::Request::Add { x, y, width, height } => (true, x, y, width, height),
            wl_region::Request::Subtract { x, y, width, height } => (false, x, y, width, height),
        };
        let region = self
            .regions
            .get_mut(&region)
            .expect("a region lives as long as its object");
        if !region.apply(add, Rect::new(x, y, width, height)) {
            return Err(ProtocolError {
                object,
                code: fulcrum_wire::protocol::wayland::wl_display::error::NO_MEMORY,
                message: "a region built from too many rectangles".into(),
            });
        }
        Ok(())
    }

    /// The region an object names, copied, as `set_opaque_region` and
    /// `set_input_region` take it.
    fn region_of(&self, client_id: ClientId, object: Option<ObjectId>) -> Option<Region> {
        let object = object?;
        match self.clients.get(&client_id)?.objects.get(&object.0)?.kind {
            ObjectKind::Region(region) => self.regions.get(&region).cloned(),
            _ => None,
        }
    }

    fn buffer_of(&self, client_id: ClientId, object: Option<ObjectId>) -> Option<BufferId> {
        let object = object?;
        match self.clients.get(&client_id)?.objects.get(&object.0)?.kind {
            ObjectKind::Buffer(buffer) => Some(buffer),
            _ => None,
        }
    }

    pub(crate) fn surface_of(&self, client_id: ClientId, object: ObjectId) -> Option<SurfaceId> {
        match self.clients.get(&client_id)?.objects.get(&object.0)?.kind {
            ObjectKind::Surface(surface) => Some(surface),
            _ => None,
        }
    }

    pub(crate) fn surface_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        version: u32,
        id: SurfaceId,
        request: wl_surface::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |code, message: &str| ProtocolError {
            object,
            code,
            message: message.to_string(),
        };
        match request {
            wl_surface::Request::Destroy => {
                self.destroy_surface(id, true);
                return Ok(());
            }
            wl_surface::Request::Attach { buffer, x, y } => {
                if version >= 5 && (x, y) != (0, 0) {
                    return Err(fail(
                        wl_surface::error::INVALID_OFFSET,
                        "attach with a non-zero offset; use wl_surface.offset",
                    ));
                }
                let buffer = self.buffer_of(client_id, buffer);
                let surface = self.surface_mut(id);
                surface.pending.buffer = Some(buffer);
                if version < 5 {
                    surface.pending.offset = Some((x, y));
                }
            }
            wl_surface::Request::Damage { x, y, width, height }
            | wl_surface::Request::DamageBuffer { x, y, width, height } => {
                self.surface_mut(id).pending.damage(Rect::new(x, y, width, height));
            }
            wl_surface::Request::Frame { callback } => {
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    callback,
                    Cow::Borrowed(wl_callback::NAME),
                    Some(&wl_callback::INFO),
                    1,
                    ObjectKind::Callback,
                )?;
                self.surface_mut(id).pending.frames.push(callback.0);
            }
            wl_surface::Request::SetOpaqueRegion { region } => {
                let region = self.region_of(client_id, region);
                self.surface_mut(id).pending.opaque = Some(region);
            }
            wl_surface::Request::SetInputRegion { region } => {
                let region = self.region_of(client_id, region);
                self.surface_mut(id).pending.input = Some(region);
            }
            wl_surface::Request::SetBufferTransform { transform } => {
                if !(0..=7).contains(&transform) {
                    return Err(fail(wl_surface::error::INVALID_TRANSFORM, "a transform outside 0 to 7"));
                }
                self.surface_mut(id).pending.transform = Some(transform);
            }
            wl_surface::Request::SetBufferScale { scale } => {
                if scale < 1 {
                    return Err(fail(wl_surface::error::INVALID_SCALE, "a buffer scale below 1"));
                }
                self.surface_mut(id).pending.scale = Some(scale);
            }
            wl_surface::Request::Offset { x, y } => {
                self.surface_mut(id).pending.offset = Some((x, y));
            }
            wl_surface::Request::Commit => {
                self.commit(id)?;
                let root = self.root_of(id);
                self.xdg_committed(root)?;
                self.layer_committed(root)?;
                // Counted once the role has the commit, so the one that maps
                // a window counts.
                self.count_commit(id);
                self.queue_redraw_for(id);
            }
            wl_surface::Request::GetRelease { .. } => {
                return Err(fail(
                    fulcrum_wire::protocol::wayland::wl_display::error::IMPLEMENTATION,
                    "get_release is not offered",
                ));
            }
        }
        Ok(())
    }

    fn surface_mut(&mut self, id: SurfaceId) -> &mut Surface {
        self.surfaces
            .get_mut(&id)
            .expect("a surface lives as long as its object")
    }

    /// Whether a surface's commits are held for its parent: it is a
    /// synchronized subsurface, or one inside a synchronized subsurface.
    pub(crate) fn is_synchronized(&self, id: SurfaceId) -> bool {
        let mut at = id;
        for _ in 0..=MAX_SUBSURFACE_DEPTH {
            let Some(surface) = self.surfaces.get(&at) else {
                return false;
            };
            let Some(parent) = surface.parent else {
                return false;
            };
            if surface.role == Role::Subsurface && surface.sync {
                return true;
            }
            at = parent;
        }
        false
    }

    fn commit(&mut self, id: SurfaceId) -> Result<(), ProtocolError> {
        let pending = std::mem::take(&mut self.surface_mut(id).pending);
        let held = self.surface_mut(id).cached.take();
        let state = match held {
            Some(held) => pending.over(held),
            None => pending,
        };
        if self.is_synchronized(id) {
            self.surface_mut(id).cached = Some(state);
            return Ok(());
        }
        self.apply(id, state)
    }

    /// Makes committed state current, then the held state of each
    /// synchronized subsurface.
    fn apply(&mut self, id: SurfaceId, state: Pending) -> Result<(), ProtocolError> {
        let surface = self.surfaces.get(&id).expect("an applied surface exists");
        let object = surface.object;
        let scale = state.scale.unwrap_or(surface.current.scale);
        let mut content = None;
        if let Some(buffer) = state.buffer {
            content = Some(match buffer.filter(|buffer| self.buffers.contains_key(buffer)) {
                Some(buffer) => {
                    let (width, height) = self.buffer_size(buffer).unwrap_or_default();
                    if width % scale != 0 || height % scale != 0 {
                        return Err(ProtocolError {
                            object,
                            code: wl_surface::error::INVALID_SIZE,
                            message: format!("a {width}x{height} buffer is not a multiple of its scale {scale}"),
                        });
                    }
                    // The copy is budgeted before it is made.
                    let held = &self.surfaces[&id];
                    let replaced = pixel_bytes(held.current.content.as_ref());
                    let bytes = (width.max(0) as usize)
                        .saturating_mul(height.max(0) as usize)
                        .saturating_mul(4);
                    self.may_hold_pixels(held.client, replaced, bytes)
                        .map_err(|over| ProtocolError {
                            object: 1,
                            code: fulcrum_wire::protocol::wayland::wl_display::error::NO_MEMORY,
                            message: format!("wl_surface#{object} cannot show its buffer: {over}"),
                        })?;
                    Some(self.import_buffer(buffer)?)
                }
                None => None,
            });
        }
        let surface = self.surfaces.get(&id).expect("an applied surface exists");
        let source = state.source.unwrap_or(surface.current.source);
        let destination = state.destination.unwrap_or(surface.current.destination);
        let shown = match &content {
            Some(content) => content.as_ref(),
            None => surface.current.content.as_ref(),
        };
        if let (Some(image), Some(crop)) = (shown, source) {
            let viewport = surface.viewport.unwrap_or(object);
            let (width, height) = (
                f64::from(image.width) / f64::from(scale),
                f64::from(image.height) / f64::from(scale),
            );
            if crop.x + crop.width > width || crop.y + crop.height > height {
                return Err(ProtocolError {
                    object: viewport,
                    code: wp_viewport::error::OUT_OF_BUFFER,
                    message: format!(
                        "the source {}x{} at {},{} is outside the {width}x{height} buffer",
                        crop.width, crop.height, crop.x, crop.y
                    ),
                });
            }
            if destination.is_none() && (crop.width.fract() != 0.0 || crop.height.fract() != 0.0) {
                return Err(ProtocolError {
                    object: viewport,
                    code: wp_viewport::error::BAD_SIZE,
                    message: format!(
                        "a source of {}x{} needs a destination size, as it is not whole",
                        crop.width, crop.height
                    ),
                });
            }
        }
        let surface = self.surface_mut(id);
        surface.current.source = source;
        surface.current.destination = destination;
        if let Some(content) = content {
            let (replaced, bytes) = (
                pixel_bytes(surface.current.content.as_ref()),
                pixel_bytes(content.as_ref()),
            );
            surface.current.content = content;
            surface.current.content_serial += 1;
            let client_id = surface.client;
            if let Some(client) = self.clients.get_mut(&client_id) {
                client.pixel_bytes = client.pixel_bytes.saturating_sub(replaced) + bytes;
            }
        }
        let surface = self.surface_mut(id);
        if let Some(offset) = state.offset {
            surface.current.offset = offset;
        }
        surface.current.frames.extend(state.frames);
        // Feedback for content not yet drawn is superseded by this commit's.
        let superseded = std::mem::replace(&mut surface.current.presentation, state.presentation);
        let feedback_client = surface.client;
        if let Some(opaque) = state.opaque {
            surface.current.opaque = opaque;
        }
        if let Some(input) = state.input {
            surface.current.input = input;
        }
        surface.current.transform = state.transform.unwrap_or(surface.current.transform);
        surface.current.scale = scale;
        surface.commits += 1;
        if !superseded.is_empty() {
            self.discard_feedback(feedback_client, superseded);
        }
        let surface = self.surface_mut(id);
        if let Some(stack) = surface.pending_stack.take() {
            surface.stack = stack;
        }
        let children: Vec<SurfaceId> = surface.stack.iter().copied().filter(|child| *child != id).collect();
        for child in children {
            let Some(child_surface) = self.surfaces.get_mut(&child) else {
                continue;
            };
            if let Some(position) = child_surface.pending_position.take() {
                child_surface.position = position;
            }
            if self.is_synchronized(child)
                && let Some(held) = self.surfaces.get_mut(&child).and_then(|child| child.cached.take())
            {
                self.apply(child, held)?;
            }
        }
        Ok(())
    }

    pub(crate) fn subcompositor_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        request: wl_subcompositor::Request,
    ) -> Result<(), ProtocolError> {
        let wl_subcompositor::Request::GetSubsurface { id, surface, parent } = request else {
            return Ok(());
        };
        let fail = |code, message: String| ProtocolError { object, code, message };
        let child = self
            .surface_of(client_id, surface)
            .expect("the dispatch checked the argument's interface");
        let parent = self
            .surface_of(client_id, parent)
            .expect("the dispatch checked the argument's interface");
        if self.surfaces[&child].role != Role::None {
            return Err(fail(
                wl_subcompositor::error::BAD_SURFACE,
                format!("wl_surface#{} already has a role", surface.0),
            ));
        }
        // The parent may not be the surface, nor inside its tree.
        let mut depth = 0;
        let mut at = Some(parent);
        while let Some(ancestor) = at {
            if ancestor == child {
                return Err(fail(
                    wl_subcompositor::error::BAD_PARENT,
                    format!("wl_surface#{} is its own ancestor", surface.0),
                ));
            }
            depth += 1;
            at = self.surfaces.get(&ancestor).and_then(|surface| surface.parent);
        }
        if depth >= MAX_SUBSURFACE_DEPTH {
            return Err(fail(
                wl_subcompositor::error::BAD_PARENT,
                format!("subsurfaces nested deeper than {MAX_SUBSURFACE_DEPTH}"),
            ));
        }
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        client.create(
            id,
            Cow::Borrowed(wl_subsurface::NAME),
            Some(&wl_subsurface::INFO),
            1,
            ObjectKind::Subsurface(child),
        )?;
        let child_surface = self.surface_mut(child);
        child_surface.role = Role::Subsurface;
        child_surface.parent = Some(parent);
        child_surface.sync = true;
        child_surface.position = (0, 0);
        // A new subsurface is the top of its parent's stack, at once.
        let parent_surface = self.surface_mut(parent);
        parent_surface.stack.push(child);
        if let Some(pending) = &mut parent_surface.pending_stack {
            pending.push(child);
        }
        Ok(())
    }

    pub(crate) fn subsurface_request(
        &mut self,
        object: u32,
        id: SurfaceId,
        request: wl_subsurface::Request,
        client_id: ClientId,
    ) -> Result<(), ProtocolError> {
        let Some(parent) = self.surfaces.get(&id).and_then(|surface| surface.parent) else {
            // A subsurface whose parent is gone is inert.
            if matches!(request, wl_subsurface::Request::Destroy) {
                self.unparent(id);
            }
            return Ok(());
        };
        match request {
            wl_subsurface::Request::Destroy => self.unparent(id),
            wl_subsurface::Request::SetPosition { x, y } => {
                self.surface_mut(id).pending_position = Some((x, y));
            }
            wl_subsurface::Request::PlaceAbove { sibling } | wl_subsurface::Request::PlaceBelow { sibling } => {
                let above = matches!(request, wl_subsurface::Request::PlaceAbove { .. });
                let sibling_id = self.surface_of(client_id, sibling).expect("the dispatch checked it");
                let parent_surface = self
                    .surfaces
                    .get(&parent)
                    .expect("a parent exists while its child names it");
                let mut stack = parent_surface
                    .pending_stack
                    .clone()
                    .unwrap_or_else(|| parent_surface.stack.clone());
                if sibling_id == id || !stack.contains(&sibling_id) {
                    return Err(ProtocolError {
                        object,
                        code: wl_subsurface::error::BAD_SURFACE,
                        message: format!("wl_surface#{} is not a sibling or the parent", sibling.0),
                    });
                }
                stack.retain(|surface| *surface != id);
                let at = stack
                    .iter()
                    .position(|surface| *surface == sibling_id)
                    .expect("checked above");
                stack.insert(if above { at + 1 } else { at }, id);
                self.surface_mut(parent).pending_stack = Some(stack);
            }
            wl_subsurface::Request::SetSync => self.surface_mut(id).sync = true,
            wl_subsurface::Request::SetDesync => {
                self.surface_mut(id).sync = false;
                if !self.is_synchronized(id)
                    && let Some(held) = self.surface_mut(id).cached.take()
                {
                    self.apply(id, held)?;
                }
            }
        }
        Ok(())
    }

    /// The subsurface object is gone: the surface leaves its parent, and
    /// may be given a role again.
    fn unparent(&mut self, id: SurfaceId) {
        let Some(surface) = self.surfaces.get_mut(&id) else {
            return;
        };
        let parent = surface.parent.take();
        surface.role = Role::None;
        surface.cached = None;
        surface.pending_position = None;
        if let Some(parent) = parent.and_then(|parent| self.surfaces.get_mut(&parent)) {
            parent.stack.retain(|surface| *surface != id);
            if let Some(pending) = &mut parent.pending_stack {
                pending.retain(|surface| *surface != id);
            }
        }
    }

    /// A surface is destroyed, by its client or with it. Its frame
    /// callbacks are destroyed too; `tell` says whether its client is still
    /// there to hear of it.
    pub(crate) fn destroy_surface(&mut self, id: SurfaceId, tell: bool) {
        let outputs = self
            .surfaces
            .get(&self.root_of(id))
            .map(|root| root.entered.clone())
            .unwrap_or_default();
        self.forget_xdg(id);
        self.forget_layer(id);
        self.drop_focus_from(id);
        self.unparent(id);
        for output in outputs {
            self.queue_redraw(output);
        }
        let Some(surface) = self.surfaces.remove(&id) else {
            return;
        };
        if let Some(client) = self.clients.get_mut(&surface.client) {
            client.pixel_bytes = client
                .pixel_bytes
                .saturating_sub(pixel_bytes(surface.current.content.as_ref()));
        }
        if tell {
            // Content of a surface that is gone will not be shown.
            let feedback: Vec<u32> = surface
                .current
                .presentation
                .iter()
                .chain(&surface.pending.presentation)
                .chain(surface.cached.iter().flat_map(|cached| &cached.presentation))
                .copied()
                .collect();
            self.discard_feedback(surface.client, feedback);
        }
        for child in surface.stack.iter().filter(|child| **child != id) {
            if let Some(child) = self.surfaces.get_mut(child) {
                child.parent = None;
            }
        }
        if tell && let Some(client) = self.clients.get_mut(&surface.client) {
            let callbacks = surface
                .current
                .frames
                .iter()
                .chain(&surface.pending.frames)
                .chain(surface.cached.iter().flat_map(|cached| &cached.frames));
            for callback in callbacks {
                client.destroy(*callback);
            }
        }
    }

    /// Draws a surface and its subsurfaces with the CPU renderer, into an
    /// image the size of what they cover. None if the surface shows nothing.
    pub(crate) fn render_tree(&self, id: SurfaceId) -> Option<Image> {
        let bounds = self.tree_bounds(id, (0, 0), 0)?;
        let mut image = Image::new(bounds.width, bounds.height);
        self.draw_tree(id, (-bounds.x, -bounds.y), &mut image, 0);
        Some(image)
    }

    /// What a surface's tree covers, with its root surface's corner at `at`.
    pub(crate) fn tree_bounds_at(&self, id: SurfaceId, at: (i32, i32)) -> Option<Rect> {
        self.tree_bounds(id, at, 0)
    }

    fn tree_bounds(&self, id: SurfaceId, at: (i32, i32), depth: usize) -> Option<Rect> {
        let surface = self.surfaces.get(&id)?;
        surface.current.content.as_ref()?;
        let (width, height) = surface.current.size();
        let mut bounds = Rect::new(at.0, at.1, width, height);
        if depth < MAX_SUBSURFACE_DEPTH {
            for child in surface.stack.iter().filter(|child| **child != id) {
                let Some(child_surface) = self.surfaces.get(child) else {
                    continue;
                };
                let origin = (at.0 + child_surface.position.0, at.1 + child_surface.position.1);
                if let Some(child_bounds) = self.tree_bounds(*child, origin, depth + 1) {
                    bounds = bounds.union(&child_bounds);
                }
            }
        }
        Some(bounds)
    }

    pub(crate) fn draw_tree(&self, id: SurfaceId, at: (i32, i32), image: &mut Image, depth: usize) {
        let Some(surface) = self.surfaces.get(&id) else {
            return;
        };
        if surface.current.content.is_none() {
            return;
        }
        for member in &surface.stack {
            if *member == id {
                if let Some(shown) = surface.current.shown() {
                    image.draw(&shown, at);
                }
            } else if depth < MAX_SUBSURFACE_DEPTH
                && let Some(child) = self.surfaces.get(member)
            {
                self.draw_tree(
                    *member,
                    (at.0 + child.position.0, at.1 + child.position.1),
                    image,
                    depth + 1,
                );
            }
        }
    }

    /// The images of every surface with no parent that shows something, in
    /// the order the surfaces were made. For the tests, until roles put
    /// surfaces on an output.
    #[doc(hidden)]
    pub fn render_root_surfaces(&self) -> Vec<Image> {
        let mut roots: Vec<SurfaceId> = self
            .surfaces
            .iter()
            .filter(|(_, surface)| surface.parent.is_none())
            .map(|(id, _)| *id)
            .collect();
        roots.sort();
        roots.into_iter().filter_map(|id| self.render_tree(id)).collect()
    }

    /// How many surfaces, regions, pools, and buffers the compositor holds.
    #[doc(hidden)]
    pub fn resource_counts(&self) -> [usize; 4] {
        [
            self.surfaces.len(),
            self.regions.len(),
            self.pools.len(),
            self.buffers.len(),
        ]
    }

    /// How many commits each surface with no parent has had applied, in the
    /// order the surfaces were made.
    #[doc(hidden)]
    pub fn root_surface_commits(&self) -> Vec<u64> {
        let mut roots: Vec<(&SurfaceId, &Surface)> = self
            .surfaces
            .iter()
            .filter(|(_, surface)| surface.parent.is_none())
            .collect();
        roots.sort_by_key(|(id, _)| **id);
        roots.into_iter().map(|(_, surface)| surface.commits).collect()
    }
}

/// The bytes the compositor holds for a surface's pixels.
fn pixel_bytes(content: Option<&Image>) -> usize {
    content.map_or(0, |image| image.pixels.len() * 4)
}
