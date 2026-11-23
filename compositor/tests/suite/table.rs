//! The Wayland sockets as a client sees them: which of the compositor's
//! globals each kind of client is offered (ADR 0032). An app is offered
//! every one but layer shell; the shell is offered them all.

use crate::common;

use common::Compositor;

/// What every client is offered: every global the test compositor makes
/// but layer shell. The test compositor names a GPU, so linux-dmabuf is among them; explicit sync
/// needs a real render node.
const EVERY_CLIENT: [&str; 14] = [
    "wl_compositor",
    "wl_data_device_manager",
    "wl_output",
    "wl_seat",
    "wl_shm",
    "wl_subcompositor",
    "wp_cursor_shape_manager_v1",
    "wp_fractional_scale_manager_v1",
    "wp_presentation",
    "wp_viewporter",
    "xdg_wm_base",
    "zwp_idle_inhibit_manager_v1",
    "zwp_linux_dmabuf_v1",
    "zwp_primary_selection_device_manager_v1",
];

fn sorted(mut names: Vec<&'static str>) -> Vec<&'static str> {
    names.sort();
    names
}

#[test]
fn an_app_is_offered_what_a_wayland_app_expects_the_gpus_buffers_included() {
    let compositor = Compositor::start();
    let client = compositor.app();

    assert_eq!(client.interfaces(), EVERY_CLIENT);
}

#[test]
fn the_shell_is_also_offered_layer_shell() {
    let compositor = Compositor::start();
    let shell = compositor.shell();

    let mut expected = EVERY_CLIENT.to_vec();
    expected.push("zwlr_layer_shell_v1");
    assert_eq!(shell.interfaces(), sorted(expected));
}

/// A second output is a new global. It goes to every client the policy
/// offers outputs to, as it is made.
#[test]
fn a_global_made_while_clients_are_connected_still_passes_the_table() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let mut shell = compositor.shell();

    compositor.with(|state| {
        state.add_output("TEST-2", (800, 600), 60_000);
    });
    client.roundtrip().unwrap();
    shell.roundtrip().unwrap();

    let outputs = |names: Vec<String>| names.iter().filter(|name| *name == "wl_output").count();
    assert_eq!(outputs(client.interfaces()), 2);
    assert_eq!(outputs(shell.interfaces()), 2);
    assert!(!client.interfaces().iter().any(|name| name == "zwlr_layer_shell_v1"));
}
