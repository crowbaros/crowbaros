//! Which new window takes the keyboard (ADR 0020).

mod common;

use common::Compositor;
use crowbar_probe::{Client, KeyEvent, Toplevel};
use fulcrum_wire::{ObjectId, protocol::xdg_shell::xdg_toplevel};

#[test]
fn a_dialog_of_the_window_with_the_keyboard_takes_it() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    client.listen_to_keyboard().unwrap();
    let parent = client
        .open_toplevel("app", "test.app", (300, 200), 0xffc0_2020)
        .unwrap();
    compositor.click(60, 60);
    client.take_keys().unwrap();

    // The parent is set before the dialog maps, as toolkits do.
    open_child(&mut client, &parent);

    let keys = client.take_keys().unwrap();
    assert!(
        keys.contains(&KeyEvent::Leave),
        "the parent kept the keyboard: {keys:?}"
    );
    assert!(
        keys.iter().any(|event| matches!(event, KeyEvent::Enter { .. })),
        "the dialog did not get it: {keys:?}"
    );
}

#[test]
fn a_dialog_of_a_window_without_the_keyboard_does_not_take_it() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    client.listen_to_keyboard().unwrap();
    let parent = client
        .open_toplevel("app", "test.app", (300, 200), 0xffc0_2020)
        .unwrap();
    client.take_keys().unwrap();

    open_child(&mut client, &parent);

    assert_eq!(client.take_keys().unwrap(), []);
}

/// Opens a toplevel whose parent is set before its first commit.
fn open_child(client: &mut Client, parent: &Toplevel) -> Toplevel {
    use fulcrum_wire::protocol::{
        wayland::wl_surface,
        xdg_shell::{xdg_surface, xdg_wm_base},
    };
    let wm_base = client.bind(xdg_wm_base::NAME, 6).unwrap();
    let surface = client.create_surface().unwrap();
    let xdg = client.new_id(xdg_surface::NAME);
    client
        .request(
            wm_base,
            xdg_wm_base::Request::GetXdgSurface {
                id: xdg,
                surface: ObjectId(surface),
            },
        )
        .unwrap();
    let toplevel = client.new_id(xdg_toplevel::NAME);
    client
        .request(xdg.0, xdg_surface::Request::GetToplevel { id: toplevel })
        .unwrap();
    client
        .request(
            toplevel.0,
            xdg_toplevel::Request::SetParent {
                parent: Some(ObjectId(parent.toplevel)),
            },
        )
        .unwrap();
    client.request(surface, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();
    let window = Toplevel {
        surface,
        xdg_surface: xdg.0,
        toplevel: toplevel.0,
    };
    client.redraw(&window, (100, 60), 0xff20_c020).unwrap();
    window
}
