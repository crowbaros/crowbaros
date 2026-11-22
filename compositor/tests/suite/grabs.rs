//! Moving and resizing windows with the pointer (ADR 0020).

use crate::common;

use common::Compositor;
use crowbar_compositor::geometry::Rect;
use crowbar_probe::{Client, Toplevel};
use fulcrum_wire::{
    Arg, ObjectId,
    protocol::{wayland::wl_seat, xdg_shell::xdg_toplevel},
};

const KEY_SUPER: u32 = 125;
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const COLOUR: u32 = 0xff30_c030;

fn window(compositor: &Compositor) -> (Client, Toplevel) {
    let mut client = compositor.app();
    client.listen_to_pointer().unwrap();
    let window = client.open_toplevel("app", "test.app", (200, 100), COLOUR).unwrap();
    (client, window)
}

fn rect(compositor: &Compositor) -> Rect {
    compositor.with(|state| state.window_rects()[0])
}

fn button(compositor: &Compositor, button: u32, pressed: bool) {
    compositor.with(move |state| state.pointer_button(button, pressed, 0));
}

/// The size and states of a toplevel's last configure.
fn last_configure(client: &mut Client, window: &Toplevel) -> (i32, i32, Vec<u32>) {
    client.roundtrip().unwrap();
    let event = client
        .events
        .iter()
        .rev()
        .find(|event| event.object == window.toplevel && event.name == "configure")
        .expect("a configure");
    match event.args.as_slice() {
        [Arg::Int(width), Arg::Int(height), Arg::Array(states)] => (
            *width,
            *height,
            states
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| u32::from_ne_bytes(*bytes))
                .collect(),
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn super_and_the_left_button_move_a_window_and_the_client_hears_no_button() {
    let compositor = Compositor::start();
    let (mut client, _window) = window(&compositor);
    compositor.pointer(60, 60);
    client.take_pointer_events().unwrap();

    compositor.key(KEY_SUPER, true);
    button(&compositor, BTN_LEFT, true);
    compositor.pointer(160, 110);
    button(&compositor, BTN_LEFT, false);
    compositor.key(KEY_SUPER, false);

    assert_eq!(rect(&compositor), Rect::new(148, 98, 200, 100));
    let events = client.take_pointer_events().unwrap();
    assert!(
        !events.iter().any(|event| event.name == "button"),
        "the client heard the button: {events:?}"
    );
}

#[test]
fn a_client_moves_its_window_while_a_button_is_held_on_it() {
    let compositor = Compositor::start();
    let (mut client, window) = window(&compositor);
    let seat = client.bind(wl_seat::NAME, 7).unwrap();
    compositor.pointer(60, 60);
    button(&compositor, BTN_LEFT, true);

    client
        .request(
            window.toplevel,
            xdg_toplevel::Request::Move {
                seat: ObjectId(seat),
                serial: 0,
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    compositor.pointer(100, 100);
    button(&compositor, BTN_LEFT, false);

    assert_eq!(rect(&compositor), Rect::new(88, 88, 200, 100));
}

#[test]
fn a_move_asked_for_with_no_button_held_does_nothing() {
    let compositor = Compositor::start();
    let (mut client, window) = window(&compositor);
    let seat = client.bind(wl_seat::NAME, 7).unwrap();
    compositor.pointer(60, 60);

    client
        .request(
            window.toplevel,
            xdg_toplevel::Request::Move {
                seat: ObjectId(seat),
                serial: 0,
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    compositor.pointer(100, 100);

    assert_eq!(rect(&compositor), Rect::new(48, 48, 200, 100));
}

#[test]
fn super_and_the_right_button_resize_from_the_nearest_corner() {
    let compositor = Compositor::start();
    let (mut client, window) = window(&compositor);
    // The bottom-right quarter of a window at 48,48 of 200x100.
    compositor.pointer(200, 130);

    compositor.key(KEY_SUPER, true);
    button(&compositor, BTN_RIGHT, true);
    compositor.pointer(250, 180);
    let (width, height, states) = last_configure(&mut client, &window);
    client.redraw(&window, (1, 1), COLOUR).unwrap();
    button(&compositor, BTN_RIGHT, false);
    compositor.key(KEY_SUPER, false);
    let (_, _, after) = last_configure(&mut client, &window);

    assert_eq!((width, height), (250, 150));
    assert!(states.contains(&xdg_toplevel::state::RESIZING), "{states:?}");
    assert!(!after.contains(&xdg_toplevel::state::RESIZING), "{after:?}");
    assert_eq!(rect(&compositor), Rect::new(48, 48, 250, 150));
}

#[test]
fn a_resize_from_the_left_keeps_the_right_edge_where_it_was() {
    let compositor = Compositor::start();
    let (mut client, window) = window(&compositor);
    let seat = client.bind(wl_seat::NAME, 7).unwrap();
    compositor.pointer(50, 100);
    button(&compositor, BTN_LEFT, true);
    client
        .request(
            window.toplevel,
            xdg_toplevel::Request::Resize {
                seat: ObjectId(seat),
                serial: 0,
                edges: xdg_toplevel::resize_edge::LEFT,
            },
        )
        .unwrap();
    client.roundtrip().unwrap();

    compositor.pointer(20, 100);
    let (width, height, _) = last_configure(&mut client, &window);
    client.redraw(&window, (1, 1), COLOUR).unwrap();
    button(&compositor, BTN_LEFT, false);

    assert_eq!((width, height), (230, 100));
    assert_eq!(rect(&compositor), Rect::new(18, 48, 230, 100));
}

#[test]
fn a_resize_stays_within_the_size_the_client_can_take() {
    let compositor = Compositor::start();
    let (mut client, window) = window(&compositor);
    client
        .request(
            window.toplevel,
            xdg_toplevel::Request::SetMinSize { width: 150, height: 80 },
        )
        .unwrap();
    client.redraw(&window, (200, 100), COLOUR).unwrap();
    compositor.pointer(200, 130);

    compositor.key(KEY_SUPER, true);
    button(&compositor, BTN_RIGHT, true);
    compositor.pointer(50, 50);

    let (width, height, _) = last_configure(&mut client, &window);
    assert_eq!((width, height), (150, 80));
}

#[test]
fn moving_a_maximized_window_floats_it() {
    let compositor = Compositor::start();
    let (mut client, window) = window(&compositor);
    client
        .request(window.toplevel, xdg_toplevel::Request::SetMaximized)
        .unwrap();
    client.roundtrip().unwrap();
    client.redraw(&window, (1, 1), COLOUR).unwrap();
    compositor.pointer(300, 300);

    compositor.key(KEY_SUPER, true);
    button(&compositor, BTN_LEFT, true);
    let (width, height, states) = last_configure(&mut client, &window);
    button(&compositor, BTN_LEFT, false);
    compositor.key(KEY_SUPER, false);

    assert_eq!((width, height), (200, 100));
    assert!(!states.contains(&xdg_toplevel::state::MAXIMIZED));
}
