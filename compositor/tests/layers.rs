//! Seam 1: layer surfaces and the room they reserve.

mod common;

use common::Compositor;
use crowbar_compositor::probe::{Anchor, KeyboardInteractivity, Layer, LayerRequest};

/// The test output is 1280 by 800. The bar reserves 44 at the top and the
/// dock 96 at the bottom, the sizes the shell uses.
#[test]
fn a_maximized_window_gets_the_room_the_shell_layers_leave() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell_client();
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
    assert_eq!(bar.size, (1280, 44));
    assert_eq!(dock.size, (400, 96));

    let mut app = compositor.ordinary_client();
    let window = app.open_toplevel("app", "test.app", (300, 200), 0xffff_ffff).unwrap();
    window.toplevel.set_maximized();
    window.surface.commit();
    app.roundtrip().unwrap();

    assert_eq!(app.state.toplevel_size, (1280, 800 - 44 - 96));
}

/// The launcher is an overlay layer surface that asks for the keyboard.
#[test]
fn an_exclusive_layer_takes_the_keyboard_until_it_closes() {
    use crowbar_compositor::probe::KeyEvent;
    const KEY_A: u32 = 30;
    let compositor = Compositor::start();
    let mut app = compositor.ordinary_client();
    app.listen_to_keyboard().unwrap();
    app.open_toplevel("app", "test.app", (300, 200), 0xffff_ffff).unwrap();
    app.take_keys().unwrap();
    let mut shell = compositor.shell_client();
    shell.listen_to_keyboard().unwrap();

    let launcher = shell
        .open_layer(
            LayerRequest {
                namespace: "test-launcher",
                layer: Layer::Overlay,
                anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
                size: (0, 0),
                exclusive_zone: -1,
                keyboard: KeyboardInteractivity::Exclusive,
            },
            0x8000_0000,
        )
        .unwrap();
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);

    assert_eq!(app.take_keys().unwrap(), [KeyEvent::Leave]);
    let seen = shell.take_keys().unwrap();
    assert!(
        seen.contains(&KeyEvent::Key {
            key: KEY_A,
            pressed: true
        }),
        "{seen:?}"
    );

    launcher.layer_surface.destroy();
    launcher.surface.destroy();
    shell.roundtrip().unwrap();

    assert!(matches!(app.take_keys().unwrap()[..], [KeyEvent::Enter { .. }, ..]));
}
