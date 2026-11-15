//! The control channel the compositor hands to the shell.
//!
//! It replaces the apps' use of the Hyprland socket. The wire format is one
//! JSON object per line, and the commands are a fixed set.

mod common;

use common::Compositor;
use serde_json::json;

#[test]
fn a_snapshot_lists_the_windows_and_names_the_active_one() {
    let compositor = Compositor::start();
    let (_shell, mut control) = compositor.shell();
    let mut first = compositor.ordinary_client();
    first
        .open_toplevel("First window", "test.first", (300, 200), 0xffff_ffff)
        .unwrap();
    let mut second = compositor.ordinary_client();
    second
        .open_toplevel("Second \"quoted\" window", "test.second", (300, 200), 0xffff_ffff)
        .unwrap();

    let reply = control.request(r#"{"op":"snapshot"}"#);

    assert_eq!(
        reply,
        json!({
            "reply": "snapshot",
            "workspace": 1,
            "active": "0x2",
            "windows": [
                {"id": "0x1", "app_id": "test.first", "title": "First window", "workspace": 1},
                {"id": "0x2", "app_id": "test.second", "title": "Second \"quoted\" window", "workspace": 1},
            ],
        })
    );
}

#[test]
fn focus_window_moves_the_keyboard_to_that_window() {
    use crowbar_compositor::probe::KeyEvent;
    let compositor = Compositor::start();
    let (_shell, mut control) = compositor.shell();
    let mut first = compositor.ordinary_client();
    first.listen_to_keyboard().unwrap();
    first
        .open_toplevel("First", "test.first", (300, 200), 0xffff_ffff)
        .unwrap();
    let mut second = compositor.ordinary_client();
    second.listen_to_keyboard().unwrap();
    second
        .open_toplevel("Second", "test.second", (300, 200), 0xffff_ffff)
        .unwrap();
    first.take_keys().unwrap();
    second.take_keys().unwrap();

    let reply = control.request(r#"{"op":"focus_window","id":"0x1"}"#);

    assert_eq!(reply, json!({"reply": "ok"}));
    assert!(matches!(first.take_keys().unwrap()[..], [KeyEvent::Enter { .. }, ..]));
    assert_eq!(second.take_keys().unwrap(), [KeyEvent::Leave]);
    assert_eq!(control.request(r#"{"op":"snapshot"}"#)["active"], "0x1");
}

#[test]
fn focus_workspace_shows_only_that_workspaces_windows() {
    use crowbar_compositor::probe::KeyEvent;
    let compositor = Compositor::start();
    let (_shell, mut control) = compositor.shell();
    let mut first = compositor.ordinary_client();
    first.listen_to_keyboard().unwrap();
    first
        .open_toplevel("First", "test.first", (300, 200), 0xffff_ffff)
        .unwrap();
    first.take_keys().unwrap();

    assert_eq!(
        control.request(r#"{"op":"focus_workspace","id":2}"#),
        json!({"reply": "ok"})
    );

    // The window on workspace 1 is no longer shown and loses the keyboard.
    assert_eq!(first.take_keys().unwrap(), [KeyEvent::Leave]);
    assert_eq!(compositor.with(|state| state.space.elements().count()), 0);

    // A window opened now belongs to workspace 2.
    let mut second = compositor.ordinary_client();
    second
        .open_toplevel("Second", "test.second", (300, 200), 0xffff_ffff)
        .unwrap();
    let snapshot = control.request(r#"{"op":"snapshot"}"#);
    assert_eq!(snapshot["workspace"], 2);
    assert_eq!(snapshot["windows"][0]["workspace"], 1);
    assert_eq!(snapshot["windows"][1]["workspace"], 2);
    assert_eq!(snapshot["active"], "0x2");

    // Going back shows the first window again and returns the keyboard to it.
    assert_eq!(
        control.request(r#"{"op":"focus_workspace","id":1}"#),
        json!({"reply": "ok"})
    );
    assert!(matches!(first.take_keys().unwrap()[..], [KeyEvent::Enter { .. }, ..]));
    assert_eq!(control.request(r#"{"op":"snapshot"}"#)["active"], "0x1");
}

#[test]
fn a_request_outside_the_fixed_set_is_refused_and_the_channel_stays_open() {
    let compositor = Compositor::start();
    let (_shell, mut control) = compositor.shell();

    for request in [
        r#"{"op":"exec","command":"sh"}"#,
        r#"{"op":"snapshot","extra":true}"#,
        r#"{"op":"focus_workspace","id":0}"#,
        r#"{"op":"focus_window","id":"0x99"}"#,
        "eval hl.dispatch()",
    ] {
        assert_eq!(control.request(request)["reply"], "error", "{request}");
    }
    assert_eq!(control.request(r#"{"op":"snapshot"}"#)["reply"], "snapshot");
}

#[test]
fn the_shell_is_told_when_the_compositor_takes_the_confirm_chord() {
    let compositor = Compositor::start();
    let (_shell, mut control) = compositor.shell();

    compositor.key(29, true); // KEY_LEFTCTRL
    compositor.key(56, true); // KEY_LEFTALT
    compositor.key(28, true); // KEY_ENTER
    compositor.key(28, false);

    assert_eq!(control.read(), json!({"event": "confirm"}));
}
