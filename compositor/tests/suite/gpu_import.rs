//! The GPU's own import of dma-bufs: a buffer it takes is drawn where it is,
//! not copied, and the compositor holds it until other content replaces it.
//!
//! CI's machines have no GPU and no dma-bufs: there, Mesa's software
//! renderer imports memfds, which the tests let through in place of
//! dma-bufs. A GPU's own driver takes no memfd, so on a machine with one,
//! `LIBGL_ALWAYS_SOFTWARE=1` runs those tests. A machine with a render node
//! also runs a buffer in its GPU's own layout. Where EGL imports nothing,
//! each test says so and passes.

use crate::common;

use std::os::fd::{AsFd, OwnedFd};

use common::{Compositor, OUTPUT_SIZE, TEST_DEVICE, own_uid};
use crowbar_compositor::{
    State,
    image::Image,
    render::{Renderer, render_output_offscreen},
};
use crowbar_probe::{Client, Toplevel};
use fulcrum_sys::{
    gbm,
    gles::{Dmabuf, DmabufPlane, Gles},
};
use fulcrum_wire::{
    ObjectId,
    protocol::{
        linux_dmabuf_v1::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1},
        wayland::{wl_shm, wl_surface},
    },
};

const XR24: u32 = u32::from_le_bytes(*b"XR24");
const LINEAR: u64 = 0;
const RED: u32 = 0xffc0_2020;
const BLUE: u32 = 0xff20_20c0;
const GREEN: u32 = 0xff20_c020;
/// Where a 64x48 window's content is on the screen.
const INSIDE: (i32, i32) = (48 + 10, 48 + 10);

/// A compositor whose GPU imports dma-bufs, made by `gpu` on its thread, or
/// None where there is no such GPU.
fn compositor_with(gpu: fn() -> Option<Gles>, memfds: bool) -> Option<Compositor> {
    let compositor = Compositor::start_with(own_uid(), own_uid(), move |state| {
        state.add_output("TEST-1", OUTPUT_SIZE, 60_000);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
        let gles = gpu().filter(|gles| !memfds || takes_memfds(gles));
        if let Some(gles) = gles {
            state.offer_dmabuf_on_gpu(TEST_DEVICE, gles);
            if memfds {
                state.import_memfds_for_tests();
            }
        }
    });
    if compositor.with(|state| state.dmabuf_gpu().is_none()) {
        eprintln!(
            "no GPU that imports {}: skipped",
            if memfds { "memfds" } else { "dma-bufs" }
        );
        return None;
    }
    Some(compositor)
}

/// Whether the GPU imports a memfd as a dma-buf, as a software renderer
/// does and a GPU's own driver does not.
fn takes_memfds(gles: &Gles) -> bool {
    let dmabuf = Dmabuf {
        width: 64,
        height: 48,
        format: XR24,
        modifier: LINEAR,
        planes: vec![DmabufPlane {
            fd: memfd(RED),
            offset: 0,
            stride: 64 * 4,
        }],
    };
    gles.import(&dmabuf).is_ok()
}

fn surfaceless() -> Option<Gles> {
    Gles::surfaceless().ok()
}

/// What the GPU draws on the first output.
fn gpu_screen(compositor: &Compositor) -> Image {
    compositor.with(|state: &mut State| {
        let mut renderer = Renderer::new(state.dmabuf_gpu().unwrap());
        let number = state.outputs()[0].number;
        render_output_offscreen(state, number, &mut renderer).unwrap()
    })
}

