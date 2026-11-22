//! Explicit sync: a commit waits for its acquire point, and the compositor
//! signals its release point when it is done with the buffer.
//!
//! These need a render node with timeline syncobjs, which CI's machines
//! have not. Where there is none, each test says so and passes. A unit test
//! in `src/state.rs` covers, with no device, that a destroyed timeline is
//! let go.

mod common;

use std::os::fd::{AsFd, OwnedFd};

use common::{Compositor, OUTPUT_SIZE, own_uid};
use crowbar_probe::{Client, Toplevel};
use fulcrum_sys::syncobj::{self, Timeline};
use fulcrum_wire::{
    ObjectId,
    protocol::{
        linux_dmabuf_v1::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1},
        linux_drm_syncobj_v1::{
            wp_linux_drm_syncobj_manager_v1, wp_linux_drm_syncobj_surface_v1, wp_linux_drm_syncobj_timeline_v1,
        },
        wayland::wl_surface,
    },
};

const RED: u32 = 0xffc0_2020;
const BLUE: u32 = 0xff20_20c0;

fn render_node() -> Option<OwnedFd> {
    let path = std::fs::read_dir("/dev/dri")
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("renderD"))
        })?;
    let fd: OwnedFd = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .ok()?
        .into();
    syncobj::supported(fd.as_fd()).then_some(fd)
}

/// A compositor that offers explicit sync on this machine's render node.
fn compositor() -> Option<(Compositor, OwnedFd)> {
    let Some(node) = render_node() else {
        eprintln!("no render node with timeline syncobjs: skipped");
        return None;
    };
    let offered = node.try_clone().unwrap();
    let compositor = Compositor::start_with(own_uid(), own_uid(), move |state| {
        state.add_output("TEST-1", OUTPUT_SIZE, 60_000);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
        state.offer_explicit_sync(offered);
    });
    Some((compositor, node))
}

