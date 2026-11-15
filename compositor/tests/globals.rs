//! Seam 1: the Wayland socket as a client sees it.

mod common;

use common::Compositor;
use crowbar_compositor::probe::ZwlrLayerShellV1;

const ORDINARY: [&str; 11] = [
    "wl_compositor",
    "wl_data_device_manager",
    "wl_output",
    "wl_seat",
    "wl_shm",
    "wl_subcompositor",
    "wp_fractional_scale_manager_v1",
    "wp_security_context_manager_v1",
    "wp_viewporter",
    "xdg_wm_base",
    "zxdg_output_manager_v1",
];

#[test]
fn an_ordinary_client_sees_only_the_ordinary_globals() {
    let compositor = Compositor::start();
    let client = compositor.ordinary_client();

    assert_eq!(client.interfaces(), ORDINARY);
}

#[test]
fn the_shell_sees_layer_shell_and_an_ordinary_client_does_not() {
    let compositor = Compositor::start();
    let shell = compositor.shell_client();
    let ordinary = compositor.ordinary_client();

    let mut expected = ORDINARY.to_vec();
    expected.push("zwlr_layer_shell_v1");
    expected.sort();
    assert_eq!(shell.interfaces(), expected);
    assert_eq!(ordinary.interfaces(), ORDINARY);
}

#[test]
fn a_client_started_through_security_context_sees_the_reduced_set() {
    let compositor = Compositor::start();
    let (sandboxed, _sandbox) = compositor.sandboxed_client();

    assert_eq!(
        sandboxed.interfaces(),
        [
            "wl_compositor",
            "wl_output",
            "wl_seat",
            "wl_shm",
            "wl_subcompositor",
            "wp_fractional_scale_manager_v1",
            "wp_viewporter",
            "xdg_wm_base",
            "zxdg_output_manager_v1",
        ]
    );
}

#[test]
fn binding_a_hidden_global_by_its_number_disconnects_the_client() {
    let compositor = Compositor::start();
    let shell = compositor.shell_client();
    let layer_shell = shell
        .state
        .globals
        .iter()
        .find(|g| g.interface == "zwlr_layer_shell_v1")
        .unwrap();
    let mut ordinary = compositor.ordinary_client();

    let _stolen: ZwlrLayerShellV1 = ordinary.bind_by_name(layer_shell.name, 1);

    assert!(ordinary.roundtrip().is_err());
}
