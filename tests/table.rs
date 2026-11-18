//! The Wayland socket as a client sees it: which globals each client is
//! offered. The lists are the rows of ADR 0020's table "What each client is
//! offered", less the globals the compositor does not create yet.

mod common;

use common::Compositor;

/// What every compartment with a Wayland listener is offered.
const COMPARTMENT: [&str; 8] = [
    "wl_compositor",
    "wl_data_device_manager",
    "wl_output",
    "wl_seat",
    "wl_shm",
    "wl_subcompositor",
    "wp_viewporter",
    "xdg_wm_base",
];

fn sorted(mut names: Vec<&'static str>) -> Vec<&'static str> {
    names.sort();
    names
}

#[test]
fn a_compartment_is_offered_what_it_needs_to_show_a_window_and_no_more() {
    let compositor = Compositor::start();
    let (client, _listener) = compositor.compartment("c1");

    assert_eq!(client.interfaces(), COMPARTMENT);
}

#[test]
fn a_compartment_whose_needs_include_the_gpu_is_also_offered_dmabuf() {
    let compositor = Compositor::start();
    let (with_gpu, _listener) = compositor.compartment_with_gpu("c1");
    let (without, _listener) = compositor.compartment("c2");

    let mut expected = COMPARTMENT.to_vec();
    expected.push("zwp_linux_dmabuf_v1");
    assert_eq!(with_gpu.interfaces(), sorted(expected));
    assert_eq!(without.interfaces(), COMPARTMENT);
}

#[test]
fn the_shell_is_also_offered_dmabuf_and_layer_shell() {
    let compositor = Compositor::start();
    let shell = compositor.shell();

    let mut expected = COMPARTMENT.to_vec();
    expected.extend(["zwp_linux_dmabuf_v1", "zwlr_layer_shell_v1"]);
    assert_eq!(shell.interfaces(), sorted(expected));
}

#[test]
fn the_warden_is_offered_the_security_context_manager_and_nothing_else() {
    let compositor = Compositor::start();
    let warden = compositor.warden();

    assert_eq!(warden.interfaces(), ["wp_security_context_manager_v1"]);
}

/// A second output is a new global. It goes to the clients the table gives
/// outputs to, and not to the Warden.
#[test]
fn a_global_made_while_clients_are_connected_still_passes_the_table() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let mut warden = compositor.warden();

    compositor.with(|state| {
        state.add_output("TEST-2", (800, 600), 60_000);
    });
    client.roundtrip().unwrap();
    warden.roundtrip().unwrap();

    let outputs = |names: Vec<String>| names.iter().filter(|name| *name == "wl_output").count();
    assert_eq!(outputs(client.interfaces()), 2);
    assert_eq!(warden.interfaces(), ["wp_security_context_manager_v1"]);
}
