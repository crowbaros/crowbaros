//! Idle inhibit: a surface that is shown keeps its output from going idle,
//! and one that is not shown does not.

mod common;

use common::Compositor;
use crowbar_probe::Client;
use fulcrum_wire::{
    ObjectId,
    protocol::{
        idle_inhibit_unstable_v1::{zwp_idle_inhibit_manager_v1, zwp_idle_inhibitor_v1},
        wayland::wl_surface,
    },
};

const KEY_SUPER: u32 = 125;
const KEY_1: u32 = 2;

fn chord(compositor: &Compositor, keys: &[u32]) {
    for key in keys {
        compositor.key(*key, true);
    }
    for key in keys.iter().rev() {
        compositor.key(*key, false);
    }
}

/// An inhibitor for a surface.
fn inhibit(client: &mut Client, surface: u32) -> u32 {
    let manager = client.bind(zwp_idle_inhibit_manager_v1::NAME, 1).unwrap();
    let inhibitor = client.new_id(zwp_idle_inhibitor_v1::NAME);
    client
        .request(
            manager,
            zwp_idle_inhibit_manager_v1::Request::CreateInhibitor {
                id: inhibitor,
                surface: ObjectId(surface),
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    inhibitor.0
}

fn inhibited(compositor: &Compositor) -> Vec<u64> {
    compositor.with(|state| state.idle_inhibited_outputs())
}

fn first_output(compositor: &Compositor) -> u64 {
    compositor.with(|state| state.outputs()[0].number)
}

#[test]
fn a_shown_window_keeps_its_output_from_idling_until_its_inhibitor_goes() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let window = client
        .open_toplevel("video", "test.video", (200, 100), 0xff80_0000)
        .unwrap();
    assert!(inhibited(&compositor).is_empty());

    let inhibitor = inhibit(&mut client, window.surface);
    assert_eq!(inhibited(&compositor), [first_output(&compositor)]);

    client
        .request(inhibitor, zwp_idle_inhibitor_v1::Request::Destroy)
        .unwrap();
    client.roundtrip().unwrap();
    assert!(inhibited(&compositor).is_empty());
}

#[test]
fn a_window_on_another_workspace_inhibits_nothing_until_it_shows_again() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let window = client
        .open_toplevel("video", "test.video", (200, 100), 0xff80_0000)
        .unwrap();
    inhibit(&mut client, window.surface);

    chord(&compositor, &[KEY_SUPER, KEY_1 + 1]);
    assert!(inhibited(&compositor).is_empty(), "a hidden window inhibits");

    chord(&compositor, &[KEY_SUPER, KEY_1]);
    assert_eq!(inhibited(&compositor), [first_output(&compositor)]);
}

#[test]
fn a_surface_never_shown_or_gone_inhibits_nothing() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let surface = client.create_surface().unwrap();
    inhibit(&mut client, surface);
    assert!(inhibited(&compositor).is_empty(), "a surface with no role inhibits");

    let window = client
        .open_toplevel("video", "test.video", (200, 100), 0xff80_0000)
        .unwrap();
    inhibit(&mut client, window.surface);
    client
        .request(
            window.toplevel,
            fulcrum_wire::protocol::xdg_shell::xdg_toplevel::Request::Destroy,
        )
        .unwrap();
    client
        .request(
            window.xdg_surface,
            fulcrum_wire::protocol::xdg_shell::xdg_surface::Request::Destroy,
        )
        .unwrap();
    client.request(window.surface, wl_surface::Request::Destroy).unwrap();
    client.roundtrip().unwrap();
    assert!(inhibited(&compositor).is_empty(), "a destroyed surface inhibits");
}

#[test]
fn a_client_that_goes_takes_its_inhibitors_with_it() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let window = client
        .open_toplevel("video", "test.video", (200, 100), 0xff80_0000)
        .unwrap();
    inhibit(&mut client, window.surface);
    assert!(!inhibited(&compositor).is_empty());

    drop(client);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while compositor.client_count() > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert!(!compositor.with(|state| state.idle_inhibited()));
    assert_eq!(
        compositor.with(|state| state.idle_inhibitor_count()),
        0,
        "the inhibitor was kept"
    );
}
