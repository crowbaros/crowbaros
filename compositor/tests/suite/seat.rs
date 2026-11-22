//! The seat: the keymap a client is given, where the pointer's events go,
//! and what a click does to the stack and to a window's activated state.

use crate::common;

use common::Compositor;
use crowbar_probe::Client;
use fulcrum_wire::{
    Arg, ObjectId,
    protocol::{
        wayland::{wl_compositor, wl_region, wl_seat, wl_surface},
        xdg_shell::xdg_toplevel,
    },
};

const WHITE: u32 = 0xffff_ffff;
const RED: u32 = 0xffff_0000;
const BLUE: u32 = 0xff00_00ff;
const BTN_LEFT: u32 = 0x110;

fn client(compositor: &Compositor) -> Client {
    compositor.app()
}

#[test]
fn a_keyboard_is_given_the_keymap_and_the_repeat_rate() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let keyboard = client.listen_to_keyboard().unwrap();
    let keymap = client.take_event(keyboard, "keymap").expect("no keymap");
    let [Arg::Uint(1), Arg::Fd(file), Arg::Uint(size)] = keymap.args.as_slice() else {
        panic!("{keymap:?}");
    };
    let mut start = vec![0u8; 10];
    rustix::io::pread(file, &mut start, 0).unwrap();
    assert_eq!(&start, b"xkb_keymap");
    assert!(*size > 1000);
    let repeat = client.take_event(keyboard, "repeat_info").expect("no repeat_info");
    assert!(matches!(repeat.args.as_slice(), [Arg::Int(25), Arg::Int(200)]));
}

#[test]
fn shift_is_sent_as_a_modifier() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let keyboard = client.listen_to_keyboard().unwrap();
    client.open_toplevel("mods", "test.mods", (100, 100), WHITE).unwrap();
    compositor.click(60, 60);
    client.roundtrip().unwrap();
    while client.take_event(keyboard, "modifiers").is_some() {}

    compositor.key(42, true);
    client.roundtrip().unwrap();
    let modifiers = client.take_event(keyboard, "modifiers").expect("no modifiers event");
    assert!(matches!(modifiers.args.as_slice(), [_, Arg::Uint(depressed), ..] if *depressed != 0));
}

#[test]
fn the_pointer_enters_moves_in_and_leaves_a_window_in_its_own_coordinates() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    client.listen_to_pointer().unwrap();
    let window = client.open_toplevel("ptr", "test.ptr", (100, 100), WHITE).unwrap();

    compositor.pointer(58, 68);
    compositor.pointer(60, 70);
    compositor.pointer(500, 500);
    let events = client.take_pointer_events().unwrap();
    let names: Vec<&str> = events.iter().map(|event| event.name).collect();
    assert_eq!(names, ["enter", "motion", "leave"]);
    // The window is at 48,48: 58,68 is 10,20 inside it.
    assert!(matches!(
        events[0].args.as_slice(),
        [_, Arg::Object(Some(ObjectId(surface))), Arg::Fixed(x), Arg::Fixed(y)]
            if *surface == window.surface && x.to_f64() == 10.0 && y.to_f64() == 20.0
    ));
}

/// A button held keeps the pointer with the surface it was pressed on, as a
/// drag of a scroll bar needs.
#[test]
fn a_held_button_keeps_the_pointer_with_its_surface() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    client.listen_to_pointer().unwrap();
    client.open_toplevel("grab", "test.grab", (100, 100), WHITE).unwrap();
    compositor.pointer(60, 60);
    compositor.with(|state| state.pointer_button(BTN_LEFT, true, 0));
    compositor.pointer(600, 600);
    compositor.with(|state| state.pointer_button(BTN_LEFT, false, 0));
    let names: Vec<&str> = client
        .take_pointer_events()
        .unwrap()
        .iter()
        .map(|event| event.name)
        .collect();
    assert_eq!(names, ["enter", "button", "motion", "button", "leave"]);
}

#[test]
fn a_click_raises_a_window_and_activates_it() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let first = client.open_toplevel("first", "test.first", (100, 100), RED).unwrap();
    let second = client.open_toplevel("second", "test.second", (100, 100), BLUE).unwrap();
    compositor.wait_until_still();
    // The second is at 80,80, over the first; 100,100 is in both.
    assert_eq!(compositor.screen(0).pixel(100, 100), Some(BLUE));

    compositor.click(60, 60);
    compositor.wait_until_still();
    assert_eq!(
        compositor.screen(0).pixel(100, 100),
        Some(RED),
        "the clicked window was not raised"
    );

    client.roundtrip().unwrap();
    let activated = |client: &mut Client, toplevel: u32| {
        let mut last = None;
        while let Some(event) = client.take_event(toplevel, "configure") {
            last = Some(event);
        }
        let event = last.expect("no configure");
        let [_, _, Arg::Array(states)] = event.args.as_slice() else {
            panic!("{event:?}");
        };
        states
            .as_chunks::<4>()
            .0
            .iter()
            .any(|state| u32::from_ne_bytes(*state) == xdg_toplevel::state::ACTIVATED)
    };
    assert!(activated(&mut client, first.toplevel));

    compositor.click(170, 170);
    client.roundtrip().unwrap();
    assert!(activated(&mut client, second.toplevel));
    assert!(
        !activated(&mut client, first.toplevel),
        "the window that lost the keyboard stayed activated"
    );
}

/// A window with an empty input region lets clicks through to what is
/// below.
#[test]
fn an_empty_input_region_lets_the_click_through() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let keyboard = client.listen_to_keyboard().unwrap();
    let below = client.open_toplevel("below", "test.below", (300, 300), RED).unwrap();
    let above = client.open_toplevel("above", "test.above", (300, 300), BLUE).unwrap();
    let compositor_object = client.bind(wl_compositor::NAME, 6).unwrap();
    let region = client.new_id(wl_region::NAME);
    client
        .request(compositor_object, wl_compositor::Request::CreateRegion { id: region })
        .unwrap();
    client
        .request(
            above.surface,
            wl_surface::Request::SetInputRegion {
                region: Some(ObjectId(region.0)),
            },
        )
        .unwrap();
    client.request(above.surface, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();

    compositor.click(200, 200);
    client.roundtrip().unwrap();
    let enter = client.take_event(keyboard, "enter").expect("no keyboard enter");
    assert!(matches!(enter.args.as_slice(), [_, Arg::Object(Some(ObjectId(surface))), _] if *surface == below.surface));
}

#[test]
fn a_seat_with_no_touch_device_refuses_one() {
    let compositor = Compositor::start();
    let mut client = client(&compositor);
    let seat = client.bind(wl_seat::NAME, 7).unwrap();
    client.roundtrip().unwrap();
    let capabilities = client.take_event(seat, "capabilities").expect("no capabilities");
    assert!(matches!(
        capabilities.args.as_slice(),
        [Arg::Uint(capabilities)] if *capabilities == wl_seat::capability::POINTER | wl_seat::capability::KEYBOARD
    ));
    let touch = client.new_id("wl_touch");
    client.request(seat, wl_seat::Request::GetTouch { id: touch }).unwrap();
    assert!(client.roundtrip().is_err());
    assert_eq!(client.error.as_ref().unwrap().code, wl_seat::error::MISSING_CAPABILITY);
}
