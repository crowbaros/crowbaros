//! The shell's control channel, as the shell sees it: one JSON object per
//! line, each request answered by one reply.

mod common;

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

use common::{Compositor, another_uid, own_uid};
use crowbar_probe::KeyEvent;

struct Control {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Control {
    fn connect(compositor: &Compositor) -> Control {
        let stream = UnixStream::connect(compositor.dir.join("control")).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        Control {
            reader: BufReader::new(stream.try_clone().unwrap()),
            writer: stream,
        }
    }

    /// Sends one line and reads the reply.
    fn ask(&mut self, request: &str) -> String {
        self.writer.write_all(format!("{request}\n").as_bytes()).unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        line.trim_end().to_string()
    }
}

/// A compartment's client with one window, listening to its keyboard.
fn window(
    compositor: &Compositor,
    compartment: &str,
    app_id: &str,
) -> (crowbar_probe::Client, crowbar_probe::CompartmentListener) {
    let (mut client, listener) = compositor.compartment(compartment);
    client.listen_to_keyboard().unwrap();
    client
        .open_toplevel(&format!("{app_id} \"title\""), app_id, (200, 100), 0xffc0_2020)
        .unwrap();
    (client, listener)
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
        r#"{"reply":"snapshot","workspace":1,"active":null,"windows":[]}"#
    );
    let (_first, _a) = window(&compositor, "c1", "test.first");
    let (_second, _b) = window(&compositor, "c2", "test.second");

    let snapshot = control.ask(r#"{"op":"snapshot"}"#);
    let ids = ids(&snapshot);
    assert_eq!(ids.len(), 2, "{snapshot}");
    assert!(
        snapshot.contains(r#""app_id":"test.first","title":"test.first \"title\"","workspace":1"#),
        "{snapshot}"
    );
    assert!(
        snapshot.contains(r#""active":null"#),
        "a new window takes no keyboard: {snapshot}"
    );

    compositor.click(60, 60);
    let snapshot = control.ask(r#"{"op":"snapshot"}"#);
    assert!(snapshot.contains(&format!(r#""active":"{}""#, ids[0])), "{snapshot}");
}

#[test]
fn the_shell_shows_a_workspace() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let (_client, _listener) = window(&compositor, "c1", "test.one");

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
    let (mut client, _listener) = window(&compositor, "c1", "test.one");
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
    let mut control = Control::connect(&compositor);
    control.writer.write_all(b"{\"op\":\"snapshot\"}\n").ok();
    let mut line = String::new();
    let read = control.reader.read_line(&mut line).unwrap_or(0);
    assert_eq!(read, 0, "the compositor answered a peer that is not the shell: {line}");
}

#[test]
fn a_request_longer_than_the_limit_closes_the_connection() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let long = format!(
        r#"{{"op":"{}"}}"#,
        "x".repeat(crowbar_compositor::control::MAX_LINE * 2)
    );
    control.writer.write_all(long.as_bytes()).ok();
    control.writer.write_all(b"\n").ok();
    let mut line = String::new();
    let read = control.reader.read_line(&mut line).unwrap_or(0);
    assert_eq!(read, 0, "the connection stayed open: {line}");
    // The shell can connect again.
    assert!(
        Control::connect(&compositor)
            .ask(r#"{"op":"snapshot"}"#)
            .starts_with(r#"{"reply":"snapshot""#)
    );
}
