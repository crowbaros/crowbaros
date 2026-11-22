//! The shell's control channel, as the shell sees it: each message a frame,
//! the version stated first, each request answered by one reply, and events
//! between them.

mod common;

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
};

use common::{
    Compositor, another_uid,
    control::{Control, VERSION},
    own_uid,
};
use crowbar_probe::KeyEvent;

/// A message is a frame: its length in four bytes, the most significant
/// first, and then the JSON. The first frame each way states the version.
#[test]
fn each_message_is_a_length_and_then_json_and_the_first_states_the_version() {
    let compositor = Compositor::start();
    let mut stream = UnixStream::connect(compositor.dir.join("control")).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();

    stream.write_all(b"\0\0\0\x0d{\"version\":1}").unwrap();
    let mut answer = [0u8; 17];
    stream.read_exact(&mut answer).unwrap();
    assert_eq!(&answer, b"\0\0\0\x0d{\"version\":1}");

    // Before anything is asked, the shell is told the snapshot.
    let told = br#"{"event":"snapshot","workspace":1,"active":null,"locked":false,"windows":[]}"#;
    let mut event = vec![0u8; 4 + told.len()];
    stream.read_exact(&mut event).unwrap();
    assert_eq!(event[..4], [0, 0, 0, 76]);
    assert_eq!(&event[4..], told);

    stream.write_all(b"\0\0\0\x12{\"op\":\"launching\"}").unwrap();
    let mut reply = [0u8; 18];
    stream.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"\0\0\0\x0e{\"reply\":\"ok\"}");
}

