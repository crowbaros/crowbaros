//! `wp_viewporter`: a surface shows a crop of its buffer, scaled to the size
//! it asks for.

mod common;

use common::Compositor;
use crowbar_probe::{Client, Toplevel};
use fulcrum_wire::{
    Fixed, ObjectId,
    protocol::{
        viewporter::{wp_viewport, wp_viewporter},
        wayland::wl_surface,
    },
};

const RED: u32 = 0xffff_0000;
const BLUE: u32 = 0xff00_00ff;
/// Where the first window opens on the test output.
const AT: i32 = 48;

fn viewport(client: &mut Client, surface: u32) -> u32 {
    let viewporter = client.bind(wp_viewporter::NAME, 1).unwrap();
    let viewport = client.new_id(wp_viewport::NAME);
    client
        .request(
            viewporter,
            wp_viewporter::Request::GetViewport {
                id: viewport,
                surface: ObjectId(surface),
            },
        )
        .unwrap();
    viewport.0
}

fn set_source(client: &mut Client, viewport: u32, source: (f64, f64, f64, f64)) {
    client
        .request(
            viewport,
            wp_viewport::Request::SetSource {
                x: Fixed::from_f64(source.0),
                y: Fixed::from_f64(source.1),
                width: Fixed::from_f64(source.2),
                height: Fixed::from_f64(source.3),
            },
        )
        .unwrap();
}

fn set_destination(client: &mut Client, viewport: u32, size: (i32, i32)) {
    client
        .request(
            viewport,
            wp_viewport::Request::SetDestination {
                width: size.0,
                height: size.1,
            },
        )
        .unwrap();
}

/// A window of 100x50, left half red and right half blue.
fn halves(client: &mut Client) -> Toplevel {
    let window = client.open_toplevel("halves", "test.viewport", (100, 50), RED).unwrap();
    let buffer = client
        .painted_buffer((100, 50), |x, _| if x < 50 { RED } else { BLUE })
        .unwrap();
    client.show(window.surface, Some(&buffer)).unwrap();
    client.roundtrip().unwrap();
    window
}

fn window_size(compositor: &Compositor) -> (i32, i32) {
    let rect = compositor.with(|state| state.window_rects()[0]);
    (rect.width, rect.height)
}

#[test]
fn a_viewport_scales_the_buffer_to_its_destination() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let window = halves(&mut client);
    let viewport = viewport(&mut client, window.surface);

    set_destination(&mut client, viewport, (200, 100));
    client.request(window.surface, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();

    assert_eq!(window_size(&compositor), (200, 100));
    let screen = compositor.screen(0);
    assert_eq!(screen.pixel(AT + 99, AT + 99), Some(RED));
    assert_eq!(screen.pixel(AT + 100, AT + 99), Some(BLUE));
    assert_eq!(screen.pixel(AT + 199, AT + 99), Some(BLUE));
}

#[test]
fn a_viewport_shows_only_its_source_and_takes_its_size() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let window = halves(&mut client);
    let viewport = viewport(&mut client, window.surface);

    // The right half: blue alone.
    set_source(&mut client, viewport, (50.0, 0.0, 50.0, 50.0));
    client.request(window.surface, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();

    assert_eq!(window_size(&compositor), (50, 50));
    let screen = compositor.screen(0);
    assert_eq!(screen.pixel(AT, AT), Some(BLUE));
    assert_eq!(screen.pixel(AT + 49, AT + 49), Some(BLUE));
}

#[test]
fn a_destroyed_viewport_leaves_the_buffer_as_it_is_after_the_next_commit() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let window = halves(&mut client);
    let viewport = viewport(&mut client, window.surface);
    set_destination(&mut client, viewport, (10, 10));
    client.request(window.surface, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();
    assert_eq!(window_size(&compositor), (10, 10));

    client.request(viewport, wp_viewport::Request::Destroy).unwrap();
    client.roundtrip().unwrap();
    assert_eq!(window_size(&compositor), (10, 10), "applied before the commit");
    client.request(window.surface, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();

    assert_eq!(window_size(&compositor), (100, 50));
}

/// Expects the compositor to refuse what `send` does on a window's surface
/// with `code`, on the object `on` returns.
fn refused(code: u32, send: impl FnOnce(&mut Client, u32) -> u32) {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let window = halves(&mut client);
    let on = send(&mut client, window.surface);
    assert!(client.roundtrip().is_err(), "the compositor accepted it");
    let error = client.error.clone().expect("a protocol error");
    assert_eq!((error.object, error.code), (on, code), "{error:?}");
    compositor.shell().roundtrip().unwrap();
}

#[test]
fn the_viewport_errors() {
    // A second viewport for one surface.
    refused(wp_viewporter::error::VIEWPORT_EXISTS, |client, surface| {
        viewport(client, surface);
        let viewporter = client.bind(wp_viewporter::NAME, 1).unwrap();
        let second = client.new_id(wp_viewport::NAME);
        client
            .request(
                viewporter,
                wp_viewporter::Request::GetViewport {
                    id: second,
                    surface: ObjectId(surface),
                },
            )
            .unwrap();
        viewporter
    });
    // Sizes that are not sizes.
    refused(wp_viewport::error::BAD_VALUE, |client, surface| {
        let viewport = viewport(client, surface);
        set_destination(client, viewport, (0, 10));
        viewport
    });
    refused(wp_viewport::error::BAD_VALUE, |client, surface| {
        let viewport = viewport(client, surface);
        set_source(client, viewport, (-2.0, 0.0, 10.0, 10.0));
        viewport
    });
    // A crop that reaches past the buffer, found at the commit.
    refused(wp_viewport::error::OUT_OF_BUFFER, |client, surface| {
        let viewport = viewport(client, surface);
        set_source(client, viewport, (60.0, 0.0, 50.0, 50.0));
        client.request(surface, wl_surface::Request::Commit).unwrap();
        viewport
    });
    // A crop of a size that is not whole, with no destination to scale to.
    refused(wp_viewport::error::BAD_SIZE, |client, surface| {
        let viewport = viewport(client, surface);
        set_source(client, viewport, (0.0, 0.0, 10.5, 10.0));
        client.request(surface, wl_surface::Request::Commit).unwrap();
        viewport
    });
}

#[test]
fn a_viewport_whose_surface_is_gone_can_only_be_destroyed() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let surface = client.create_surface().unwrap();
    let gone = viewport(&mut client, surface);
    client.request(surface, wl_surface::Request::Destroy).unwrap();
    client.request(gone, wp_viewport::Request::Destroy).unwrap();
    client.roundtrip().unwrap();

    let surface = client.create_surface().unwrap();
    let orphan = viewport(&mut client, surface);
    client.request(surface, wl_surface::Request::Destroy).unwrap();
    set_destination(&mut client, orphan, (10, 10));
    assert!(client.roundtrip().is_err());
    let error = client.error.clone().unwrap();
    assert_eq!((error.object, error.code), (orphan, wp_viewport::error::NO_SURFACE));
}
