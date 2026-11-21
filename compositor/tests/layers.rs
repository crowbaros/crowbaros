//! The Wayland socket as the shell sees it: layer surfaces, where they go,
//! and the room they reserve.

mod common;

use common::{Compositor, OUTPUT_SIZE};
use crowbar_compositor::geometry::Rect;
use crowbar_probe::{Client, LayerRequest};
use fulcrum_wire::protocol::{
    wayland::wl_surface,
    wlr_layer_shell_unstable_v1::{zwlr_layer_shell_v1, zwlr_layer_surface_v1},
};

use zwlr_layer_surface_v1::{
    anchor::{BOTTOM, LEFT, RIGHT, TOP},
    keyboard_interactivity::{EXCLUSIVE, ON_DEMAND},
};

const BACKGROUND: u32 = 0;
const TOP_LAYER: u32 = 2;
const OVERLAY: u32 = 3;

fn bar(namespace: &str) -> LayerRequest<'_> {
    LayerRequest {
        namespace,
        layer: TOP_LAYER,
        anchor: TOP | LEFT | RIGHT,
        size: (0, 44),
        exclusive_zone: 44,
        keyboard: 0,
    }
}

/// The bar reserves 44 at the top and the dock 96 at the bottom, the sizes
/// the shell uses.
#[test]
fn the_shells_layers_get_their_size_and_reserve_their_room() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();

    let bar = shell.open_layer(bar("test-bar"), 0xff20_4060).unwrap();
    let dock = shell
        .open_layer(
            LayerRequest {
                namespace: "test-dock",
                layer: TOP_LAYER,
                anchor: BOTTOM,
                size: (400, 96),
                exclusive_zone: 96,
                keyboard: 0,
            },
            0xff40_6020,
        )
        .unwrap();

    assert_eq!(bar.size, (OUTPUT_SIZE.0 as u32, 44));
    assert_eq!(dock.size, (400, 96));
    let usable = compositor.with(|state| {
        let output = state.outputs()[0].number;
        state.usable_area(output)
    });
    assert_eq!(usable, Rect::new(0, 44, OUTPUT_SIZE.0, OUTPUT_SIZE.1 - 44 - 96));
    let screen = compositor.screen(0);
    assert_eq!(screen.pixel(0, 0), Some(0xff20_4060));
    assert_eq!(screen.pixel(OUTPUT_SIZE.0 / 2, OUTPUT_SIZE.1 - 1), Some(0xff40_6020));
}

/// A new window opens in the room the shell's layers leave, not under the bar.
#[test]
fn a_new_window_opens_below_the_bar() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    shell.open_layer(bar("test-bar"), 0xff20_4060).unwrap();
    let mut client = compositor.app();

    client
        .open_toplevel("app", "test.app", (300, 200), 0xffff_ffff)
        .unwrap();

    let at = compositor.with(|state| state.window_rects()[0]);
    assert_eq!((at.x, at.y), (48, 44 + 48));
}

/// The wallpaper is below every window, and an overlay above every one.
#[test]
fn background_layers_are_below_windows_and_overlays_above() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    shell
        .open_layer(
            LayerRequest {
                namespace: "test-wallpaper",
                layer: BACKGROUND,
                anchor: TOP | BOTTOM | LEFT | RIGHT,
                size: (0, 0),
                exclusive_zone: -1,
                keyboard: 0,
            },
            0xff11_2233,
        )
        .unwrap();
    let mut client = compositor.app();
    client
        .open_toplevel("app", "test.app", (300, 200), 0xffff_ffff)
        .unwrap();
    shell
        .open_layer(
            LayerRequest {
                namespace: "test-overlay",
                layer: OVERLAY,
                anchor: TOP | LEFT,
                size: (100, 100),
                exclusive_zone: 0,
                keyboard: 0,
            },
            0xff00_00ff,
        )
        .unwrap();
    compositor.wait_until_still();

    let screen = compositor.screen(0);
    assert_eq!(screen.pixel(1000, 600), Some(0xff11_2233), "the wallpaper");
    assert_eq!(
        screen.pixel(200, 200),
        Some(0xffff_ffff),
        "the window, over the wallpaper"
    );
    assert_eq!(screen.pixel(60, 60), Some(0xff00_00ff), "the overlay, over the window");
}

/// A layer that ignores exclusive zones (-1) covers the whole output, and
/// the first layer to reserve room keeps it.
#[test]
fn exclusive_zones_are_kept_in_the_order_layers_were_made() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    shell.open_layer(bar("first"), 0xff20_4060).unwrap();
    let second = shell.open_layer(bar("second"), 0xff60_4020).unwrap();
    assert_eq!(second.size, (OUTPUT_SIZE.0 as u32, 44));
    let rects = compositor.with(|state| state.layer_rects_by_namespace());
    assert_eq!(rects[0], ("first".into(), Rect::new(0, 0, OUTPUT_SIZE.0, 44)));
    assert_eq!(rects[1], ("second".into(), Rect::new(0, 44, OUTPUT_SIZE.0, 44)));
}

