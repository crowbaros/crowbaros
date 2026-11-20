//! Maximized, fullscreen, and snapped windows, and the frame around every
//! window (ADR 0020).

mod common;

use common::{Compositor, OUTPUT_SIZE};
use crowbar_compositor::{FRAME_FOCUSED, FRAME_WIDTH, geometry::Rect};
use crowbar_probe::{Client, LayerRequest, Toplevel};
use fulcrum_wire::{
    Arg,
    protocol::{
        wlr_layer_shell_unstable_v1::zwlr_layer_surface_v1::anchor::{LEFT, RIGHT, TOP},
        xdg_shell::xdg_toplevel,
    },
};

const KEY_SUPER: u32 = 125;
const KEY_F: u32 = 33;
const KEY_LEFT: u32 = 105;
const BAR: i32 = 44;
const BAR_COLOUR: u32 = 0xff20_4060;
const WINDOW: u32 = 0xff30_c030;

fn bar(compositor: &Compositor) -> Client {
    let mut shell = compositor.shell();
    shell
        .open_layer(
            LayerRequest {
                namespace: "test-bar",
                layer: 2,
                anchor: TOP | LEFT | RIGHT,
                size: (0, BAR as u32),
                exclusive_zone: BAR,
                keyboard: 0,
            },
            BAR_COLOUR,
        )
        .unwrap();
    shell
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

fn rect(compositor: &Compositor) -> Rect {
    compositor.with(|state| state.window_rects()[0])
}

#[test]
fn a_maximized_window_fills_the_room_the_bar_leaves_inside_its_frame() {
    let compositor = Compositor::start();
    let _shell = bar(&compositor);
    let mut client = compositor.app();
    let window = client.open_toplevel("app", "test.app", (100, 60), WINDOW).unwrap();

    client
        .request(window.toplevel, xdg_toplevel::Request::SetMaximized)
        .unwrap();
    let (width, height, states) = last_configure(&mut client, &window);
    client.redraw(&window, (1, 1), WINDOW).unwrap();

    let expected = Rect::new(
        FRAME_WIDTH,
        BAR + FRAME_WIDTH,
        OUTPUT_SIZE.0 - 2 * FRAME_WIDTH,
        OUTPUT_SIZE.1 - BAR - 2 * FRAME_WIDTH,
    );
    assert_eq!((width, height), (expected.width, expected.height));
    assert!(states.contains(&xdg_toplevel::state::MAXIMIZED), "{states:?}");
    assert_eq!(rect(&compositor), expected);
}

#[test]
fn leaving_a_mode_puts_the_window_back_where_and_as_it_floated() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client.open_toplevel("app", "test.app", (100, 60), WINDOW).unwrap();
    client
        .request(window.toplevel, xdg_toplevel::Request::SetMaximized)
        .unwrap();
    client.roundtrip().unwrap();
    client.redraw(&window, (1, 1), WINDOW).unwrap();

    client
        .request(window.toplevel, xdg_toplevel::Request::UnsetMaximized)
        .unwrap();
    let (width, height, states) = last_configure(&mut client, &window);
    client.redraw(&window, (1, 1), WINDOW).unwrap();

    assert_eq!((width, height), (100, 60));
    assert!(!states.contains(&xdg_toplevel::state::MAXIMIZED));
    assert_eq!(rect(&compositor), Rect::new(48, 48, 100, 60));
}

#[test]
fn a_window_that_asks_to_be_maximized_before_it_maps_is_configured_so() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let wm_base = client.bind("xdg_wm_base", 6).unwrap();
    let surface = client.create_surface().unwrap();
    let xdg = client.new_id("xdg_surface");
    client
        .request(
            wm_base,
            fulcrum_wire::protocol::xdg_shell::xdg_wm_base::Request::GetXdgSurface {
                id: xdg,
                surface: fulcrum_wire::ObjectId(surface),
            },
        )
        .unwrap();
    let toplevel = client.new_id("xdg_toplevel");
    client
        .request(
            xdg.0,
            fulcrum_wire::protocol::xdg_shell::xdg_surface::Request::GetToplevel { id: toplevel },
        )
        .unwrap();
    client.request(toplevel.0, xdg_toplevel::Request::SetMaximized).unwrap();
    client
        .request(surface, fulcrum_wire::protocol::wayland::wl_surface::Request::Commit)
        .unwrap();
    let window = Toplevel {
        surface,
        xdg_surface: xdg.0,
        toplevel: toplevel.0,
    };

    let (width, height, states) = last_configure(&mut client, &window);
    client.redraw(&window, (1, 1), WINDOW).unwrap();

    assert_eq!(
        (width, height),
        (OUTPUT_SIZE.0 - 2 * FRAME_WIDTH, OUTPUT_SIZE.1 - 2 * FRAME_WIDTH)
    );
    assert!(states.contains(&xdg_toplevel::state::MAXIMIZED));
    assert_eq!(rect(&compositor), Rect::new(FRAME_WIDTH, FRAME_WIDTH, width, height));
}

