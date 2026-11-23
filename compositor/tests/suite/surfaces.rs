//! Surfaces and shared memory: what a client draws is what the compositor
//! has, pixel for pixel, at the moment the client commits it.

use crate::common;

use common::Compositor;
use crowbar_probe::Client;
use fulcrum_wire::{
    ObjectId,
    protocol::wayland::{wl_display, wl_shm, wl_shm_pool, wl_subcompositor, wl_subsurface, wl_surface},
};

const RED: u32 = 0xffff_0000;
const BLUE: u32 = 0xff00_00ff;
const HALF_GREEN: u32 = 0x8000_8000;

fn images(compositor: &Compositor) -> Vec<crowbar_compositor::image::Image> {
    compositor.with(|state| state.render_root_surfaces())
}

fn client(compositor: &Compositor) -> Client {
    compositor.app()
}

/// The exit check of Milestone 1: a client's shared-memory pixels appear in
/// the CPU renderer's output.
#[test]
fn a_clients_pixels_are_what_the_compositor_draws() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let surface = client.create_surface().unwrap();
    let buffer = client.shm_buffer((4, 3), RED).unwrap();
    client.show(surface, Some(&buffer)).unwrap();
    client.roundtrip().unwrap();

    let drawn = images(&compositor);
    assert_eq!(drawn.len(), 1);
    assert_eq!((drawn[0].width, drawn[0].height), (4, 3));
    assert!(drawn[0].pixels.iter().all(|pixel| *pixel == RED));
}

#[test]
fn nothing_changes_until_the_commit() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let surface = client.create_surface().unwrap();
    let buffer = client.shm_buffer((2, 2), RED).unwrap();
    client
        .request(
            surface,
            wl_surface::Request::Attach {
                buffer: Some(ObjectId(buffer.buffer)),
                x: 0,
                y: 0,
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    assert!(images(&compositor).is_empty(), "an attach showed before the commit");

    client.request(surface, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();
    assert_eq!(images(&compositor).len(), 1);
}

/// The pixels are copied at the commit, so the buffer is the client's again
/// at once, and what it draws next does not show until it commits again.
#[test]
fn a_committed_buffer_is_released_and_later_drawing_waits_for_a_commit() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let surface = client.create_surface().unwrap();
    let buffer = client.shm_buffer((2, 1), RED).unwrap();
    client.show(surface, Some(&buffer)).unwrap();
    client.roundtrip().unwrap();
    assert!(client.take_event(buffer.buffer, "release").is_some());

    let blue: Vec<u8> = [BLUE.to_le_bytes(), BLUE.to_le_bytes()].concat();
    rustix::io::pwrite(&buffer.file, &blue, 0).unwrap();
    client.roundtrip().unwrap();
    assert!(images(&compositor)[0].pixels.iter().all(|pixel| *pixel == RED));

    client.show(surface, Some(&buffer)).unwrap();
    client.roundtrip().unwrap();
    assert!(images(&compositor)[0].pixels.iter().all(|pixel| *pixel == BLUE));
}

#[test]
fn attaching_no_buffer_unmaps() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let surface = client.create_surface().unwrap();
    let buffer = client.shm_buffer((2, 2), RED).unwrap();
    client.show(surface, Some(&buffer)).unwrap();
    client.show(surface, None).unwrap();
    client.roundtrip().unwrap();
    assert!(images(&compositor).is_empty());
}

#[test]
fn premultiplied_alpha_blends_a_subsurface_over_its_parent() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let parent = client.create_surface().unwrap();
    let child = client.create_surface().unwrap();
    let subsurface = client.subsurface(child, parent).unwrap();
    client
        .request(subsurface, wl_subsurface::Request::SetPosition { x: 1, y: 0 })
        .unwrap();
    let back = client.shm_buffer((2, 1), 0xff00_0000).unwrap();
    let front = client.shm_buffer((1, 1), HALF_GREEN).unwrap();
    // The child is synchronized: its commit waits for the parent's.
    client.show(child, Some(&front)).unwrap();
    client.show(parent, Some(&back)).unwrap();
    client.roundtrip().unwrap();

    let drawn = &images(&compositor)[0];
    assert_eq!(drawn.pixel(0, 0), Some(0xff00_0000));
    assert_eq!(drawn.pixel(1, 0), Some(0xff00_8000));
}

