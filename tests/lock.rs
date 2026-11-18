//! Lock (ADR 0020, as Fulcrum's proposal): while the session is locked, the
//! shell's lock screen is all that is drawn and all that gets input.

mod common;

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

use common::{Compositor, own_uid};
use crowbar_probe::{Client, KeyEvent, LayerRequest};

const RED: u32 = 0xffc0_2020;
const BLUE: u32 = 0xff20_20c0;
const BLACK: u32 = 0xff00_0000;
const KEY_A: u32 = 30;
const KEY_L: u32 = 38;
const KEY_2: u32 = 3;
const KEY_SUPER: u32 = 125;

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

    fn send(&mut self, request: &str) {
        self.writer.write_all(format!("{request}\n").as_bytes()).unwrap();
    }

    fn line(&mut self) -> String {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        line.trim_end().to_string()
    }
}

fn chord(compositor: &Compositor, keys: &[u32]) {
    for key in keys {
        compositor.key(*key, true);
    }
    for key in keys.iter().rev() {
        compositor.key(*key, false);
    }
}

/// The shell's lock screen over the whole of the first output, taking the
/// keyboard.
fn lock_screen(shell: &mut Client) {
    shell.listen_to_keyboard().unwrap();
    shell
        .open_layer(
            LayerRequest {
                namespace: crowbar_compositor::lock::LOCK_NAMESPACE,
                layer: 3,
                anchor: 1 | 2 | 4 | 8,
                size: (0, 0),
                exclusive_zone: -1,
                keyboard: 1,
            },
            BLUE,
        )
        .unwrap();
    shell.roundtrip().unwrap();
}

/// A compartment's window at 48,48 that has the keyboard.
fn focused_window(compositor: &Compositor) -> (Client, crowbar_probe::CompartmentListener) {
    let (mut client, listener) = compositor.compartment("c1");
    client.listen_to_keyboard().unwrap();
    client.open_toplevel("app", "test.app", (200, 100), RED).unwrap();
    compositor.click(60, 60);
    client.roundtrip().unwrap();
    client.take_keys().unwrap();
    (client, listener)
}

fn pressed(client: &mut Client) -> Vec<u32> {
    client
        .take_keys()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            KeyEvent::Key { key, pressed: true } => Some(key),
            _ => None,
        })
        .collect()
}

#[test]
fn locked_the_lock_screen_is_all_that_shows_and_all_that_hears() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let (mut client, _listener) = focused_window(&compositor);

    control.send(r#"{"op":"lock"}"#);
    assert_eq!(control.line(), r#"{"event":"locked"}"#);
    assert_eq!(control.line(), r#"{"reply":"ok"}"#);
    assert_eq!(compositor.screen(0).pixel(60, 60), Some(BLACK), "the window shows");

    let mut shell = compositor.shell();
    lock_screen(&mut shell);
    assert_eq!(compositor.screen(0).pixel(60, 60), Some(BLUE));
    shell.take_keys().unwrap();

    // Keys go to the lock screen, and a click or a chord moves nothing.
    compositor.click(60, 60);
    chord(&compositor, &[KEY_SUPER, KEY_2]);
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    shell.roundtrip().unwrap();
    client.roundtrip().unwrap();
    // Super itself reaches whoever has the keyboard, as always; the chord's
    // other key does not.
    assert_eq!(pressed(&mut shell), [KEY_SUPER, KEY_A]);
    assert!(pressed(&mut client).is_empty(), "a window heard a key while locked");
    assert_eq!(compositor.with(|state| state.workspace()), 1, "a chord acted");

    control.send(r#"{"op":"unlock"}"#);
    assert_eq!(control.line(), r#"{"event":"unlocked"}"#);
    assert_eq!(control.line(), r#"{"reply":"ok"}"#);
    // The lock screen is the shell's to take down; the window has the
    // keyboard back.
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    client.roundtrip().unwrap();
    assert_eq!(pressed(&mut client), [KEY_A]);
}

#[test]
fn super_l_locks_and_nothing_else_draws_on_another_display() {
    let compositor = Compositor::start_with(own_uid(), own_uid(), |state| {
        state.add_output("TEST-1", (640, 480), 60_000);
        state.add_output("TEST-2", (640, 480), 60_000);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
    });
    let mut control = Control::connect(&compositor);
    let (_client, _listener) = focused_window(&compositor);

    chord(&compositor, &[KEY_SUPER, KEY_L]);
    assert_eq!(control.line(), r#"{"event":"locked"}"#);
    let mut shell = compositor.shell();
    lock_screen(&mut shell);

    assert_eq!(compositor.screen(0).pixel(60, 60), Some(BLUE));
    let other = compositor.screen(1);
    assert!(
        other.pixels.iter().all(|pixel| *pixel == BLACK),
        "the other display is not blank"
    );

    // Super tapped alone does not open the launcher while locked.
    chord(&compositor, &[KEY_SUPER]);
    control.send(r#"{"op":"snapshot"}"#);
    let reply = control.line();
    assert!(reply.starts_with(r#"{"reply":"snapshot""#), "{reply}");
    assert!(reply.contains(r#""locked":true"#), "{reply}");
}

#[test]
fn the_session_locks_itself_when_idle_unless_a_shown_surface_inhibits_it() {
    let compositor = Compositor::start();
    compositor.with(|state| state.lock_after_idle(Some(Duration::from_millis(100))));
    std::thread::sleep(Duration::from_millis(400));
    assert!(compositor.with(|state| state.locked()), "idle did not lock");

    compositor.with(|state| state.unlock());
    let (mut client, _listener) = compositor.compartment("c1");
    let window = client.open_toplevel("video", "test.video", (200, 100), RED).unwrap();
    let manager = client
        .bind(
            fulcrum_wire::protocol::idle_inhibit_unstable_v1::zwp_idle_inhibit_manager_v1::NAME,
            1,
        )
        .unwrap();
    let inhibitor = client.new_id(fulcrum_wire::protocol::idle_inhibit_unstable_v1::zwp_idle_inhibitor_v1::NAME);
    client
        .request(
            manager,
            fulcrum_wire::protocol::idle_inhibit_unstable_v1::zwp_idle_inhibit_manager_v1::Request::CreateInhibitor {
                id: inhibitor,
                surface: fulcrum_wire::ObjectId(window.surface),
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    compositor.with(|state| state.lock_after_idle(Some(Duration::from_millis(100))));
    std::thread::sleep(Duration::from_millis(400));

    assert!(
        !compositor.with(|state| state.locked()),
        "a shown inhibitor did not hold it"
    );
}
