//! The Wayland socket as the shell sees it: layer surfaces and the room
//! they reserve.

mod common;

use common::{Compositor, OUTPUT_SIZE};
use crowbar_probe::{Anchor, KeyEvent, KeyboardInteractivity, Layer, LayerRequest};

/// The bar reserves 44 at the top and the dock 96 at the bottom, the sizes
/// the shell uses.
#[test]
fn the_shells_layers_get_their_size_and_reserve_their_room() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();

    let bar = shell
        .open_layer(
            LayerRequest {
                namespace: "test-bar",
                layer: Layer::Top,
                anchor: Anchor::Top | Anchor::Left | Anchor::Right,
                size: (0, 44),
                exclusive_zone: 44,
                keyboard: KeyboardInteractivity::None,
            },
            0xff20_4060,
        )
        .unwrap();
    let dock = shell
        .open_layer(
            LayerRequest {
                namespace: "test-dock",
                layer: Layer::Top,
                anchor: Anchor::Bottom,
                size: (400, 96),
                exclusive_zone: 96,
                keyboard: KeyboardInteractivity::None,
            },
            0xff40_6020,
        )
        .unwrap();

    assert_eq!(bar.size, (OUTPUT_SIZE.0 as u32, 44));
    assert_eq!(dock.size, (400, 96));
    let usable = compositor.with(|state| {
        let output = state.outputs().next().unwrap().clone();
        let area = state.usable_area(&output);
        (area.loc.x, area.loc.y, area.size.w, area.size.h)
    });
    assert_eq!(usable, (0, 44, OUTPUT_SIZE.0, OUTPUT_SIZE.1 - 44 - 96));
}

/// A new window opens in the room the shell's layers leave, not under the bar.
#[test]
fn a_new_window_opens_below_the_bar() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    shell
        .open_layer(
            LayerRequest {
                namespace: "test-bar",
                layer: Layer::Top,
                anchor: Anchor::Top | Anchor::Left | Anchor::Right,
                size: (0, 44),
                exclusive_zone: 44,
                keyboard: KeyboardInteractivity::None,
            },
            0xff20_4060,
        )
        .unwrap();
    let (mut client, _listener) = compositor.compartment("c1");

    client
        .open_toplevel("app", "test.app", (300, 200), 0xffff_ffff)
        .unwrap();

    let at = compositor.with(|state| {
        let window = state.space.elements().next().unwrap().clone();
        let at = state.space.element_location(&window).unwrap();
        (at.x, at.y)
    });
    assert_eq!(at, (48, 44 + 48));
}

/// The launcher is a layer surface that takes keys. A click on it gives it
/// the keyboard, and a click on a layer that takes none does not.
#[test]
fn a_click_gives_the_keyboard_to_a_layer_only_if_the_layer_takes_keys() {
    const KEY_A: u32 = 30;
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    shell.listen_to_keyboard().unwrap();
    let layer = |shell: &mut crowbar_probe::Client, name, anchor, keyboard| {
        shell
            .open_layer(
                LayerRequest {
                    namespace: name,
                    layer: Layer::Top,
                    anchor,
                    size: (0, 44),
                    exclusive_zone: 44,
                    keyboard,
                },
                0xff20_4060,
            )
            .unwrap()
    };
    let sides = Anchor::Left | Anchor::Right;
    layer(&mut shell, "test-bar", Anchor::Top | sides, KeyboardInteractivity::None);
    layer(
        &mut shell,
        "test-launcher",
        Anchor::Bottom | sides,
        KeyboardInteractivity::OnDemand,
    );

    compositor.click(600, 20);
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    assert_eq!(
        shell.take_keys().unwrap(),
        [],
        "a layer that takes no keys got the keyboard"
    );

    compositor.click(600, OUTPUT_SIZE.1 - 20);
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    let seen = shell.take_keys().unwrap();
    assert!(
        seen.contains(&KeyEvent::Key {
            key: KEY_A,
            pressed: true
        }),
        "{seen:?}"
    );
}