#[test]
fn a_synchronized_subsurface_waits_for_its_parent_and_a_desynchronized_one_does_not() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let parent = client.create_surface().unwrap();
    let child = client.create_surface().unwrap();
    let subsurface = client.subsurface(child, parent).unwrap();
    let base = client.shm_buffer((2, 2), 0xff00_0000).unwrap();
    let red = client.shm_buffer((1, 1), RED).unwrap();
    let blue = client.shm_buffer((1, 1), BLUE).unwrap();
    client.show(parent, Some(&base)).unwrap();
    client.roundtrip().unwrap();

    client.show(child, Some(&red)).unwrap();
    client.roundtrip().unwrap();
    assert_eq!(
        images(&compositor)[0].pixel(0, 0),
        Some(0xff00_0000),
        "a synchronized commit showed early"
    );
    client.request(parent, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();
    assert_eq!(images(&compositor)[0].pixel(0, 0), Some(RED));

    client.request(subsurface, wl_subsurface::Request::SetDesync).unwrap();
    client.show(child, Some(&blue)).unwrap();
    client.roundtrip().unwrap();
    assert_eq!(images(&compositor)[0].pixel(0, 0), Some(BLUE));
}

/// Switching to desynchronized applies what was held.
#[test]
fn set_desync_applies_the_held_state() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let parent = client.create_surface().unwrap();
    let child = client.create_surface().unwrap();
    let subsurface = client.subsurface(child, parent).unwrap();
    let base = client.shm_buffer((1, 1), 0xff00_0000).unwrap();
    let red = client.shm_buffer((1, 1), RED).unwrap();
    client.show(parent, Some(&base)).unwrap();
    client.show(child, Some(&red)).unwrap();
    client.request(subsurface, wl_subsurface::Request::SetDesync).unwrap();
    client.roundtrip().unwrap();
    assert_eq!(images(&compositor)[0].pixel(0, 0), Some(RED));
}