/// The shell lays out a bar again, larger: the usable area follows.
#[test]
fn a_layer_that_changes_its_zone_moves_the_usable_area() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    let bar = shell.open_layer(bar("test-bar"), 0xff20_4060).unwrap();
    shell
        .request(
            bar.layer_surface,
            zwlr_layer_surface_v1::Request::SetExclusiveZone { zone: 60 },
        )
        .unwrap();
    shell.request(bar.surface, wl_surface::Request::Commit).unwrap();
    shell.roundtrip().unwrap();
    let usable = compositor.with(|state| state.usable_area(state.outputs()[0].number));
    assert_eq!(usable.y, 60);
}

fn refused(compositor: &Compositor, code: u32, send: impl FnOnce(&mut Client)) {
    let mut shell = compositor.shell();
    send(&mut shell);
    assert!(shell.roundtrip().is_err(), "the compositor accepted it");
    let error = shell.error.clone().expect("a protocol error");
    assert_eq!(error.code, code, "{error:?}");
}

#[test]
fn the_layer_shell_errors() {
    let compositor = Compositor::start();
    // A size of zero not stretched between two edges.
    refused(&compositor, zwlr_layer_surface_v1::error::INVALID_SIZE, |shell| {
        let layer_shell = shell.bind(zwlr_layer_shell_v1::NAME, 4).unwrap();
        let surface = shell.create_surface().unwrap();
        let id = shell.new_id(zwlr_layer_surface_v1::NAME);
        shell
            .request(
                layer_shell,
                zwlr_layer_shell_v1::Request::GetLayerSurface {
                    id,
                    surface: fulcrum_wire::ObjectId(surface),
                    output: None,
                    layer: TOP_LAYER,
                    namespace: "bad".into(),
                },
            )
            .unwrap();
        shell
            .request(id.0, zwlr_layer_surface_v1::Request::SetAnchor { anchor: TOP })
            .unwrap();
        shell.request(surface, wl_surface::Request::Commit).unwrap();
    });
    // An unknown layer.
    refused(&compositor, zwlr_layer_shell_v1::error::INVALID_LAYER, |shell| {
        let layer_shell = shell.bind(zwlr_layer_shell_v1::NAME, 4).unwrap();
        let surface = shell.create_surface().unwrap();
        let id = shell.new_id(zwlr_layer_surface_v1::NAME);
        shell
            .request(
                layer_shell,
                zwlr_layer_shell_v1::Request::GetLayerSurface {
                    id,
                    surface: fulcrum_wire::ObjectId(surface),
                    output: None,
                    layer: 9,
                    namespace: "bad".into(),
                },
            )
            .unwrap();
    });
    // A buffer before the first configure.
    refused(
        &compositor,
        zwlr_layer_surface_v1::error::INVALID_SURFACE_STATE,
        |shell| {
            let layer_shell = shell.bind(zwlr_layer_shell_v1::NAME, 4).unwrap();
            let surface = shell.create_surface().unwrap();
            let id = shell.new_id(zwlr_layer_surface_v1::NAME);
            shell
                .request(
                    layer_shell,
                    zwlr_layer_shell_v1::Request::GetLayerSurface {
                        id,
                        surface: fulcrum_wire::ObjectId(surface),
                        output: None,
                        layer: TOP_LAYER,
                        namespace: "early".into(),
                    },
                )
                .unwrap();
            shell
                .request(id.0, zwlr_layer_surface_v1::Request::SetSize { width: 10, height: 10 })
                .unwrap();
            let buffer = shell.shm_buffer((10, 10), 0xffff_ffff).unwrap();
            shell.show(surface, Some(&buffer)).unwrap();
        },
    );
}

/// The output a layer is on goes away: the shell is told it is closed.
#[test]
fn a_layer_on_an_output_that_goes_is_closed() {
    let compositor = Compositor::start();
    let second = compositor.with(|state| state.add_output("TEST-2", OUTPUT_SIZE, 60_000));
    let mut shell = compositor.shell();
    let output = shell
        .globals
        .iter()
        .filter(|global| global.interface == "wl_output")
        .map(|global| global.name)
        .max()
        .unwrap();
    let wl_output = {
        let id = shell.new_id("wl_output");
        shell
            .request(
                2,
                fulcrum_wire::protocol::wayland::wl_registry::Request::Bind {
                    name: output,
                    id: fulcrum_wire::DynamicNewId {
                        interface: "wl_output".into(),
                        version: 4,
                        id: id.0,
                    },
                },
            )
            .unwrap();
        id.0
    };
    let layer_shell = shell.bind(zwlr_layer_shell_v1::NAME, 4).unwrap();
    let surface = shell.create_surface().unwrap();
    let id = shell.new_id(zwlr_layer_surface_v1::NAME);
    shell
        .request(
            layer_shell,
            zwlr_layer_shell_v1::Request::GetLayerSurface {
                id,
                surface: fulcrum_wire::ObjectId(surface),
                output: Some(fulcrum_wire::ObjectId(wl_output)),
                layer: TOP_LAYER,
                namespace: "second-bar".into(),
            },
        )
        .unwrap();
    shell
        .request(id.0, zwlr_layer_surface_v1::Request::SetSize { width: 10, height: 10 })
        .unwrap();
    shell.request(surface, wl_surface::Request::Commit).unwrap();
    shell.roundtrip().unwrap();

    compositor.with(move |state| state.remove_output(second));
    shell.roundtrip().unwrap();
    assert!(shell.take_event(id.0, "closed").is_some());
}

