//! Outputs as clients see them when one changes size, as when its mode
//! changes or the nested backend's window is resized.

mod common;

use common::{Compositor, OUTPUT_SIZE};
use crowbar_compositor::geometry::Rect;
use crowbar_probe::LayerRequest;
use fulcrum_wire::{
    Arg,
    protocol::{wayland::wl_output, wlr_layer_shell_unstable_v1::zwlr_layer_surface_v1},
};

use zwlr_layer_surface_v1::anchor::{LEFT, RIGHT, TOP};

#[test]
fn an_output_that_changes_size_tells_its_clients_and_places_the_layers_again() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    let output = shell.bind(wl_output::NAME, 4).unwrap();
    let bar = shell
        .open_layer(
            LayerRequest {
                namespace: "test-bar",
                layer: 2,
                anchor: TOP | LEFT | RIGHT,
                size: (0, 44),
                exclusive_zone: 44,
                keyboard: 0,
            },
            0xff20_4060,
        )
        .unwrap();
    assert_eq!(bar.size, (OUTPUT_SIZE.0 as u32, 44));
    shell.events.clear();

    compositor.with(|state| {
        let number = state.outputs()[0].number;
        state.resize_output(number, (1600, 900));
    });
    shell.roundtrip().unwrap();

    let mode = shell
        .events
        .iter()
        .rev()
        .find(|event| event.object == output && event.name == "mode")
        .map(|event| match event.args.as_slice() {
            [Arg::Uint(flags), Arg::Int(width), Arg::Int(height), Arg::Int(_)] => (*flags, *width, *height),
            other => panic!("a mode event with {other:?}"),
        });
    assert_eq!(
        mode,
        Some((wl_output::mode::CURRENT | wl_output::mode::PREFERRED, 1600, 900))
    );
    assert!(
        shell
            .events
            .iter()
            .any(|event| event.object == output && event.name == "done"),
        "the output's change was never done"
    );
    assert_eq!(shell.layer_sizes[&bar.layer_surface], (1600, 44));
    let usable = compositor.with(|state| state.usable_area(state.outputs()[0].number));
    assert_eq!(usable, Rect::new(0, 44, 1600, 900 - 44));
}

#[test]
fn the_outputs_to_the_right_of_one_that_grows_move_along() {
    let compositor = Compositor::start();

    let positions = compositor.with(|state| {
        state.add_output("TEST-2", (1024, 768), 60_000);
        let first = state.outputs()[0].number;
        state.resize_output(first, (1600, 900));
        state
            .outputs()
            .iter()
            .map(|output| (output.position, output.size))
            .collect::<Vec<_>>()
    });

    assert_eq!(positions, [((0, 0), (1600, 900)), ((1600, 0), (1024, 768))]);
}

/// A window on the part of an output that is gone comes back onto it.
#[test]
fn a_window_an_output_shrinks_away_from_comes_back_onto_it() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    for title in ["first", "second"] {
        client
            .open_toplevel(title, "test.app", (200, 100), 0xffff_ffff)
            .unwrap();
    }
    let corners = |rects: Vec<Rect>| {
        let mut corners: Vec<(i32, i32)> = rects.iter().map(|rect| (rect.x, rect.y)).collect();
        corners.sort();
        corners
    };
    let before = compositor.with(|state| state.window_rects());
    assert_eq!(corners(before), [(48, 48), (80, 80)]);

    // The second window's corner is past the smaller output's edge.
    let after = compositor.with(|state| {
        let number = state.outputs()[0].number;
        state.resize_output(number, (70, 70));
        state.window_rects()
    });

    assert_eq!(corners(after), [(48, 48), (48, 48)]);
}
