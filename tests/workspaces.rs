//! Nine workspaces, one showing at a time (ADR 0020), and the compositor's
//! Super chords that switch them.

mod common;

use common::Compositor;
use crowbar_compositor::BACKGROUND as BACKGROUND_PIXEL;
use crowbar_probe::{Client, KeyEvent};
use fulcrum_wire::protocol::wayland::wl_output;

const KEY_SUPER: u32 = 125;
const KEY_SHIFT: u32 = 42;
const KEY_1: u32 = 2;
const KEY_X: u32 = 45;
const RED: u32 = 0xffc0_2020;
/// Inside the first window, which opens at 48,48.
const INSIDE: (i32, i32) = (60, 60);

fn chord(compositor: &Compositor, keys: &[u32]) {
    for key in keys {
        compositor.key(*key, true);
    }
    for key in keys.iter().rev() {
        compositor.key(*key, false);
    }
}

fn workspace(n: u32) -> u32 {
    KEY_1 + n - 1
}

fn window(compositor: &Compositor) -> (Client, crowbar_probe::Toplevel, crowbar_probe::CompartmentListener) {
    let (mut client, listener) = compositor.compartment("c1");
    client.listen_to_keyboard().unwrap();
    // `enter` and `leave` name the client's own outputs.
    client.bind(wl_output::NAME, 4).unwrap();
    let window = client.open_toplevel("one", "test.one", (200, 100), RED).unwrap();
    (client, window, listener)
}

#[test]
fn a_window_on_a_workspace_not_showing_is_nowhere_until_it_shows_again() {
    let compositor = Compositor::start();
    let (mut client, window, _listener) = window(&compositor);
    assert_eq!(compositor.screen(0).pixel(INSIDE.0, INSIDE.1), Some(RED));

    chord(&compositor, &[KEY_SUPER, workspace(2)]);
    client.roundtrip().unwrap();

    assert_eq!(compositor.with(|state| state.workspace()), 2);
    assert_eq!(compositor.screen(0).pixel(INSIDE.0, INSIDE.1), Some(BACKGROUND_PIXEL));
    assert!(compositor.with(|state| state.window_rects()).is_empty());
    assert!(
        client.take_event(window.surface, "leave").is_some(),
        "the client was not told its window left the output"
    );

    chord(&compositor, &[KEY_SUPER, workspace(1)]);
    client.roundtrip().unwrap();

    assert_eq!(compositor.screen(0).pixel(INSIDE.0, INSIDE.1), Some(RED));
    assert!(client.take_event(window.surface, "enter").is_some());
}

#[test]
fn a_chord_with_super_never_reaches_a_client() {
    let compositor = Compositor::start();
    let (mut client, _window, _listener) = window(&compositor);
    compositor.click(INSIDE.0, INSIDE.1);
    client.take_keys().unwrap();

    chord(&compositor, &[KEY_SUPER, KEY_X]);

    let keys: Vec<(u32, bool)> = client
        .take_keys()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            KeyEvent::Key { key, pressed } => Some((key, pressed)),
            _ => None,
        })
        .collect();
    assert_eq!(keys, [(KEY_SUPER, true), (KEY_SUPER, false)], "X was sent");
}

#[test]
fn hiding_a_window_takes_the_keyboard_from_it() {
    let compositor = Compositor::start();
    let (mut client, _window, _listener) = window(&compositor);
    compositor.click(INSIDE.0, INSIDE.1);
    client.take_keys().unwrap();

    chord(&compositor, &[KEY_SUPER, workspace(3)]);

    assert!(
        client.take_keys().unwrap().contains(&KeyEvent::Leave),
        "the hidden window kept the keyboard"
    );
}

#[test]
fn super_shift_moves_the_window_with_the_keyboard_to_another_workspace() {
    let compositor = Compositor::start();
    let (mut client, _window, _listener) = window(&compositor);
    compositor.click(INSIDE.0, INSIDE.1);

    chord(&compositor, &[KEY_SUPER, KEY_SHIFT, workspace(4)]);
    client.roundtrip().unwrap();

    assert_eq!(
        compositor.with(|state| state.workspace()),
        1,
        "the workspace showing stays"
    );
    assert!(compositor.with(|state| state.window_rects()).is_empty());
    chord(&compositor, &[KEY_SUPER, workspace(4)]);
    assert_eq!(compositor.with(|state| state.window_rects()).len(), 1);
}

#[test]
fn a_new_window_opens_on_the_workspace_showing() {
    let compositor = Compositor::start();
    chord(&compositor, &[KEY_SUPER, workspace(5)]);
    let (_client, _window, _listener) = window(&compositor);

    assert_eq!(compositor.with(|state| state.window_rects()).len(), 1);
    chord(&compositor, &[KEY_SUPER, workspace(1)]);
    assert!(compositor.with(|state| state.window_rects()).is_empty());
}
