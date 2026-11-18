//! Explicit sync: `wp_linux_drm_syncobj_manager_v1`. A client of a GPU says
//! which point of a timeline its GPU signals when a buffer is ready to read
//! (the acquire point), and which point the compositor signals when it is
//! done with the buffer (the release point). Drivers without implicit sync,
//! as NVIDIA's, need it.
//!
//! A commit whose acquire point is not signalled yet is held, not waited for:
//! the kernel wakes an eventfd at that point, and the event loop applies the
//! commit then. A later commit is held behind it, and merges over it. A
//! dma-buf the compositor copies at the commit is done with then, and its
//! release point is signalled at once; one the GPU shows uncopied, when other
//! content replaces it (see `dmabuf.rs`).
//!
//! The table offers it to a compartment whose needs include the GPU, and to
//! the shell (ADR 0020, ADR 0023).

use std::{
    borrow::Cow,
    collections::HashMap,
    os::fd::{AsFd, OwnedFd},
    rc::Rc,
};

use fulcrum_sys::syncobj::{self, Timeline};
use fulcrum_wire::protocol::linux_drm_syncobj_v1::{
    wp_linux_drm_syncobj_manager_v1, wp_linux_drm_syncobj_surface_v1, wp_linux_drm_syncobj_timeline_v1,
};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    event_loop::Token,
    globals::GlobalKind,
    log,
    shm::Source,
    surface::{Pending, SurfaceId},
};

/// The `wp_linux_drm_syncobj_manager_v1` version Fulcrum offers.
pub const EXPLICIT_SYNC_VERSION: u32 = 1;

pub(crate) type TimelineId = u64;

/// A point of a timeline.
#[derive(Clone, Debug)]
pub(crate) struct Point {
    pub timeline: Rc<Timeline>,
    pub point: u64,
}

/// A commit held until its acquire point is signalled.
pub(crate) struct Held {
    pub state: Pending,
    /// Readable once the acquire point is signalled.
    pub event: OwnedFd,
}

#[derive(Default)]
pub(crate) struct ExplicitSync {
    /// The DRM device the timelines are imported on.
    device: Option<OwnedFd>,
    pub timelines: HashMap<TimelineId, Rc<Timeline>>,
    next: TimelineId,
    pub held: HashMap<SurfaceId, Held>,
}

impl State {
    /// Offers explicit sync on a DRM device, if the device has timeline
    /// syncobjs. The table still decides who sees it.
    pub fn offer_explicit_sync(&mut self, device: OwnedFd) {
        if !syncobj::supported(device.as_fd()) {
            log::info!("no explicit sync: the DRM device has no timeline syncobjs");
            return;
        }
        self.explicit_sync.device = Some(device);
        self.add_global(
            &wp_linux_drm_syncobj_manager_v1::INFO,
            EXPLICIT_SYNC_VERSION,
            GlobalKind::ExplicitSync,
        );
    }