/// An on-demand layer takes the keyboard on a click. A layer that takes no
/// keys does not.
#[test]
fn a_click_gives_the_keyboard_to_a_layer_only_if_the_layer_takes_keys() {
    use crowbar_probe::KeyEvent;
    const KEY_A: u32 = 30;
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    shell.listen_to_keyboard().unwrap();
    let mut layer = |name, anchor, keyboard| {
        shell
            .open_layer(
                LayerRequest {
                    namespace: name,
                    layer: TOP_LAYER,
                    anchor,
                    size: (0, 44),
                    exclusive_zone: 44,
                    keyboard,
                },
                0xff20_4060,
            )
            .unwrap();
    };
    layer("test-bar", TOP | LEFT | RIGHT, 0);
    layer("test-on-demand", BOTTOM | LEFT | RIGHT, ON_DEMAND);

    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    assert!(shell.take_keys().unwrap().is_empty());

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

#[test]
fn an_exclusive_overlay_takes_the_keyboard_until_it_unmaps() {
    use crowbar_probe::KeyEvent;
    const KEY_A: u32 = 30;
    let compositor = Compositor::start();
    let mut app = compositor.app();
    app.listen_to_keyboard().unwrap();
    app.open_toplevel("app", "test.app", (300, 200), 0xffc0_2020).unwrap();
    app.take_keys().unwrap();
    let mut shell = compositor.shell();
    shell.listen_to_keyboard().unwrap();
    let overlay = shell
        .open_layer(
            LayerRequest {
                namespace: "test-launcher",
                layer: OVERLAY,
                anchor: TOP | LEFT,
                size: (40, 40),
                exclusive_zone: 0,
                keyboard: EXCLUSIVE,
            },
            0xff20_4060,
        )
        .unwrap();

    let key = KeyEvent::Key {
        key: KEY_A,
        pressed: true,
    };
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    assert!(
        shell.take_keys().unwrap().contains(&key),
        "mapping did not focus the overlay"
    );
    assert_eq!(app.take_keys().unwrap(), [KeyEvent::Leave]);

    compositor.click(100, 100);
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    assert!(shell.take_keys().unwrap().contains(&key), "a click outside took focus");
    assert!(app.take_keys().unwrap().is_empty());

    shell.show(overlay.surface, None).unwrap();
    shell.roundtrip().unwrap();
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    assert_eq!(shell.take_keys().unwrap(), [KeyEvent::Leave]);
    assert!(
        app.take_keys().unwrap().contains(&key),
        "unmapping did not restore focus"
    );
}

#[test]
fn an_exclusive_overlay_precedes_a_top_layer_and_defers_new_window_focus() {
    use crowbar_probe::KeyEvent::{Enter, Leave};
    let compositor = Compositor::start();
    let mut overlay = compositor.shell();
    let mut top = compositor.shell();
    let mut app = compositor.app();
    overlay.listen_to_keyboard().unwrap();
    top.listen_to_keyboard().unwrap();
    app.listen_to_keyboard().unwrap();
    let request = LayerRequest {
        namespace: "test-exclusive",
        layer: OVERLAY,
        anchor: TOP | LEFT,
        size: (40, 40),
        exclusive_zone: 0,
        keyboard: EXCLUSIVE,
    };
    let overlay_window = overlay.open_layer(request, 0xff20_4060).unwrap();
    let top_window = top
        .open_layer(
            LayerRequest {
                layer: TOP_LAYER,
                ..request
            },
            0xff40_6020,
        )
        .unwrap();
    assert_eq!(overlay.take_keys().unwrap(), [Enter { keys: vec![] }]);
    assert!(top.take_keys().unwrap().is_empty());

    app.open_toplevel("app", "test.app", (300, 200), 0xffc0_2020).unwrap();
    assert!(overlay.take_keys().unwrap().is_empty());
    assert!(app.take_keys().unwrap().is_empty());

    overlay
        .request(overlay_window.layer_surface, zwlr_layer_surface_v1::Request::Destroy)
        .unwrap();
    assert_eq!(overlay.take_keys().unwrap(), [Leave]);
    assert_eq!(top.take_keys().unwrap(), [Enter { keys: vec![] }]);

    top.request(top_window.layer_surface, zwlr_layer_surface_v1::Request::Destroy)
        .unwrap();
    assert_eq!(top.take_keys().unwrap(), [Leave]);
    assert_eq!(app.take_keys().unwrap(), [Enter { keys: vec![] }]);
}