/// A memfd of 64x48 pixels of one colour, in rows.
fn memfd(argb: u32) -> OwnedFd {
    let fd = rustix::fs::memfd_create("test-dmabuf", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    paint(&fd, argb);
    fd
}

fn paint(fd: &OwnedFd, argb: u32) {
    let bytes: Vec<u8> = std::iter::repeat_n(argb.to_le_bytes(), 64 * 48).flatten().collect();
    rustix::io::pwrite(fd, &bytes, 0).unwrap();
}

/// A 64x48 buffer from planes, as the client gives them.
fn buffer(client: &mut Client, planes: Vec<(OwnedFd, u32, u32)>, modifier: u64) -> u32 {
    let dmabuf = client.bind(zwp_linux_dmabuf_v1::NAME, 4).unwrap();
    let params = client.new_id(zwp_linux_buffer_params_v1::NAME);
    client
        .request(dmabuf, zwp_linux_dmabuf_v1::Request::CreateParams { params_id: params })
        .unwrap();
    for (index, (fd, offset, stride)) in planes.into_iter().enumerate() {
        client
            .request(
                params.0,
                zwp_linux_buffer_params_v1::Request::Add {
                    fd,
                    plane_idx: index as u32,
                    offset,
                    stride,
                    modifier_hi: (modifier >> 32) as u32,
                    modifier_lo: modifier as u32,
                },
            )
            .unwrap();
    }
    let buffer = client.new_id("wl_buffer");
    client
        .request(
            params.0,
            zwp_linux_buffer_params_v1::Request::CreateImmed {
                buffer_id: buffer,
                width: 64,
                height: 48,
                format: XR24,
                flags: 0,
            },
        )
        .unwrap();
    buffer.0
}

fn linear(client: &mut Client, fd: OwnedFd) -> u32 {
    buffer(client, vec![(fd, 0, 64 * 4)], LINEAR)
}

fn show(client: &mut Client, surface: u32, buffer: Option<u32>) {
    client
        .request(
            surface,
            wl_surface::Request::Attach {
                buffer: buffer.map(ObjectId),
                x: 0,
                y: 0,
            },
        )
        .unwrap();
    client
        .request(
            surface,
            wl_surface::Request::Damage {
                x: 0,
                y: 0,
                width: i32::MAX,
                height: i32::MAX,
            },
        )
        .unwrap();
    client.request(surface, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();
}

fn released(client: &mut Client, buffer: u32) -> bool {
    client.take_event(buffer, "release").is_some()
}

fn window(compositor: &Compositor) -> (Client, Toplevel) {
    let mut client = compositor.app();
    let window = client.open_toplevel("gpu", "test.gpu", (64, 48), GREEN).unwrap();
    (client, window)
}

#[test]
fn a_dmabuf_the_gpu_imports_is_drawn_where_it_is_and_held() {
    let Some(compositor) = compositor_with(surfaceless, true) else {
        return;
    };
    let (mut client, window) = window(&compositor);
    let fd = memfd(RED);
    let kept = fd.try_clone().unwrap();
    let buffer = linear(&mut client, fd);

    show(&mut client, window.surface, Some(buffer));

    assert_eq!(gpu_screen(&compositor).pixel(INSIDE.0, INSIDE.1), Some(RED));
    assert!(!released(&mut client, buffer), "a buffer the GPU shows is held");
    assert_eq!(
        compositor.screen(0).pixel(INSIDE.0, INSIDE.1),
        Some(crowbar_compositor::BACKGROUND),
        "the CPU copied it"
    );

    // The client draws into the buffer again, as it may not while it is
    // held; that it shows is what drawing where it is means.
    paint(&kept, BLUE);
    show(&mut client, window.surface, Some(buffer));

    assert_eq!(gpu_screen(&compositor).pixel(INSIDE.0, INSIDE.1), Some(BLUE));
    assert!(!released(&mut client, buffer), "a buffer shown again is still held");
}

#[test]
fn a_held_buffer_is_released_when_other_content_replaces_it() {
    let Some(compositor) = compositor_with(surfaceless, true) else {
        return;
    };
    let (mut client, window) = window(&compositor);
    let first = linear(&mut client, memfd(RED));
    let second = linear(&mut client, memfd(BLUE));

    show(&mut client, window.surface, Some(first));
    show(&mut client, window.surface, Some(second));

    assert!(released(&mut client, first), "the replaced buffer was not released");
    assert!(!released(&mut client, second));
    assert_eq!(gpu_screen(&compositor).pixel(INSIDE.0, INSIDE.1), Some(BLUE));

    let pixels = client.shm_buffer((64, 48), GREEN).unwrap();
    show(&mut client, window.surface, Some(pixels.buffer));

    assert!(
        released(&mut client, second),
        "pixels replaced it, and it was not released"
    );
}

#[test]
fn a_surface_that_is_destroyed_lets_its_buffer_go() {
    let Some(compositor) = compositor_with(surfaceless, true) else {
        return;
    };
    let (mut client, _window) = window(&compositor);
    let surface = client.create_surface().unwrap();
    let buffer = linear(&mut client, memfd(RED));
    show(&mut client, surface, Some(buffer));
    assert!(!released(&mut client, buffer));

    client.request(surface, wl_surface::Request::Destroy).unwrap();
    client.roundtrip().unwrap();

    assert!(released(&mut client, buffer));
}

/// A software renderer reads a memfd it imported outside the guarded
/// mapping, so a client could crash the compositor by shrinking it. Only
/// dma-bufs go to the GPU: a memfd is copied, and a shrunk one is the
/// client's error.
#[test]
fn a_memfd_is_copied_and_never_reaches_the_gpu() {
    let Some(compositor) = compositor_with(surfaceless, false) else {
        return;
    };
    let (mut client, window) = window(&compositor);
    let buffer = linear(&mut client, memfd(RED));
    show(&mut client, window.surface, Some(buffer));
    assert!(released(&mut client, buffer), "a copied buffer is released at once");
    assert_eq!(compositor.screen(0).pixel(INSIDE.0, INSIDE.1), Some(RED));

    let fd = memfd(BLUE);
    let kept = fd.try_clone().unwrap();
    let shrinking = linear(&mut client, fd);
    client.roundtrip().unwrap();
    rustix::fs::ftruncate(&kept, 0).unwrap();
    client
        .request(
            window.surface,
            wl_surface::Request::Attach {
                buffer: Some(ObjectId(shrinking)),
                x: 0,
                y: 0,
            },
        )
        .unwrap();
    client.request(window.surface, wl_surface::Request::Commit).unwrap();

    assert!(client.roundtrip().is_err());
    assert_eq!(client.error.clone().unwrap().code, wl_shm::error::INVALID_FD);
    compositor.shell().roundtrip().unwrap();
}

/// The machine's GPU, through GBM on its render node.
fn render_node_gpu() -> Option<Gles> {
    let device = render_node_gbm()?;
    Gles::on_gbm(&device).ok()
}

fn render_node_gbm() -> Option<gbm::Device> {
    gbm::Device::new(render_node_fd()?).ok()
}

#[test]
fn a_buffer_in_the_gpus_own_layout_is_shown() {
    let Some(compositor) = compositor_with(render_node_gpu, false) else {
        return;
    };
    // The client's side: a buffer in a layout the GPU chose, drawn by it.
    let device = render_node_gbm().unwrap();
    let gles = Gles::on_gbm(&device).unwrap();
    let modifiers: Vec<u64> = gles
        .dmabuf_formats()
        .iter()
        .filter(|format| format.format == XR24)
        .map(|format| format.modifier)
        .collect();
    let made = device
        .create_buffer(64, 48, XR24, &modifiers, gbm::USE_RENDERING)
        .unwrap();
    let planes = || -> Vec<(OwnedFd, u32, u32)> {
        made.planes()
            .iter()
            .enumerate()
            .map(|(index, plane)| (made.export(index).unwrap(), plane.offset, plane.stride))
            .collect()
    };
    let target = gles
        .target(&Dmabuf {
            width: 64,
            height: 48,
            format: XR24,
            modifier: made.modifier(),
            planes: planes()
                .into_iter()
                .map(|(fd, offset, stride)| DmabufPlane { fd, offset, stride })
                .collect(),
        })
        .unwrap();
    let mut frame = gles.frame(&target);
    frame.clear((0, 0, 64, 48), RED);
    frame.finish();
    assert!(fulcrum_sys::dmabuf::is_dmabuf(planes()[0].0.as_fd()));

    let (mut client, window) = window(&compositor);
    let buffer = buffer(&mut client, planes(), made.modifier());
    show(&mut client, window.surface, Some(buffer));

    assert_eq!(
        gpu_screen(&compositor).pixel(INSIDE.0, INSIDE.1),
        Some(RED),
        "modifier {:#x}",
        made.modifier()
    );
    assert!(!released(&mut client, buffer));
}

/// Under explicit sync, a held buffer's release point is signalled when
/// other content replaces it, not at its commit.
#[test]
fn a_held_buffers_release_point_waits_for_its_replacement() {
    use fulcrum_sys::syncobj::{self, Timeline};
    use fulcrum_wire::protocol::linux_drm_syncobj_v1::{
        wp_linux_drm_syncobj_manager_v1, wp_linux_drm_syncobj_surface_v1, wp_linux_drm_syncobj_timeline_v1,
    };

    let Some(node) = render_node_fd().filter(|node| syncobj::supported(node.as_fd())) else {
        eprintln!("no render node with timeline syncobjs: skipped");
        return;
    };
    let offered = node.try_clone().unwrap();
    let node = std::rc::Rc::new(node);
    let compositor = Compositor::start_with(own_uid(), own_uid(), move |state| {
        state.add_output("TEST-1", OUTPUT_SIZE, 60_000);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
        state.offer_explicit_sync(offered);
        if let Some(gles) = surfaceless().filter(takes_memfds) {
            state.offer_dmabuf_on_gpu(TEST_DEVICE, gles);
            state.import_memfds_for_tests();
        }
    });
    if compositor.with(|state| state.dmabuf_gpu().is_none()) {
        eprintln!("no GPU that imports memfds: skipped");
        return;
    }
    let (mut client, window) = window(&compositor);
    let manager = client.bind(wp_linux_drm_syncobj_manager_v1::NAME, 1).unwrap();
    // One timeline the client's GPU signals, and one the compositor does.
    let timeline = |client: &mut Client| {
        let exported = syncobj::create_exported(node.as_fd()).unwrap();
        let handle = Timeline::import(&node, exported.as_fd()).unwrap();
        let id = client.new_id(wp_linux_drm_syncobj_timeline_v1::NAME);
        client
            .request(
                manager,
                wp_linux_drm_syncobj_manager_v1::Request::ImportTimeline { id, fd: exported },
            )
            .unwrap();
        (id.0, handle)
    };
    let (acquire, acquire_handle) = timeline(&mut client);
    let (release, release_handle) = timeline(&mut client);
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
    let points = |client: &mut Client, acquire_point: u32, release_point: u32| {
        client
            .request(
                syncobj_surface.0,
                wp_linux_drm_syncobj_surface_v1::Request::SetAcquirePoint {
                    timeline: ObjectId(acquire),
                    point_hi: 0,
                    point_lo: acquire_point,
                },
            )
            .unwrap();
        client
            .request(
                syncobj_surface.0,
                wp_linux_drm_syncobj_surface_v1::Request::SetReleasePoint {
                    timeline: ObjectId(release),
                    point_hi: 0,
                    point_lo: release_point,
                },
            )
            .unwrap();
    };
    acquire_handle.signal(2).unwrap();
    let first = linear(&mut client, memfd(RED));
    let second = linear(&mut client, memfd(BLUE));

    points(&mut client, 1, 1);
    show(&mut client, window.surface, Some(first));
    assert_eq!(release_handle.signalled().unwrap(), 0, "signalled while it is shown");

    points(&mut client, 2, 2);
    show(&mut client, window.surface, Some(second));
    assert_eq!(
        release_handle.signalled().unwrap(),
        1,
        "the replaced buffer's point, and not the shown one's"
    );
    assert_eq!(gpu_screen(&compositor).pixel(INSIDE.0, INSIDE.1), Some(BLUE));
}

fn render_node_fd() -> Option<OwnedFd> {
    let path = std::fs::read_dir("/dev/dri")
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| path.to_string_lossy().contains("renderD"))?;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .ok()
        .map(Into::into)
}