/// A one-colour "dma-buf" of 64x48 as a memfd, and its buffer.
fn dmabuf_buffer(client: &mut Client, argb: u32) -> u32 {
    let fd = rustix::fs::memfd_create("test-dmabuf", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    let bytes: Vec<u8> = std::iter::repeat_n(argb.to_le_bytes(), 64 * 48).flatten().collect();
    rustix::io::write(&fd, &bytes).unwrap();
    let dmabuf = client.bind(zwp_linux_dmabuf_v1::NAME, 4).unwrap();
    let params = client.new_id(zwp_linux_buffer_params_v1::NAME);
    client
        .request(dmabuf, zwp_linux_dmabuf_v1::Request::CreateParams { params_id: params })
        .unwrap();
    client
        .request(
            params.0,
            zwp_linux_buffer_params_v1::Request::Add {
                fd,
                plane_idx: 0,
                offset: 0,
                stride: 64 * 4,
                modifier_hi: 0,
                modifier_lo: 0,
            },
        )
        .unwrap();
    let buffer = client.new_id("wl_buffer");
    client
        .request(
            params.0,
            zwp_linux_buffer_params_v1::Request::CreateImmed {
                buffer_id: buffer,
                width: 64,
                height: 48,
                format: u32::from_le_bytes(*b"XR24"),
                flags: 0,
            },
        )
        .unwrap();
    buffer.0
}

struct Synced {
    client: Client,
    window: Toplevel,
    syncobj_surface: u32,
    /// The client's timeline object, and the test's own handle on it.
    timeline: u32,
    handle: Timeline,
}

fn synced(compositor: &Compositor, node: &OwnedFd) -> Synced {
    let mut client = compositor.app();
    let window = client.open_toplevel("app", "test.app", (64, 48), RED).unwrap();
    let manager = client.bind(wp_linux_drm_syncobj_manager_v1::NAME, 1).unwrap();
    let exported = syncobj::create_exported(node.as_fd()).unwrap();
    let handle = Timeline::import(node.as_fd(), exported.as_fd()).unwrap();
    let timeline = client.new_id(wp_linux_drm_syncobj_timeline_v1::NAME);
    client
        .request(
            manager,
            wp_linux_drm_syncobj_manager_v1::Request::ImportTimeline {
                id: timeline,
                fd: exported,
            },
        )
        .unwrap();
    let syncobj_surface = client.new_id(wp_linux_drm_syncobj_surface_v1::NAME);
    client
        .request(
            manager,
            wp_linux_drm_syncobj_manager_v1::Request::GetSurface {
                id: syncobj_surface,
                surface: ObjectId(window.surface),
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    Synced {
        client,
        window,
        syncobj_surface: syncobj_surface.0,
        timeline: timeline.0,
        handle,
    }
}

fn point(client: &mut Client, syncobj_surface: u32, timeline: u32, acquire: bool, point: u64) {
    let (point_hi, point_lo) = ((point >> 32) as u32, point as u32);
    let request = if acquire {
        wp_linux_drm_syncobj_surface_v1::Request::SetAcquirePoint {
            timeline: ObjectId(timeline),
            point_hi,
            point_lo,
        }
    } else {
        wp_linux_drm_syncobj_surface_v1::Request::SetReleasePoint {
            timeline: ObjectId(timeline),
            point_hi,
            point_lo,
        }
    };
    client.request(syncobj_surface, request).unwrap();
}

fn attach_and_commit(client: &mut Client, surface: u32, buffer: u32) {
    client
        .request(
            surface,
            wl_surface::Request::Attach {
                buffer: Some(ObjectId(buffer)),
                x: 0,
                y: 0,
            },
        )
        .unwrap();
    client.request(surface, wl_surface::Request::Commit).unwrap();
}

#[test]
fn a_commit_waits_for_its_acquire_point_and_its_release_point_is_signalled() {
    let Some((compositor, node)) = compositor() else { return };
    let mut synced = synced(&compositor, &node);
    let buffer = dmabuf_buffer(&mut synced.client, BLUE);
    point(&mut synced.client, synced.syncobj_surface, synced.timeline, true, 1);
    point(&mut synced.client, synced.syncobj_surface, synced.timeline, false, 2);

    attach_and_commit(&mut synced.client, synced.window.surface, buffer);
    synced.client.roundtrip().unwrap();

    assert_eq!(
        compositor.screen(0).pixel(60, 60),
        Some(RED),
        "drawn before its acquire point"
    );
    assert!(synced.handle.signalled().unwrap() < 2);

    synced.handle.signal(1).unwrap();
    synced.client.roundtrip().unwrap();
    compositor.wait_until_still();

    assert_eq!(
        compositor.screen(0).pixel(60, 60),
        Some(BLUE),
        "not drawn after its acquire point"
    );
    assert_eq!(
        synced.handle.signalled().unwrap(),
        2,
        "the release point was not signalled"
    );
}

/// A client makes and destroys timelines whenever it rebuilds a swapchain.
/// The compositor lets each go with its object, and a point already set on
/// a destroyed timeline still counts, as the protocol says.
#[test]
fn a_destroyed_timeline_is_let_go_and_a_point_already_set_on_it_still_counts() {
    let Some((compositor, node)) = compositor() else { return };
    let mut synced = synced(&compositor, &node);
    let manager = synced.client.bind(wp_linux_drm_syncobj_manager_v1::NAME, 1).unwrap();
    for _ in 0..32 {
        let timeline = synced.client.new_id(wp_linux_drm_syncobj_timeline_v1::NAME);
        let fd = syncobj::create_exported(node.as_fd()).unwrap();
        synced
            .client
            .request(
                manager,
                wp_linux_drm_syncobj_manager_v1::Request::ImportTimeline { id: timeline, fd },
            )
            .unwrap();
        synced
            .client
            .request(timeline.0, wp_linux_drm_syncobj_timeline_v1::Request::Destroy)
            .unwrap();
    }
    synced.client.roundtrip().unwrap();
    assert_eq!(
        compositor.with(|state| state.timelines_held()),
        1,
        "the compositor holds timelines whose objects were destroyed"
    );

    let buffer = dmabuf_buffer(&mut synced.client, BLUE);
    point(&mut synced.client, synced.syncobj_surface, synced.timeline, true, 1);
    point(&mut synced.client, synced.syncobj_surface, synced.timeline, false, 2);
    synced
        .client
        .request(synced.timeline, wp_linux_drm_syncobj_timeline_v1::Request::Destroy)
        .unwrap();
    attach_and_commit(&mut synced.client, synced.window.surface, buffer);
    synced.client.roundtrip().unwrap();

    assert_eq!(compositor.with(|state| state.timelines_held()), 0);
    assert_eq!(
        compositor.screen(0).pixel(60, 60),
        Some(RED),
        "drawn before its acquire point"
    );

    synced.handle.signal(1).unwrap();
    synced.client.roundtrip().unwrap();
    compositor.wait_until_still();

    assert_eq!(
        compositor.screen(0).pixel(60, 60),
        Some(BLUE),
        "not drawn after its acquire point"
    );
    assert_eq!(
        synced.handle.signalled().unwrap(),
        2,
        "the release point was not signalled"
    );
}

/// Expects the compositor to refuse what `send` does with `code`.
fn refused(code: u32, send: impl FnOnce(&mut Synced)) {
    let Some((compositor, node)) = compositor() else { return };
    let mut synced = synced(&compositor, &node);
    send(&mut synced);
    assert!(synced.client.roundtrip().is_err(), "the compositor accepted it");
    let error = synced.client.error.clone().unwrap();
    assert_eq!(error.code, code, "{error:?}");
}

#[test]
fn the_explicit_sync_errors() {
    use wp_linux_drm_syncobj_surface_v1::error;
    refused(error::NO_ACQUIRE_POINT, |synced| {
        let buffer = dmabuf_buffer(&mut synced.client, BLUE);
        point(&mut synced.client, synced.syncobj_surface, synced.timeline, false, 2);
        attach_and_commit(&mut synced.client, synced.window.surface, buffer);
    });
    refused(error::NO_RELEASE_POINT, |synced| {
        let buffer = dmabuf_buffer(&mut synced.client, BLUE);
        point(&mut synced.client, synced.syncobj_surface, synced.timeline, true, 1);
        attach_and_commit(&mut synced.client, synced.window.surface, buffer);
    });
    refused(error::UNSUPPORTED_BUFFER, |synced| {
        let buffer = synced.client.shm_buffer((64, 48), BLUE).unwrap();
        point(&mut synced.client, synced.syncobj_surface, synced.timeline, true, 1);
        point(&mut synced.client, synced.syncobj_surface, synced.timeline, false, 2);
        let id = buffer.buffer;
        attach_and_commit(&mut synced.client, synced.window.surface, id);
    });
    refused(error::NO_BUFFER, |synced| {
        point(&mut synced.client, synced.syncobj_surface, synced.timeline, true, 1);
        synced
            .client
            .request(synced.window.surface, wl_surface::Request::Commit)
            .unwrap();
    });
    refused(error::CONFLICTING_POINTS, |synced| {
        let buffer = dmabuf_buffer(&mut synced.client, BLUE);
        point(&mut synced.client, synced.syncobj_surface, synced.timeline, true, 3);
        point(&mut synced.client, synced.syncobj_surface, synced.timeline, false, 3);
        attach_and_commit(&mut synced.client, synced.window.surface, buffer);
    });
}

#[test]
fn a_descriptor_that_is_not_a_timeline_is_refused() {
    let Some((compositor, _node)) = compositor() else {
        return;
    };
    let mut client = compositor.app();
    let manager = client.bind(wp_linux_drm_syncobj_manager_v1::NAME, 1).unwrap();
    let memfd = rustix::fs::memfd_create("not-a-timeline", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    let timeline = client.new_id(wp_linux_drm_syncobj_timeline_v1::NAME);
    client
        .request(
            manager,
            wp_linux_drm_syncobj_manager_v1::Request::ImportTimeline {
                id: timeline,
                fd: memfd,
            },
        )
        .unwrap();

    assert!(client.roundtrip().is_err());
    assert_eq!(
        client.error.clone().unwrap().code,
        wp_linux_drm_syncobj_manager_v1::error::INVALID_TIMELINE
    );
}