#[test]
fn position_and_stacking_change_on_the_parents_commit() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let parent = client.create_surface().unwrap();
    let child = client.create_surface().unwrap();
    let subsurface = client.subsurface(child, parent).unwrap();
    let base = client.shm_buffer((2, 1), RED).unwrap();
    let blue = client.shm_buffer((1, 1), BLUE).unwrap();
    client.request(subsurface, wl_subsurface::Request::SetDesync).unwrap();
    client.show(parent, Some(&base)).unwrap();
    client.show(child, Some(&blue)).unwrap();
    client.roundtrip().unwrap();
    assert_eq!(
        images(&compositor)[0].pixel(0, 0),
        Some(BLUE),
        "a new subsurface is on top"
    );

    client
        .request(subsurface, wl_subsurface::Request::SetPosition { x: 1, y: 0 })
        .unwrap();
    client
        .request(
            subsurface,
            wl_subsurface::Request::PlaceBelow {
                sibling: ObjectId(parent),
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    assert_eq!(
        images(&compositor)[0].pixel(0, 0),
        Some(BLUE),
        "the change showed before the parent's commit"
    );

    client.request(parent, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();
    let drawn = &images(&compositor)[0];
    assert_eq!(drawn.pixel(0, 0), Some(RED));
    assert_eq!(
        drawn.pixel(1, 0),
        Some(RED),
        "the subsurface below its parent was drawn over it"
    );
}

/// Expects the compositor to refuse what `send` does with `code`.
fn refused(compositor: &Compositor, code: u32, send: impl FnOnce(&mut Client)) -> String {
    let mut client = client(compositor);
    send(&mut client);
    assert!(client.roundtrip().is_err(), "the compositor accepted it");
    let error = client.error.clone().expect("a protocol error");
    assert_eq!(error.code, code, "{error:?}");
    compositor.shell().roundtrip().unwrap();
    error.message
}

#[test]
fn a_surface_with_a_role_or_its_own_ancestor_cannot_become_a_subsurface() {
    let compositor = Compositor::start();
    refused(&compositor, wl_subcompositor::error::BAD_SURFACE, |client| {
        let (a, b, c) = (
            client.create_surface().unwrap(),
            client.create_surface().unwrap(),
            client.create_surface().unwrap(),
        );
        client.subsurface(a, b).unwrap();
        client.subsurface(a, c).unwrap();
    });
    refused(&compositor, wl_subcompositor::error::BAD_PARENT, |client| {
        let (a, b) = (client.create_surface().unwrap(), client.create_surface().unwrap());
        client.subsurface(b, a).unwrap();
        client.subsurface(a, b).unwrap();
    });
    refused(&compositor, wl_subcompositor::error::BAD_PARENT, |client| {
        let a = client.create_surface().unwrap();
        client.subsurface(a, a).unwrap();
    });
}

#[test]
fn subsurfaces_cannot_nest_without_limit() {
    let compositor = Compositor::start();
    let message = refused(&compositor, wl_subcompositor::error::BAD_PARENT, |client| {
        let mut parent = client.create_surface().unwrap();
        for _ in 0..=crowbar_compositor::MAX_SUBSURFACE_DEPTH {
            let child = client.create_surface().unwrap();
            client.subsurface(child, parent).unwrap();
            parent = child;
        }
    });
    assert!(message.contains("nested"), "{message}");
}

#[test]
fn buffers_that_do_not_fit_their_pool_or_format_are_refused() {
    let compositor = Compositor::start();
    let cases: [(i32, i32, i32, i32, u32, u32); 5] = [
        // offset, width, height, stride, format, error
        (0, 4, 4, 16, 0x3432_4752, wl_shm::error::INVALID_FORMAT),
        (0, 4, 4, 15, 0, wl_shm::error::INVALID_STRIDE),
        (4, 4, 4, 16, 0, wl_shm::error::INVALID_STRIDE),
        (0, 0, 4, 16, 0, wl_shm::error::INVALID_STRIDE),
        (-4, 4, 4, 16, 0, wl_shm::error::INVALID_STRIDE),
    ];
    for (offset, width, height, stride, format, code) in cases {
        refused(&compositor, code, |client| {
            let pool = client.shm_buffer((4, 4), RED).unwrap().pool;
            let id = client.new_id("wl_buffer");
            client
                .request(
                    pool,
                    wl_shm_pool::Request::CreateBuffer {
                        id,
                        offset,
                        width,
                        height,
                        stride,
                        format,
                    },
                )
                .unwrap();
        });
    }
    refused(&compositor, wl_shm::error::INVALID_STRIDE, |client| {
        let pool = client.shm_buffer((4, 4), RED).unwrap().pool;
        client.request(pool, wl_shm_pool::Request::Resize { size: 8 }).unwrap();
    });
}

/// A client can shorten its pool even within the last page of a buffer.
#[test]
fn a_client_that_truncates_its_pool_gets_an_error_and_the_compositor_goes_on() {
    let compositor = Compositor::start();
    for shortened_size in [0, 64 * 64 * 4 - 1] {
        let message = refused(&compositor, wl_shm::error::INVALID_FD, |client| {
            let surface = client.create_surface().unwrap();
            let buffer = client.shm_buffer((64, 64), RED).unwrap();
            client.roundtrip().unwrap();
            rustix::fs::ftruncate(&buffer.file, shortened_size).unwrap();
            client.show(surface, Some(&buffer)).unwrap();
        });
        assert!(message.contains("SHM"), "{message}");
    }
}

#[test]
fn a_buffer_must_be_a_multiple_of_its_scale() {
    let compositor = Compositor::start();
    refused(&compositor, wl_surface::error::INVALID_SIZE, |client| {
        let surface = client.create_surface().unwrap();
        let buffer = client.shm_buffer((3, 3), RED).unwrap();
        client
            .request(surface, wl_surface::Request::SetBufferScale { scale: 2 })
            .unwrap();
        client.show(surface, Some(&buffer)).unwrap();
    });
}

/// An object argument must name an object of the interface the protocol
/// says, as libwayland checks.
#[test]
fn an_object_of_the_wrong_interface_is_refused() {
    let compositor = Compositor::start();
    let message = refused(&compositor, wl_display::error::INVALID_OBJECT, |client| {
        let surface = client.create_surface().unwrap();
        let other = client.create_surface().unwrap();
        client
            .request(
                surface,
                wl_surface::Request::Attach {
                    buffer: Some(ObjectId(other)),
                    x: 0,
                    y: 0,
                },
            )
            .unwrap();
    });
    assert!(message.contains("attach.buffer"), "{message}");
}

#[test]
fn a_clients_surfaces_and_buffers_go_with_it() {
    let compositor = Compositor::start();
    {
        let mut client = client(&compositor);
        let parent = client.create_surface().unwrap();
        let child = client.create_surface().unwrap();
        client.subsurface(child, parent).unwrap();
        let buffer = client.shm_buffer((2, 2), RED).unwrap();
        client.show(parent, Some(&buffer)).unwrap();
        client.roundtrip().unwrap();
        assert_eq!(compositor.with(|state| state.resource_counts()), [2, 0, 1, 1]);
    }
    let gone = (0..200).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(5));
        compositor.with(|state| state.resource_counts()) == [0, 0, 0, 0]
    });
    assert!(gone, "a disconnected client's resources were kept");
}
