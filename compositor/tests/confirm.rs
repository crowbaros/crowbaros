//! The one function every key enters by, whichever backend it came from.
//!
//! The confirm chord in this spike is Ctrl+Alt+Enter. ADR 0014 says the
//! confirm gesture is never delivered to a Tool or an app.

mod common;

use common::Compositor;
use crowbar_compositor::probe::{Client, KeyEvent};

const KEY_ENTER: u32 = 28;
const KEY_LEFTCTRL: u32 = 29;
const KEY_A: u32 = 30;
const KEY_LEFTALT: u32 = 56;

/// An ordinary client with a mapped window and the keyboard focus.
fn focused_client(compositor: &Compositor) -> Client {
    let mut client = compositor.ordinary_client();
    client.listen_to_keyboard().unwrap();
    client
        .open_toplevel("keys", "test.keys", (300, 200), 0xffff_ffff)
        .unwrap();
    let seen = client.take_keys().unwrap();
    assert!(
        seen.iter().any(|e| matches!(e, KeyEvent::Enter { .. })),
        "the test client did not get the keyboard focus: {seen:?}"
    );
    client
}

fn keys_only(events: Vec<KeyEvent>) -> Vec<(u32, bool)> {
    events
        .into_iter()
        .filter_map(|e| match e {
            KeyEvent::Key { key, pressed } => Some((key, pressed)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_plain_key_reaches_the_focused_client() {
    let compositor = Compositor::start();
    let mut client = focused_client(&compositor);

    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);

    assert_eq!(keys_only(client.take_keys().unwrap()), [(KEY_A, true), (KEY_A, false)]);
}

#[test]
fn the_confirm_chord_never_reaches_the_focused_client() {
    let compositor = Compositor::start();
    let mut client = focused_client(&compositor);

    compositor.key(KEY_LEFTCTRL, true);
    compositor.key(KEY_LEFTALT, true);
    compositor.key(KEY_ENTER, true);
    compositor.key(KEY_ENTER, false);
    compositor.key(KEY_LEFTALT, false);
    compositor.key(KEY_LEFTCTRL, false);

    // The client had the keyboard the whole time: it saw the modifiers. It
    // saw neither the press nor the release of the chord's key.
    assert_eq!(
        keys_only(client.take_keys().unwrap()),
        [
            (KEY_LEFTCTRL, true),
            (KEY_LEFTALT, true),
            (KEY_LEFTALT, false),
            (KEY_LEFTCTRL, false),
        ]
    );
    assert_eq!(compositor.with(|state| state.confirms_taken), 1);
}

#[test]
fn the_release_is_withheld_even_after_the_modifiers_are_let_go() {
    let compositor = Compositor::start();
    let mut client = focused_client(&compositor);

    compositor.key(KEY_LEFTCTRL, true);
    compositor.key(KEY_LEFTALT, true);
    compositor.key(KEY_ENTER, true);
    compositor.key(KEY_LEFTCTRL, false);
    compositor.key(KEY_LEFTALT, false);
    compositor.key(KEY_ENTER, false);

    assert_eq!(
        keys_only(client.take_keys().unwrap()),
        [
            (KEY_LEFTCTRL, true),
            (KEY_LEFTALT, true),
            (KEY_LEFTCTRL, false),
            (KEY_LEFTALT, false),
        ]
    );
}

/// A release can be lost, for example when the keyboard is unplugged while
/// the key is held. The next press of that key is then an ordinary key, and
/// its release must reach the client.
#[test]
fn a_withheld_key_whose_release_was_lost_is_an_ordinary_key_the_next_time() {
    let compositor = Compositor::start();
    let mut client = focused_client(&compositor);

    compositor.key(KEY_LEFTCTRL, true);
    compositor.key(KEY_LEFTALT, true);
    compositor.key(KEY_ENTER, true);
    compositor.key(KEY_LEFTCTRL, false);
    compositor.key(KEY_LEFTALT, false);
    // No release of Enter arrives.
    client.take_keys().unwrap();

    compositor.key(KEY_ENTER, true);
    compositor.key(KEY_ENTER, false);

    assert_eq!(
        keys_only(client.take_keys().unwrap()),
        [(KEY_ENTER, true), (KEY_ENTER, false)]
    );
}

#[test]
fn enter_without_the_modifiers_is_an_ordinary_key() {
    let compositor = Compositor::start();
    let mut client = focused_client(&compositor);

    compositor.key(KEY_ENTER, true);
    compositor.key(KEY_ENTER, false);

    assert_eq!(
        keys_only(client.take_keys().unwrap()),
        [(KEY_ENTER, true), (KEY_ENTER, false)]
    );
    assert_eq!(compositor.with(|state| state.confirms_taken), 0);
}

#[test]
fn a_client_that_gains_focus_during_the_chord_is_not_told_the_key_is_down() {
    let compositor = Compositor::start();
    let _first = focused_client(&compositor);

    compositor.key(KEY_LEFTCTRL, true);
    compositor.key(KEY_LEFTALT, true);
    compositor.key(KEY_ENTER, true);
    let mut second = compositor.ordinary_client();
    second.listen_to_keyboard().unwrap();
    second
        .open_toplevel("late", "test.late", (300, 200), 0xffff_ffff)
        .unwrap();
    compositor.key(KEY_ENTER, false);

    let seen = second.take_keys().unwrap();
    let held: Vec<u32> = seen
        .iter()
        .filter_map(|e| match e {
            KeyEvent::Enter { held } => Some(held.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(seen.iter().any(|e| matches!(e, KeyEvent::Enter { .. })), "{seen:?}");
    assert!(!held.contains(&KEY_ENTER), "{seen:?}");
    assert_eq!(keys_only(seen), []);
}

/// A client may ask the seat for a keyboard object whenever it likes. The new
/// object is told which keys are down.
#[test]
fn a_keyboard_object_made_during_the_chord_is_not_told_the_key_is_down() {
    let compositor = Compositor::start();
    let mut client = focused_client(&compositor);

    compositor.key(KEY_LEFTCTRL, true);
    compositor.key(KEY_LEFTALT, true);
    compositor.key(KEY_ENTER, true);
    client.take_keys().unwrap();
    client.new_keyboard().unwrap();
    compositor.key(KEY_ENTER, false);

    let seen = client.take_keys().unwrap();
    let held: Vec<u32> = seen
        .iter()
        .filter_map(|e| match e {
            KeyEvent::Enter { held } => Some(held.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(seen.iter().any(|e| matches!(e, KeyEvent::Enter { .. })), "{seen:?}");
    assert!(!held.contains(&KEY_ENTER), "{seen:?}");
    assert_eq!(keys_only(seen), []);
}
