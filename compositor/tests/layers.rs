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
