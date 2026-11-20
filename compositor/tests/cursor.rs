//! The pointer's image: the compositor's own, or a client's cursor surface,
//! which covers no frame (ADR 0020).

mod common;

use common::Compositor;
use crowbar_compositor::{FRAME_UNFOCUSED, backend::POINTER_COLOUR, image::Image};
use crowbar_probe::{Client, Toplevel};
use fulcrum_wire::{ObjectId, protocol::wayland::wl_pointer};

const RED: u32 = 0xffc0_2020;
const GREEN: u32 = 0xff20_c020;

fn screen(compositor: &Compositor) -> Image {
    compositor.with(|state| {
        let number = state.outputs()[0].number;
        state.render_output_with_cursor(number).unwrap()
    })
}

/// A client with a window at 48,48 of 200x100, listening to its pointer.
fn window(compositor: &Compositor) -> (Client, Toplevel, u32) {
    let mut client = compositor.app();
    let pointer = client.listen_to_pointer().unwrap();
    let window = client.open_toplevel("app", "test.app", (200, 100), GREEN).unwrap();
    (client, window, pointer)
}

/// A cursor surface of 16x16 in one colour, set with a hotspot of 2,2.
fn set_cursor(client: &mut Client, pointer: u32, colour: u32) -> u32 {
    let surface = client.create_surface().unwrap();
    let buffer = client.shm_buffer((16, 16), colour).unwrap();
    client.show(surface, Some(&buffer)).unwrap();
    client
        .request(
            pointer,
            wl_pointer::Request::SetCursor {
                serial: 0,
                surface: Some(ObjectId(surface)),
                hotspot_x: 2,
                hotspot_y: 2,
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    surface
}

#[test]
fn the_compositor_draws_its_own_pointer_where_no_client_set_one() {
    let compositor = Compositor::start();
    compositor.pointer(300, 300);

    let screen = screen(&compositor);

    assert_eq!(screen.pixel(305, 305), Some(POINTER_COLOUR));
    assert_ne!(screen.pixel(320, 320), Some(POINTER_COLOUR));
}

#[test]
fn the_client_with_the_pointer_sets_its_image_at_its_hotspot() {
    let compositor = Compositor::start();
    let (mut client, _window, pointer) = window(&compositor);
    compositor.pointer(120, 90);
    client.take_pointer_events().unwrap();

    set_cursor(&mut client, pointer, RED);

    let screen = screen(&compositor);
    // From 118,88 to 133,103.
    assert_eq!(screen.pixel(118, 88), Some(RED));
    assert_eq!(screen.pixel(133, 103), Some(RED));
    assert_eq!(screen.pixel(134, 104), Some(GREEN), "the window under it");
}

#[test]
fn a_client_without_the_pointer_cannot_set_its_image() {
    let compositor = Compositor::start();
    let (mut client, _window, pointer) = window(&compositor);
    compositor.pointer(600, 600);

    set_cursor(&mut client, pointer, RED);

    assert_eq!(screen(&compositor).pixel(605, 605), Some(POINTER_COLOUR));
}

#[test]
fn a_null_surface_hides_the_pointer_until_it_leaves() {
    let compositor = Compositor::start();
    let (mut client, _window, pointer) = window(&compositor);
    compositor.pointer(120, 90);
    client
        .request(
            pointer,
            wl_pointer::Request::SetCursor {
                serial: 0,
                surface: None,
                hotspot_x: 0,
                hotspot_y: 0,
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    assert_eq!(
        screen(&compositor).pixel(125, 95),
        Some(GREEN),
        "the pointer still shows"
    );

    compositor.pointer(600, 600);

    assert_eq!(
        screen(&compositor).pixel(605, 605),
        Some(POINTER_COLOUR),
        "the compositor's image did not come back"
    );
}

#[test]
fn a_cursor_image_covers_no_frame() {
    let compositor = Compositor::start();
    let (mut client, _window, pointer) = window(&compositor);
    // Just inside the window's left edge: the image's left part is over the
    // frame, at 46 and 47.
    compositor.pointer(52, 90);
    set_cursor(&mut client, pointer, RED);

    let screen = screen(&compositor);

    assert_eq!(
        screen.pixel(47, 92),
        Some(FRAME_UNFOCUSED),
        "the cursor covered the frame"
    );
    assert_eq!(screen.pixel(50, 92), Some(RED));
}

#[test]
fn a_surface_with_another_role_cannot_be_the_cursor() {
    let compositor = Compositor::start();
    let (mut client, window, pointer) = window(&compositor);
    compositor.pointer(120, 90);
    client.roundtrip().unwrap();

    client
        .request(
            pointer,
            wl_pointer::Request::SetCursor {
                serial: 0,
                surface: Some(ObjectId(window.surface)),
                hotspot_x: 0,
                hotspot_y: 0,
            },
        )
        .unwrap();

    assert!(client.roundtrip().is_err());
    assert_eq!(client.error.clone().unwrap().code, wl_pointer::error::ROLE);
}
