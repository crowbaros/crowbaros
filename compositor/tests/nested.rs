//! The nested backend, with Fulcrum as its own host: a compositor that shows
//! its desktop in a window on another, as a developer runs it inside their
//! session. Only in a build with the nested feature, which Base never is.
#![cfg(feature = "nested")]

mod common;

use std::time::{Duration, Instant};

use common::{Compositor, own_uid};
use crowbar_probe::KeyEvent;

const KEY_A: u32 = 30;
/// Where the host opens the nested compositor's window, and where the
/// nested compositor opens its client's.
const WINDOW_AT: i32 = 48;

/// A compositor nested in a test compositor, as a client of the host's
/// shell socket.
fn nested_in(host: &Compositor) -> Compositor {
    let socket = host.parts.shell.socket.clone();
    Compositor::start_with(own_uid(), own_uid(), move |state| {
        crowbar_compositor::backend::nested::init_at(state, &socket).unwrap();
    })
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "{what} did not happen");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_nested_desktop_is_a_window_on_the_host() {
    let host = Compositor::start();
    let nested = nested_in(&host);

    let outputs = nested.with(|state| {
        state
            .outputs()
            .iter()
            .map(|output| (output.name.clone(), output.size))
            .collect::<Vec<_>>()
    });
    // The window maps on the host with the nested compositor's first frame.
    wait_for("the nested window on the host", || {
        host.with(|state| state.window_requests().len()) == 1
    });
    let titles = host.with(|state| state.window_requests());

    // The host left the size to the nested compositor, which took its own.
    assert_eq!(outputs, [("NESTED-1".to_string(), (1280, 800))]);
    assert_eq!(
        (titles[0].title.as_str(), titles[0].app_id.as_str()),
        ("Fulcrum", "org.crowbaros.Fulcrum")
    );
}

#[test]
fn a_client_of_the_nested_compositor_shows_on_the_host_and_hears_its_keys() {
    let host = Compositor::start();
    let nested = nested_in(&host);
    let mut client = nested.app();
    client.listen_to_keyboard().unwrap();
    client
        .open_toplevel("inner", "test.inner", (300, 200), 0xff20_60a0)
        .unwrap();

    // The client's window, at 48,48 in the nested desktop, which is at
    // 48,48 on the host's.
    let inside = 2 * WINDOW_AT + 20;
    wait_for("the client's window on the host's screen", || {
        host.screen(0).pixel(inside, inside) == Some(0xff20_60a0)
    });

    // A click on it through the host gives it the nested keyboard.
    host.click(inside, inside);
    host.key(KEY_A, true);
    host.key(KEY_A, false);
    let mut keys = Vec::new();
    wait_for("the key reaching the client", || {
        keys.extend(client.take_keys().unwrap());
        keys.iter().any(|event| {
            matches!(
                event,
                KeyEvent::Key {
                    key: KEY_A,
                    pressed: false
                }
            )
        })
    });
    assert!(matches!(keys[0], KeyEvent::Enter { .. }), "{keys:?}");
}