#[test]
fn super_f_makes_the_window_with_the_keyboard_cover_the_bar_and_keep_its_frame() {
    let compositor = Compositor::start();
    let _shell = bar(&compositor);
    let mut client = compositor.app();
    let window = client.open_toplevel("app", "test.app", (100, 60), WINDOW).unwrap();
    compositor.click(60, 100);

    for (key, pressed) in [(KEY_SUPER, true), (KEY_F, true), (KEY_F, false), (KEY_SUPER, false)] {
        compositor.key(key, pressed);
    }
    let (width, height, states) = last_configure(&mut client, &window);
    client.redraw(&window, (1, 1), WINDOW).unwrap();

    assert_eq!((width, height), OUTPUT_SIZE);
    assert!(states.contains(&xdg_toplevel::state::FULLSCREEN));
    let screen = compositor.screen(0);
    assert_eq!(
        screen.pixel(100, 20),
        Some(WINDOW),
        "the bar was drawn over the fullscreen window"
    );
    assert_eq!(
        screen.pixel(0, 100),
        Some(FRAME_FOCUSED),
        "no frame inside the output's edge"
    );
}

#[test]
fn super_left_snaps_the_window_to_the_left_half() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client.open_toplevel("app", "test.app", (100, 60), WINDOW).unwrap();
    compositor.click(60, 60);

    for (key, pressed) in [
        (KEY_SUPER, true),
        (KEY_LEFT, true),
        (KEY_LEFT, false),
        (KEY_SUPER, false),
    ] {
        compositor.key(key, pressed);
    }
    let (width, height, states) = last_configure(&mut client, &window);
    client.redraw(&window, (1, 1), WINDOW).unwrap();

    assert_eq!(
        (width, height),
        (OUTPUT_SIZE.0 / 2 - 2 * FRAME_WIDTH, OUTPUT_SIZE.1 - 2 * FRAME_WIDTH)
    );
    assert!(states.contains(&xdg_toplevel::state::TILED_LEFT), "{states:?}");
    assert_eq!(rect(&compositor).x, FRAME_WIDTH);
}

#[test]
fn a_maximized_window_is_fitted_again_when_its_output_changes() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client.open_toplevel("app", "test.app", (100, 60), WINDOW).unwrap();
    client
        .request(window.toplevel, xdg_toplevel::Request::SetMaximized)
        .unwrap();
    client.roundtrip().unwrap();

    compositor.with(|state| {
        let number = state.outputs()[0].number;
        state.resize_output(number, (1600, 900));
    });

    let (width, height, _) = last_configure(&mut client, &window);
    assert_eq!((width, height), (1600 - 2 * FRAME_WIDTH, 900 - 2 * FRAME_WIDTH));
}

#[test]
fn a_client_is_told_it_may_ask_to_maximize_and_go_fullscreen() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client.open_toplevel("app", "test.app", (100, 60), WINDOW).unwrap();
    client.roundtrip().unwrap();

    let capabilities = client
        .events
        .iter()
        .rev()
        .find(|event| event.object == window.toplevel && event.name == "wm_capabilities")
        .map(|event| match event.args.as_slice() {
            [Arg::Array(bytes)] => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| u32::from_ne_bytes(*bytes))
                .collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        });
    assert_eq!(
        capabilities,
        Some(vec![
            xdg_toplevel::wm_capabilities::MAXIMIZE,
            xdg_toplevel::wm_capabilities::FULLSCREEN
        ])
    );
}