/// A peer whose first frame does not state the version, or states one the
/// compositor does not speak, is closed, and is sent nothing first.
#[test]
fn a_peer_that_states_no_version_or_one_fulcrum_does_not_speak_is_closed() {
    let compositor = Compositor::start();
    for first in [
        r#"{"op":"snapshot"}"#,
        r#"{"version":2}"#,
        r#"{"version":0}"#,
        r#"{"version":"1"}"#,
        r#"{"version":1,"op":"snapshot"}"#,
        r#"[1]"#,
        "version 1",
        "",
    ] {
        let mut control = Control::silent(&compositor.dir.join("control"));
        control.send(first);
        assert_eq!(control.frame(), None, "{first:?} was answered");
    }
    // One line of JSON, as the channel spoke before, is no frame: its first
    // four bytes are a length far past the limit.
    let mut control = Control::silent(&compositor.dir.join("control"));
    control.stream.write_all(b"{\"op\":\"snapshot\"}\n").unwrap();
    assert_eq!(control.frame(), None, "a line was answered");

    // The shell can connect again.
    assert!(
        Control::connect(&compositor)
            .ask(r#"{"op":"snapshot"}"#)
            .starts_with(r#"{"reply":"snapshot""#)
    );
}

/// Until it states the version a peer is sent nothing, events included.
#[test]
fn a_peer_is_sent_nothing_until_it_states_the_version() {
    let compositor = Compositor::start();
    let mut control = Control::silent(&compositor.dir.join("control"));
    let _client = window(&compositor, "test.early");
    compositor.key(KEY_LEFTMETA, true);
    compositor.key(KEY_LEFTMETA, false);
    assert_eq!(
        control.frame_within(std::time::Duration::from_millis(300)),
        None,
        "a peer that stated no version was sent something"
    );

    control.send(VERSION);
    assert_eq!(control.frame().as_deref(), Some(VERSION));
    // The snapshot as it is now, and not the launcher event it missed.
    let told = control.frame().unwrap();
    assert!(told.starts_with(r#"{"event":"snapshot","#), "{told}");
    assert!(told.contains(r#""app_id":"test.early""#), "{told}");
    assert_eq!(control.ask(r#"{"op":"launching"}"#), r#"{"reply":"ok"}"#);
}

/// A socket carries bytes, not frames: a frame may come a byte at a time,
/// and several may come at once.
#[test]
fn a_frame_may_come_in_pieces_and_several_frames_at_once() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    assert!(control.frame().unwrap().starts_with(r#"{"event":"snapshot","#));

    for byte in b"\0\0\0\x12{\"op\":\"launching\"}" {
        control.stream.write_all(&[*byte]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(control.frame().as_deref(), Some(r#"{"reply":"ok"}"#));

    // Two requests and the length of a third in one write, then its rest.
    control
        .stream
        .write_all(b"\0\0\0\x12{\"op\":\"launching\"}\0\0\0\x1f{\"op\":\"focus_workspace\",\"id\":2}\0\0\0\x11")
        .unwrap();
    assert_eq!(control.message(), r#"{"reply":"ok"}"#);
    assert_eq!(control.message(), r#"{"reply":"ok"}"#);
    control.stream.write_all(br#"{"op":"snapshot"}"#).unwrap();
    assert!(control.message().starts_with(r#"{"reply":"snapshot","workspace":2,"#));
}

/// An app with one window, listening to its keyboard.
fn window(compositor: &Compositor, app_id: &str) -> crowbar_probe::Client {
    let mut client = compositor.app();
    client.listen_to_keyboard().unwrap();
    client
        .open_toplevel(&format!("{app_id} \"title\""), app_id, (200, 100), 0xffc0_2020)
        .unwrap();
    client
}

/// The window IDs a snapshot lists, in order.
fn ids(snapshot: &str) -> Vec<String> {
    snapshot
        .split("\"id\":\"")
        .skip(1)
        .map(|rest| rest.split('"').next().unwrap().to_string())
        .collect()
}

#[test]
fn a_snapshot_lists_every_window_and_the_one_with_the_keyboard() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    assert_eq!(
        control.ask(r#"{"op":"snapshot"}"#),
        r#"{"reply":"snapshot","workspace":1,"active":null,"locked":false,"windows":[]}"#
    );
    let _first = window(&compositor, "test.first");
    let _second = window(&compositor, "test.second");

    let snapshot = control.ask(r#"{"op":"snapshot"}"#);
    let ids = ids(&snapshot);
    assert_eq!(ids.len(), 2, "{snapshot}");
    assert!(
        snapshot.contains(r#""app_id":"test.first","title":"test.first \"title\"","workspace":1"#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(&format!(r#""active":"{}""#, ids[1])),
        "the newest window has the keyboard: {snapshot}"
    );

    compositor.click(60, 60);
    let snapshot = control.ask(r#"{"op":"snapshot"}"#);
    assert!(snapshot.contains(&format!(r#""active":"{}""#, ids[0])), "{snapshot}");
}

#[test]
fn the_shell_shows_a_workspace() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let _client = window(&compositor, "test.one");

    assert_eq!(control.ask(r#"{"op":"focus_workspace","id":2}"#), r#"{"reply":"ok"}"#);

    assert_eq!(compositor.with(|state| state.workspace()), 2);
    assert!(compositor.with(|state| state.window_rects()).is_empty());
    assert!(
        control
            .ask(r#"{"op":"snapshot"}"#)
            .starts_with(r#"{"reply":"snapshot","workspace":2,"#)
    );
}

#[test]
fn the_shell_focuses_a_window_on_another_workspace() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let mut client = window(&compositor, "test.one");
    let id = ids(&control.ask(r#"{"op":"snapshot"}"#)).remove(0);
    control.ask(r#"{"op":"focus_workspace","id":3}"#);
    client.take_keys().unwrap();

    assert_eq!(
        control.ask(&format!(r#"{{"op":"focus_window","id":"{id}"}}"#)),
        r#"{"reply":"ok"}"#
    );

    assert_eq!(compositor.with(|state| state.workspace()), 1);
    assert!(
        matches!(client.take_keys().unwrap()[..], [KeyEvent::Enter { .. }, ..]),
        "the window did not get the keyboard"
    );
}

#[test]
fn a_bad_request_gets_an_error_and_the_channel_goes_on() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    for request in [
        "not json",
        r#"{"op":"run","command":"rm -rf /"}"#,
        r#"{"op":"snapshot","extra":1}"#,
        r#"{"op":"focus_workspace","id":0}"#,
        r#"{"op":"focus_workspace","id":10}"#,
        r#"{"op":"focus_workspace","id":"2"}"#,
        r#"{"op":"focus_window","id":"0x999"}"#,
        r#"{"op":"focus_window","id":"window"}"#,
        r#"{"op":["snapshot"]}"#,
        r#"{"id":1}"#,
        // The version is stated once, first.
        VERSION,
        "",
    ] {
        let reply = control.ask(request);
        assert!(
            reply.starts_with(r#"{"reply":"error","message":"#),
            "{request}: {reply}"
        );
    }
    assert!(
        control
            .ask(r#"{"op":"snapshot"}"#)
            .starts_with(r#"{"reply":"snapshot""#)
    );
}

#[test]
fn the_channel_takes_the_shells_user_id_and_no_other() {
    let compositor = Compositor::start_trusting(another_uid(), own_uid());
    let mut control = Control::silent(&compositor.dir.join("control"));
    // The compositor may have closed already, and the write then fails.
    let mut frame = (VERSION.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(VERSION.as_bytes());
    control.stream.write_all(&frame).ok();
    let answer = control.frame();
    assert_eq!(answer, None, "the compositor answered a peer that is not the shell");
}

/// The length comes first, so the compositor closes on it without waiting
/// for a body it will not read.
#[test]
fn a_frame_longer_than_the_limit_closes_the_connection() {
    let compositor = Compositor::start();
    let limit = crowbar_compositor::control::MAX_FRAME;
    assert_eq!(limit, 4096);

    // A request of the limit exactly is answered.
    let mut control = Control::connect(&compositor);
    let longest = format!("{:<4096}", r#"{"op":"launching"}"#);
    assert_eq!(control.ask(&longest), r#"{"reply":"ok"}"#);

    // One byte more is not, though not a byte of the request was sent.
    control.stream.write_all(&[0, 0, 0x10, 0x01]).unwrap();
    assert!(control.closed_with_no_reply(), "the connection stayed open");

    // Nor is the longest length there is, as the first frame.
    let mut control = Control::silent(&compositor.dir.join("control"));
    control.stream.write_all(&[0xff, 0xff, 0xff, 0xff]).unwrap();
    assert!(control.closed_with_no_reply(), "the connection stayed open");

    // The shell can connect again.
    assert!(
        Control::connect(&compositor)
            .ask(r#"{"op":"snapshot"}"#)
            .starts_with(r#"{"reply":"snapshot""#)
    );
}

const KEY_LEFTMETA: u32 = 125;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_1: u32 = 2;
const KEY_2: u32 = 3;

/// The shell is told the snapshot when it connects, and again whenever it
/// changes, so the bar can show a workspace as soon as it shows (#273).
#[test]
fn the_shell_is_told_of_each_change_without_asking() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    assert_eq!(
        control.snapshot_event(),
        r#"{"event":"snapshot","workspace":1,"active":null,"locked":false,"windows":[]}"#
    );

    let _client = window(&compositor, "test.told");
    let opened = control.snapshot_event();
    let id = ids(&opened).remove(0);
    assert!(opened.contains(&format!(r#""active":"{id}""#)), "{opened}");

    // Super+Shift+2 moves the window with the keyboard to workspace 2.
    compositor.key(KEY_LEFTMETA, true);
    compositor.key(KEY_LEFTSHIFT, true);
    compositor.key(KEY_2, true);
    compositor.key(KEY_2, false);
    compositor.key(KEY_LEFTSHIFT, false);
    compositor.key(KEY_LEFTMETA, false);
    let moved = control.snapshot_event();
    assert!(moved.contains(r#""workspace":1,"active":null"#), "{moved}");
    assert!(moved.contains(r#""workspace":2,"attention""#), "{moved}");

    // Super+2 shows it.
    compositor.key(KEY_LEFTMETA, true);
    compositor.key(KEY_2, true);
    compositor.key(KEY_2, false);
    compositor.key(KEY_LEFTMETA, false);
    let shown = control.snapshot_event();
    assert!(shown.starts_with(r#"{"event":"snapshot","workspace":2,"#), "{shown}");

    // Nothing changed since: the next line is a reply, not another snapshot.
    control.send(r#"{"op":"launching"}"#);
    assert_eq!(control.frame().as_deref(), Some(r#"{"reply":"ok"}"#));
}

#[test]
fn super_tapped_alone_asks_the_shell_for_its_launcher() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);

    compositor.key(KEY_LEFTMETA, true);
    compositor.key(KEY_LEFTMETA, false);

    assert_eq!(control.message(), r#"{"event":"launcher"}"#);

    // A chord with Super, or a click with it held, is not a tap.
    compositor.key(KEY_LEFTMETA, true);
    compositor.key(KEY_1, true);
    compositor.key(KEY_1, false);
    compositor.key(KEY_LEFTMETA, false);
    compositor.key(KEY_LEFTMETA, true);
    compositor.with(|state| state.pointer_button(0x110, true, 0));
    compositor.with(|state| state.pointer_button(0x110, false, 0));
    compositor.key(KEY_LEFTMETA, false);
    let reply = control.ask(r#"{"op":"snapshot"}"#);
    assert!(
        reply.starts_with(r#"{"reply":"snapshot""#),
        "an event came first: {reply}"
    );
}

/// A window that maps while the session is locked does not get the
/// keyboard, and wants attention until it has it.
#[test]
fn a_window_that_opens_while_locked_wants_attention_until_it_has_it() {
    let compositor = Compositor::start();
    compositor.with(|state| state.lock("test"));
    let _client = window(&compositor, "test.quiet");
    compositor.with(|state| state.unlock());

    let mut control = Control::connect(&compositor);
    let snapshot = control.ask(r#"{"op":"snapshot"}"#);
    assert!(snapshot.contains(r#""active":null"#), "{snapshot}");
    assert!(snapshot.contains(r#""attention":true"#), "{snapshot}");

    compositor.click(60, 60);
    let snapshot = control.ask(r#"{"op":"snapshot"}"#);
    assert!(snapshot.contains(r#""attention":false"#), "{snapshot}");
}

/// Every new window opens above and takes the keyboard, as on any desktop,
/// whether or not the shell said it launched an app. `launching` is still
/// answered.
#[test]
fn a_new_window_takes_the_keyboard() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let _first = window(&compositor, "test.first");
    let snapshot = control.ask(r#"{"op":"snapshot"}"#);
    let first = ids(&snapshot)[0].clone();
    assert!(snapshot.contains(&format!(r#""active":"{first}""#)), "{snapshot}");

    let _second = window(&compositor, "test.second");
    assert_eq!(control.ask(r#"{"op":"launching"}"#), r#"{"reply":"ok"}"#);
    let _third = window(&compositor, "test.third");

    let snapshot = control.ask(r#"{"op":"snapshot"}"#);
    let third = ids(&snapshot)[2].clone();
    assert!(snapshot.contains(&format!(r#""active":"{third}""#)), "{snapshot}");
    assert!(!snapshot.contains(r#""attention":true"#), "{snapshot}");
}
