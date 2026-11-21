//! The shell's control channel, as the shell sees it: one JSON object per
//! line, each request answered by one reply, and events between them.

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

    /// Sends one line and reads the reply. The snapshots the compositor
    /// sends unasked are passed over, as the shell passes events over.
    fn ask(&mut self, request: &str) -> String {
        self.writer.write_all(format!("{request}\n").as_bytes()).unwrap();
        self.line()
    }

    /// Reads the next line the compositor sent that is not a snapshot event.
    fn line(&mut self) -> String {
        loop {
            let line = self.any_line();
            if !line.starts_with(SNAPSHOT_EVENT) {
                return line;
            }
        }
    }

    /// Reads the next snapshot event, passing over anything else.
    fn snapshot_event(&mut self) -> String {
        loop {
            let line = self.any_line();
            if line.starts_with(SNAPSHOT_EVENT) {
                return line;
            }
        }
    }

    fn any_line(&mut self) -> String {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        line.trim_end().to_string()
    }
}

const SNAPSHOT_EVENT: &str = r#"{"event":"snapshot","#;

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
    // The snapshot it was told on connecting may come first; no reply does.
    loop {
        let mut line = String::new();
        let read = control.reader.read_line(&mut line).unwrap_or(0);
        if read == 0 {
            break;
        }
        assert!(line.starts_with(SNAPSHOT_EVENT), "the connection stayed open: {line}");
    }
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
    control.writer.write_all(b"{\"op\":\"launching\"}\n").unwrap();
    assert_eq!(control.any_line(), r#"{"reply":"ok"}"#);
}

#[test]
fn super_tapped_alone_asks_the_shell_for_its_launcher() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);

    compositor.key(KEY_LEFTMETA, true);
    compositor.key(KEY_LEFTMETA, false);

    assert_eq!(control.line(), r#"{"event":"launcher"}"#);

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