    pub(crate) fn explicit_sync_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        request: wp_linux_drm_syncobj_manager_v1::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |code, message: String| ProtocolError { object, code, message };
        match request {
            wp_linux_drm_syncobj_manager_v1::Request::Destroy => {}
            wp_linux_drm_syncobj_manager_v1::Request::GetSurface { id, surface } => {
                let surface_id = self
                    .surface_of(client_id, surface)
                    .expect("the dispatch checked the argument's interface");
                if self.surfaces[&surface_id].syncobj.is_some() {
                    return Err(fail(
                        wp_linux_drm_syncobj_manager_v1::error::SURFACE_EXISTS,
                        format!("wl_surface#{} already has a syncobj surface", surface.0),
                    ));
                }
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(wp_linux_drm_syncobj_surface_v1::NAME),
                    Some(&wp_linux_drm_syncobj_surface_v1::INFO),
                    1,
                    ObjectKind::SyncobjSurface(surface_id),
                )?;
                if let Some(state) = self.surfaces.get_mut(&surface_id) {
                    state.syncobj = Some(id.0);
                }
            }
            wp_linux_drm_syncobj_manager_v1::Request::ImportTimeline { id, fd } => {
                let Some(device) = &self.explicit_sync.device else {
                    return Err(fail(
                        wp_linux_drm_syncobj_manager_v1::error::INVALID_TIMELINE,
                        "the compositor has no DRM device".into(),
                    ));
                };
                let timeline = Timeline::import(device.as_fd(), fd.as_fd()).map_err(|error| {
                    fail(
                        wp_linux_drm_syncobj_manager_v1::error::INVALID_TIMELINE,
                        format!("not a timeline: {error}"),
                    )
                })?;
                self.explicit_sync.next += 1;
                let timeline_id = self.explicit_sync.next;
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(wp_linux_drm_syncobj_timeline_v1::NAME),
                    Some(&wp_linux_drm_syncobj_timeline_v1::INFO),
                    1,
                    ObjectKind::SyncobjTimeline(timeline_id),
                )?;
                self.explicit_sync.timelines.insert(timeline_id, Rc::new(timeline));
            }
        }
        Ok(())
    }

    pub(crate) fn syncobj_surface_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        surface_id: SurfaceId,
        request: wp_linux_drm_syncobj_surface_v1::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |code, message: String| ProtocolError { object, code, message };
        let (timeline, point_hi, point_lo, acquire) = match request {
            wp_linux_drm_syncobj_surface_v1::Request::Destroy => {
                if let Some(surface) = self.surfaces.get_mut(&surface_id) {
                    surface.syncobj = None;
                }
                return Ok(());
            }
            wp_linux_drm_syncobj_surface_v1::Request::SetAcquirePoint {
                timeline,
                point_hi,
                point_lo,
            } => (timeline, point_hi, point_lo, true),
            wp_linux_drm_syncobj_surface_v1::Request::SetReleasePoint {
                timeline,
                point_hi,
                point_lo,
            } => (timeline, point_hi, point_lo, false),
        };
        let timeline = match self
            .clients
            .get(&client_id)
            .and_then(|client| client.objects.get(&timeline.0))
        {
            Some(found) => match found.kind {
                ObjectKind::SyncobjTimeline(id) => self.explicit_sync.timelines.get(&id).cloned(),
                _ => None,
            },
            None => None,
        }
        .expect("the dispatch checked the argument's interface");
        let Some(surface) = self.surfaces.get_mut(&surface_id) else {
            return Err(fail(
                wp_linux_drm_syncobj_surface_v1::error::NO_SURFACE,
                "the surface was destroyed".into(),
            ));
        };
        let point = Point {
            timeline,
            point: (u64::from(point_hi) << 32) | u64::from(point_lo),
        };
        if acquire {
            surface.pending.acquire = Some(point);
        } else {
            surface.pending.release = Some(point);
        }
        Ok(())
    }

    /// Checks a commit's points, as the protocol says, on a surface with a
    /// syncobj surface.
    pub(crate) fn check_points(&self, id: SurfaceId, state: &Pending) -> Result<(), ProtocolError> {
        let Some(object) = self.surfaces.get(&id).and_then(|surface| surface.syncobj) else {
            return Ok(());
        };
        let fail = |code, message: &str| ProtocolError {
            object,
            code,
            message: message.into(),
        };
        use wp_linux_drm_syncobj_surface_v1::error;
        match state.attached() {
            Some(Some(buffer)) => {
                if state.acquire.is_none() {
                    return Err(fail(
                        error::NO_ACQUIRE_POINT,
                        "a buffer was committed with no acquire point",
                    ));
                }
                if state.release.is_none() {
                    return Err(fail(
                        error::NO_RELEASE_POINT,
                        "a buffer was committed with no release point",
                    ));
                }
                let dmabuf = self
                    .buffers
                    .get(&buffer)
                    .is_some_and(|buffer| matches!(buffer.source, Source::Dmabuf { .. } | Source::Gpu(_)));
                if !dmabuf {
                    return Err(fail(error::UNSUPPORTED_BUFFER, "explicit sync is for dma-bufs"));
                }
            }
            _ => {
                if state.acquire.is_some() || state.release.is_some() {
                    return Err(fail(error::NO_BUFFER, "points were set with no buffer"));
                }
            }
        }
        if let (Some(acquire), Some(release)) = (&state.acquire, &state.release)
            && Rc::ptr_eq(&acquire.timeline, &release.timeline)
            && release.point <= acquire.point
        {
            return Err(fail(
                error::CONFLICTING_POINTS,
                "the release point is not after the acquire point on its timeline",
            ));
        }
        Ok(())
    }

    /// Holds a commit whose acquire point is not signalled yet, or one that
    /// must wait behind a held commit. False if it may be applied now.
    pub(crate) fn hold_commit(&mut self, id: SurfaceId, state: Pending) -> Result<Option<Pending>, ProtocolError> {
        let held = self.explicit_sync.held.remove(&id);
        let state = match held {
            Some(held) => {
                self.event_loop.remove(held.event.as_fd());
                state.over(held.state)
            }
            None => state,
        };
        let waiting = state.acquire.as_ref().and_then(|acquire| {
            let signalled = acquire.timeline.signalled().unwrap_or(0);
            (signalled < acquire.point).then(|| acquire.clone())
        });
        let Some(acquire) = waiting else {
            return Ok(Some(state));
        };
        match acquire.timeline.eventfd(acquire.point) {
            Ok(event) => {
                if self.event_loop.add(event.as_fd(), Token::Held(id), false).is_err() {
                    return Ok(Some(state));
                }
                self.explicit_sync.held.insert(id, Held { state, event });
                Ok(None)
            }
            // A kernel without the eventfd ioctl: the commit is applied, as
            // implicit sync would have it.
            Err(_) => Ok(Some(state)),
        }
    }

    /// A held commit's acquire point was signalled: it is applied now.
    pub(crate) fn release_held(&mut self, id: SurfaceId) {
        let Some(held) = self.explicit_sync.held.remove(&id) else {
            return;
        };
        self.event_loop.remove(held.event.as_fd());
        let Some(client) = self.surfaces.get(&id).map(|surface| surface.client) else {
            return;
        };
        if let Err(error) = self.apply_commit(id, held.state)
            && let Some(client) = self.clients.get_mut(&client)
        {
            client.post_error(error);
        }
    }

    /// The compositor is done with a buffer: its release point is signalled.
    pub(crate) fn signal_release(release: Option<Point>) {
        if let Some(release) = release
            && let Err(error) = release.timeline.signal(release.point)
        {
            log::warning!("could not signal a release point: {error}");
        }
    }

    /// A surface is gone: a commit it held goes with it, and its buffer is
    /// done with.
    pub(crate) fn forget_held(&mut self, id: SurfaceId) {
        if let Some(mut held) = self.explicit_sync.held.remove(&id) {
            self.event_loop.remove(held.event.as_fd());
            State::signal_release(held.state.release.take());
        }
    }
}
