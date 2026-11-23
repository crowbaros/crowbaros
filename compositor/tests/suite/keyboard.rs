//! Who gets the keys. A new window takes the keyboard as it opens, and a
//! click gives it.

use crate::common;

use common::Compositor;
use crowbar_probe::{Client, KeyEvent};

const KEY_A: u32 = 30;

fn keys_only(events: Vec<KeyEvent>) -> Vec<(u32, bool)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            KeyEvent::Key { key, pressed } => Some((key, pressed)),
            _ => None,
        })
        .collect()
}

/// A client with a window, listening to the keyboard. The first window
/// opens at 48,48 on the test output and the second at 80,80.
fn client_with_window(compositor: &Compositor, title: &str) -> Client {
    let mut client = compositor.app();
    client.listen_to_keyboard().unwrap();
    client
        .open_toplevel(title, "test.keys", (300, 200), 0xffff_ffff)
        .unwrap();
    client
}

#[test]
fn a_new_window_takes_the_keyboard() {
    let compositor = Compositor::start();
    let mut first = client_with_window(&compositor, "c1");
    let mut second = client_with_window(&compositor, "c2");
    first.roundtrip().unwrap();

    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);

    assert!(matches!(
        first.take_keys().unwrap()[..],
        [KeyEvent::Enter { .. }, .., KeyEvent::Leave]
    ));
    let seen = second.take_keys().unwrap();
    assert!(matches!(seen[..], [KeyEvent::Enter { .. }, ..]), "{seen:?}");
    assert_eq!(keys_only(seen), [(KEY_A, true), (KEY_A, false)]);
}

#[test]
fn a_click_on_a_window_gives_it_the_keyboard() {
    let compositor = Compositor::start();
    let mut client = client_with_window(&compositor, "c1");
    let _second = client_with_window(&compositor, "c2");
    client.roundtrip().unwrap();
    client.take_keys().unwrap();

    compositor.click(60, 60);
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);

    let seen = client.take_keys().unwrap();
    assert!(matches!(seen[..], [KeyEvent::Enter { .. }, ..]), "{seen:?}");
    assert_eq!(keys_only(seen), [(KEY_A, true), (KEY_A, false)]);
}

#[test]
fn the_keys_go_to_the_window_that_was_clicked_and_to_no_other() {
    let compositor = Compositor::start();
    let mut first = client_with_window(&compositor, "c1");
    let mut second = client_with_window(&compositor, "c2");
    // The first window is at 48,48 and the second at 80,80. Each click
    // lands where only one of them is.
    compositor.click(60, 60);
    first.take_keys().unwrap();
    second.take_keys().unwrap();

    compositor.click(370, 270);
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);

    assert_eq!(first.take_keys().unwrap(), [KeyEvent::Leave]);
    assert_eq!(keys_only(second.take_keys().unwrap()), [(KEY_A, true), (KEY_A, false)]);
}

#[test]
fn a_click_on_nothing_leaves_the_keyboard_where_it_is() {
    let compositor = Compositor::start();
    let mut client = client_with_window(&compositor, "c1");
    compositor.click(60, 60);
    client.take_keys().unwrap();

    compositor.click(1200, 700);
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);

    assert_eq!(keys_only(client.take_keys().unwrap()), [(KEY_A, true), (KEY_A, false)]);
}

/// When the window with the keyboard closes, no other window is handed it.
#[test]
fn the_keyboard_of_a_window_that_closed_goes_to_no_other_window() {
    let compositor = Compositor::start();
    let mut first = client_with_window(&compositor, "c1");
    let mut second = client_with_window(&compositor, "c2");
    compositor.click(370, 270);
    assert!(matches!(second.take_keys().unwrap()[..], [KeyEvent::Enter { .. }, ..]));
    first.take_keys().unwrap();

    drop(second);
    compositor.wait_until_still();
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);

    assert_eq!(first.take_keys().unwrap(), []);
}
