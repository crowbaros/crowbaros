//! The Harness's socket on Fulcrum (ADR 0035): shared framing, person's UID
//! alone, a new connection replacing the old one, and the snapshot that serves
//! as desktop context.

use std::{io::Write, time::Duration};

use crowbar_compositor::agent::AGENT_NAMESPACE;
use crowbar_probe::{KeyEvent, LayerRequest};
use fulcrum_wire::protocol::wlr_layer_shell_unstable_v1::zwlr_layer_surface_v1::{
    anchor::{BOTTOM, RIGHT, TOP},
    keyboard_interactivity::ON_DEMAND,
};

use crate::common::{
    Compositor, OUTPUT_SIZE, another_uid,
    control::{Control, VERSION},
    own_uid,
};

const OVERLAY: u32 = 3;

fn agent_connect(compositor: &Compositor) -> Control {
    Control::at(&compositor.dir.join("agent"))
}

fn open_window(compositor: &Compositor, app_id: &str, title: &str) -> crowbar_probe::Client {
    let mut client = compositor.app();
    client.listen_to_keyboard().unwrap();
    client.open_toplevel(title, app_id, (200, 100), 0xffc0_2020).unwrap();
    client
}

#[test]
fn the_harness_socket_speaks_shared_framing_and_refuses_bad_versions() {
    let compositor = Compositor::start();
    for bad in [
        r#"{"op":"snapshot"}"#,
        r#"{"version":2}"#,
        r#"{"version":0}"#,
        r#"{"version":"1"}"#,
        r#"{"version":1,"op":"snapshot"}"#,
        "not json",
        "",
    ] {
        let mut peer = Control::silent(&compositor.dir.join("agent"));
        peer.send(bad);
        assert!(peer.closed_without_a_word(), "{bad:?} was not refused");
    }

    // Oversized frame closes the connection as soon as its length is read.
    let mut too_long = Control::silent(&compositor.dir.join("agent"));
    too_long.stream.write_all(&[0, 0, 0x10, 0x01]).unwrap();
    assert!(too_long.closed_without_a_word());

    // A valid peer gets version 1 and answers snapshot.
    let mut agent = agent_connect(&compositor);
    let reply = agent.ask(r#"{"op":"snapshot"}"#);
    assert!(reply.starts_with(r#"{"reply":"snapshot","#), "{reply}");
}

#[test]
fn the_harness_socket_refuses_any_user_id_but_the_persons() {
    // Our process has the shell's user ID, not the person's (apps_uid).
    let compositor = Compositor::start_trusting(own_uid(), another_uid());
    let mut peer = Control::silent(&compositor.dir.join("agent"));
    let _ = peer.try_send(VERSION);
    assert!(
        peer.closed_without_a_word(),
        "a peer without the person's user ID was answered on the Harness's socket"
    );
}

#[test]
fn a_new_harness_connection_replaces_the_old_one() {
    let compositor = Compositor::start();
    let mut first = agent_connect(&compositor);
    assert!(first.ask(r#"{"op":"snapshot"}"#).starts_with(r#"{"reply":"snapshot","#));

    // Connecting a second peer from the person's UID replaces the first.
    let mut second = agent_connect(&compositor);
    assert!(
        first.closed_without_a_word(),
        "the old Harness connection was not closed when a new one connected"
    );
    assert!(
        second
            .ask(r#"{"op":"snapshot"}"#)
            .starts_with(r#"{"reply":"snapshot","#)
    );
}

#[test]
fn the_harness_snapshot_lists_active_window_windows_by_workspace_and_displays() {
    let compositor = Compositor::start_with(own_uid(), own_uid(), |state| {
        state.add_output("DP-1", OUTPUT_SIZE, 60_000);
        state.add_output("HDMI-A-1", (1920, 1080), 60_000);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
    });
    let mut shell = Control::connect(&compositor);
    let mut agent = agent_connect(&compositor);

    let _first = open_window(&compositor, "foot", "~");
    assert_eq!(shell.ask(r#"{"op":"focus_workspace","id":2}"#), r#"{"reply":"ok"}"#);
    let _second = open_window(&compositor, "com.crowbaros.StudioSettings", "Studio Settings");

    let snap = agent.ask(r#"{"op":"snapshot"}"#);
    assert!(snap.starts_with(r#"{"reply":"snapshot","workspace":2,"#), "{snap}");
    assert!(snap.contains(r#""locked":false"#), "{snap}");
    assert!(snap.contains(r#""app_id":"foot","title":"~","workspace":1"#), "{snap}");
    assert!(
        snap.contains(r#""app_id":"com.crowbaros.StudioSettings","title":"Studio Settings","workspace":2"#),
        "{snap}"
    );
    // Active points to the window on workspace 2 with the keyboard.
    assert!(!snap.contains(r#""active":null"#), "{snap}");
    // Displays are included.
    assert!(snap.contains(r#""name":"DP-1","width":1280,"height":800,"#), "{snap}");
    assert!(
        snap.contains(r#""name":"HDMI-A-1","width":1920,"height":1080,"#),
        "{snap}"
    );
    // No shell events are pushed to the Harness socket.
    assert_eq!(agent.frame_within(Duration::from_millis(150)), None);
}

#[test]
fn the_harness_snapshot_keeps_the_active_window_while_the_agents_surface_has_the_keyboard() {
    let compositor = Compositor::start();
    let mut shell_control = Control::connect(&compositor);
    let mut agent = agent_connect(&compositor);

    let _foot = open_window(&compositor, "foot", "work-terminal");
    let before = agent.ask(r#"{"op":"snapshot"}"#);
    let active_id = before
        .split(r#""active":""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("foot should be active before summoning")
        .to_string();

    // Open the Agent's surface and give it the keyboard, as the shell does on Super+Space.
    let mut shell = compositor.shell();
    shell.listen_to_keyboard().unwrap();
    let _drawer = shell
        .open_layer(
            LayerRequest {
                layer: OVERLAY,
                namespace: AGENT_NAMESPACE,
                size: (400, 0),
                anchor: TOP | BOTTOM | RIGHT,
                exclusive_zone: -1,
                keyboard: ON_DEMAND,
            },
            0xff10_2030,
        )
        .unwrap();
    assert_eq!(shell_control.ask(r#"{"op":"focus_agent"}"#), r#"{"reply":"ok"}"#);
    shell.roundtrip().unwrap();
    assert!(
        shell
            .take_keys()
            .unwrap()
            .into_iter()
            .any(|e| matches!(e, KeyEvent::Enter { .. })),
        "Agent's surface did not take the keyboard"
    );

    // The shell's snapshot has active:null (the drawer holds focus), while
    // the Harness's snapshot still names foot as the window with the keyboard.
    let shell_snap = shell_control.ask(r#"{"op":"snapshot"}"#);
    assert!(shell_snap.contains(r#""active":null"#), "{shell_snap}");
    let agent_snap = agent.ask(r#"{"op":"snapshot"}"#);
    assert!(
        agent_snap.contains(&format!(r#""active":"{active_id}""#)),
        "expected {active_id} to stay active while the drawer has focus: {agent_snap}"
    );
}

#[test]
fn the_harness_socket_refuses_unknown_or_shell_only_requests_without_closing() {
    let compositor = Compositor::start();
    let mut agent = agent_connect(&compositor);
    for request in [
        r#"{"op":"unlock"}"#,
        r#"{"op":"lock"}"#,
        r#"{"op":"focus_workspace","id":2}"#,
        r#"{"op":"snapshot","extra":1}"#,
        r#"{"op":"bogus"}"#,
    ] {
        let reply = agent.ask(request);
        assert!(reply.starts_with(r#"{"reply":"error","#), "{request} -> {reply}");
    }
    // Channel stays open and serves snapshot.
    assert!(agent.ask(r#"{"op":"snapshot"}"#).starts_with(r#"{"reply":"snapshot","#));
}

#[test]
fn the_shell_snapshot_reports_when_a_window_is_fullscreen() {
    const KEY_LEFTMETA: u32 = 125;
    const KEY_F: u32 = 33;

    let compositor = Compositor::start();
    let mut shell = Control::connect(&compositor);
    let _win = open_window(&compositor, "foot", "~");

    let before = shell.ask(r#"{"op":"snapshot"}"#);
    assert!(before.contains(r#""fullscreen":false"#), "{before}");

    // Super+F makes the active window fullscreen and pushes a snapshot event.
    compositor.key(KEY_LEFTMETA, true);
    compositor.key(KEY_F, true);
    compositor.key(KEY_F, false);
    compositor.key(KEY_LEFTMETA, false);

    let after = shell.ask(r#"{"op":"snapshot"}"#);
    assert!(after.contains(r#""fullscreen":true"#), "{after}");

    // Super+F again leaves fullscreen.
    compositor.key(KEY_LEFTMETA, true);
    compositor.key(KEY_F, true);
    compositor.key(KEY_F, false);
    compositor.key(KEY_LEFTMETA, false);

    let restored = shell.ask(r#"{"op":"snapshot"}"#);
    assert!(restored.contains(r#""fullscreen":false"#), "{restored}");
}
