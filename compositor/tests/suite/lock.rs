//! Lock (ADR 0032): while the session is locked, the
//! shell's lock screen is all that is drawn and all that gets input.

use crate::common;

use std::{
    collections::HashMap,
    io::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use common::{
    Compositor, another_uid,
    control::{Control, VERSION},
    own_uid,
};
use crowbar_compositor::{
    State,
    backend::{Backend, Drawn},
    image::Image,
    lock::{LOCK_SHOWN_WITHIN, LOCKED_BACKGROUND},
};
use crowbar_probe::{Client, KeyEvent, LayerRequest};

const RED: u32 = 0xffc0_2020;
const BLUE: u32 = 0xff20_20c0;
const KEY_A: u32 = 30;
const KEY_L: u32 = 38;
const KEY_2: u32 = 3;
const KEY_SUPER: u32 = 125;
const KEY_CTRL: u32 = 29;
const KEY_ALT: u32 = 56;
const KEY_ENTER: u32 = 28;

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

/// An app's window at 48,48 that has the keyboard.
fn focused_window(compositor: &Compositor) -> Client {
    let mut client = compositor.app();
    client.listen_to_keyboard().unwrap();
    client.open_toplevel("app", "test.app", (200, 100), RED).unwrap();
    compositor.click(60, 60);
    client.roundtrip().unwrap();
    client.take_keys().unwrap();
    client
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
    let mut client = focused_window(&compositor);

    control.send(r#"{"op":"lock"}"#);
    assert_eq!(control.message(), r#"{"event":"locked"}"#);
    assert_eq!(control.message(), r#"{"reply":"ok"}"#);
    assert_eq!(
        compositor.screen(0).pixel(60, 60),
        Some(LOCKED_BACKGROUND),
        "the window shows"
    );

    let mut shell = compositor.shell();
    lock_screen(&mut shell);
    assert_eq!(compositor.screen(0).pixel(60, 60), Some(BLUE));
    shell.take_keys().unwrap();

    // A click or a chord moves nothing, and the lock screen takes text at
    // once: the person types their password with nothing first.
    compositor.click(60, 60);
    chord(&compositor, &[KEY_SUPER, KEY_2]);
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    shell.roundtrip().unwrap();
    client.roundtrip().unwrap();
    assert_eq!(pressed(&mut shell), [KEY_SUPER, KEY_A], "the lock screen took no text");
    assert!(pressed(&mut client).is_empty(), "a window heard a key while locked");
    assert_eq!(compositor.with(|state| state.workspace()), 1, "a chord acted");

    // With no unlock authority, the shell's word unlocks.
    control.send(r#"{"op":"unlock"}"#);
    assert_eq!(control.message(), r#"{"event":"unlocked"}"#);
    assert_eq!(control.message(), r#"{"reply":"ok"}"#);
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
    let _client = focused_window(&compositor);

    chord(&compositor, &[KEY_SUPER, KEY_L]);
    assert_eq!(control.message(), r#"{"event":"locked"}"#);
    let mut shell = compositor.shell();
    lock_screen(&mut shell);

    assert_eq!(compositor.screen(0).pixel(60, 60), Some(BLUE));
    let other = compositor.screen(1);
    assert!(
        other.pixels.iter().all(|pixel| *pixel == LOCKED_BACKGROUND),
        "the other display is not blank"
    );

    // Super tapped alone does not open the launcher while locked.
    chord(&compositor, &[KEY_SUPER]);
    control.send(r#"{"op":"snapshot"}"#);
    let reply = control.message();
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
    let mut client = compositor.app();
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

/// How long the idle tests leave the session before it locks itself.
const IDLE: Duration = Duration::from_millis(200);

/// Turns the loop until the session is locked.
fn turn_until_locked(state: &mut State) {
    let give_up = Instant::now() + Duration::from_secs(10);
    while !state.locked() {
        assert!(Instant::now() < give_up, "idle did not lock");
        state.turn(Some(Duration::from_secs(1))).unwrap();
    }
}

/// Whether the loop sleeps through a whole turn of twice the idle time:
/// nothing is ready, and no timer runs out.
fn sleeps(state: &mut State) -> bool {
    let started = Instant::now();
    state.turn(Some(IDLE * 2)).unwrap();
    started.elapsed() >= IDLE * 2
}

#[test]
fn the_loop_sleeps_once_the_session_locked_itself_and_unlock_starts_the_timer_again() {
    let mut state = State::new().unwrap();
    state.lock_after_idle(Some(IDLE));
    turn_until_locked(&mut state);
    assert!(sleeps(&mut state), "the loop woke while the session was locked");

    // The time without input counts from the unlock: the session was locked
    // for longer than it takes to lock itself.
    let unlocked = Instant::now();
    state.unlock();
    turn_until_locked(&mut state);
    assert!(unlocked.elapsed() >= IDLE, "the session locked before it was idle");
    assert!(
        sleeps(&mut state),
        "the loop woke after the session locked itself again"
    );
}

#[test]
fn no_idle_timer_runs_while_the_session_is_locked_by_other_means() {
    // Locked by a request, with the timer running.
    let mut state = State::new().unwrap();
    state.lock_after_idle(Some(IDLE));
    state.lock("test");
    assert!(sleeps(&mut state), "the loop woke for the timer set before the lock");
    state.unlock();
    turn_until_locked(&mut state);

    // Locked before it is told to lock when idle.
    let mut state = State::new().unwrap();
    state.lock("test");
    state.lock_after_idle(Some(IDLE));
    assert!(sleeps(&mut state), "the loop woke for a timer set while locked");
    state.unlock();
    turn_until_locked(&mut state);
}

#[test]
fn the_confirm_chord_neither_locks_nor_unlocks_and_no_client_hears_its_enter() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let mut client = focused_window(&compositor);

    // Unlocked, the chord does not lock, and the window does not hear its
    // Enter.
    chord(&compositor, &[KEY_CTRL, KEY_ALT, KEY_ENTER]);
    client.roundtrip().unwrap();
    let keys = pressed(&mut client);
    assert!(!keys.contains(&KEY_ENTER), "a client heard the chord's Enter: {keys:?}");
    assert!(!compositor.with(|state| state.locked()), "the chord locked");

    // Locked, it does not unlock, and the lock screen does not hear its
    // Enter either.
    control.send(r#"{"op":"lock"}"#);
    assert_eq!(control.message(), r#"{"event":"locked"}"#);
    assert_eq!(control.message(), r#"{"reply":"ok"}"#);
    let mut shell = compositor.shell();
    lock_screen(&mut shell);
    shell.take_keys().unwrap();
    chord(&compositor, &[KEY_CTRL, KEY_ALT, KEY_ENTER]);
    shell.roundtrip().unwrap();
    let keys = pressed(&mut shell);
    assert!(
        !keys.contains(&KEY_ENTER),
        "the lock screen heard the chord's Enter: {keys:?}"
    );
    assert!(compositor.with(|state| state.locked()), "the chord unlocked");

    // The shell hears no event of the chord: the next message answers its
    // request.
    control.send(r#"{"op":"snapshot"}"#);
    let reply = control.message();
    assert!(reply.starts_with(r#"{"reply":"snapshot""#), "{reply}");
}

/// The unlock authority, listening at `authority` in the compositor's
/// directory, from `uid`.
fn with_authority(compositor: &Compositor, uid: u32) -> std::path::PathBuf {
    let path = compositor.dir.join("authority");
    let socket = path.clone();
    compositor.with(move |state| state.listen_for_unlock_authority(&socket, uid).unwrap());
    path
}

#[test]
fn once_there_is_an_unlock_authority_only_its_word_unlocks() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let mut authority = Control::at(&with_authority(&compositor, own_uid()));
    let mut client = focused_window(&compositor);

    // The authority locks, and hears no event: its reply is all it gets.
    authority.send(r#"{"op":"lock"}"#);
    assert_eq!(authority.message(), r#"{"reply":"ok"}"#);
    assert_eq!(control.message(), r#"{"event":"locked"}"#);

    // The shell's word is not enough.
    control.send(r#"{"op":"unlock"}"#);
    let refused = control.message();
    assert!(refused.starts_with(r#"{"reply":"error""#), "{refused}");
    assert!(compositor.with(|state| state.locked()), "the shell unlocked");

    // The authority may lock and unlock, and do nothing else.
    authority.send(r#"{"op":"snapshot"}"#);
    assert!(
        authority.message().starts_with(r#"{"reply":"error""#),
        "the authority took a snapshot"
    );
    authority.send(r#"{"op":"unlock","by":"me"}"#);
    assert!(
        authority.message().starts_with(r#"{"reply":"error""#),
        "an unlock took a field"
    );

    authority.send(r#"{"op":"unlock"}"#);
    assert_eq!(authority.message(), r#"{"reply":"ok"}"#);
    assert_eq!(control.message(), r#"{"event":"unlocked"}"#);
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    client.roundtrip().unwrap();
    assert_eq!(pressed(&mut client), [KEY_A], "the window lost the keyboard");
}

#[test]
fn the_unlock_authority_takes_its_user_id_and_no_other() {
    let compositor = Compositor::start();
    let mut impostor = Control::silent(&with_authority(&compositor, another_uid()));
    compositor.with(|state| state.lock("test"));

    // The compositor may have closed already, and a write then fails.
    for message in [VERSION, r#"{"op":"unlock"}"#] {
        impostor.try_send(message).ok();
    }
    assert!(
        impostor.closed_without_a_word(),
        "the compositor answered a peer that is not the authority, or left it open"
    );
    assert!(compositor.with(|state| state.locked()), "an impostor unlocked");
}

/// The unlock authority's socket speaks as the shell's control channel
/// does: frames, the version first, and no frame past the limit.
#[test]
fn the_unlock_authority_states_the_version_first_or_is_closed() {
    let compositor = Compositor::start();
    let path = with_authority(&compositor, own_uid());
    compositor.with(|state| state.lock("test"));

    let mut authority = Control::silent(&path);
    authority.send(VERSION);
    authority.send(r#"{"op":"lock"}"#);
    assert_eq!(authority.frame().as_deref(), Some(VERSION));
    assert_eq!(authority.frame().as_deref(), Some(r#"{"reply":"ok"}"#));

    for first in [r#"{"op":"unlock"}"#, r#"{"version":2}"#] {
        let mut authority = Control::silent(&path);
        authority.send(first);
        authority.try_send(r#"{"op":"unlock"}"#).ok();
        assert!(authority.closed_without_a_word(), "{first} was answered, or left open");
    }
    // One line, as the unlock helper spoke before.
    let mut authority = Control::silent(&path);
    authority.stream.write_all(b"{\"op\":\"unlock\"}\n").unwrap();
    assert!(authority.closed_without_a_word(), "a line was answered, or left open");
    let mut authority = Control::at(&path);
    authority.stream.write_all(&[0, 0, 0x10, 0x01]).unwrap();
    assert!(
        authority.closed_without_a_word(),
        "a frame past the limit was answered, or left open"
    );
    assert!(compositor.with(|state| state.locked()), "a refused peer unlocked");

    let mut authority = Control::at(&path);
    authority.send(r#"{"op":"unlock"}"#);
    assert_eq!(authority.frame().as_deref(), Some(r#"{"reply":"ok"}"#));
    assert!(!compositor.with(|state| state.locked()));
}

#[test]
fn a_session_started_locked_shows_and_gives_nothing_before_the_lock_screen() {
    let compositor = Compositor::start_with(own_uid(), own_uid(), |state| {
        state.lock("start");
        state.add_output("TEST-1", (640, 480), 60_000);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
    });
    let mut client = compositor.app();
    client.listen_to_keyboard().unwrap();
    client.open_toplevel("app", "test.app", (200, 100), RED).unwrap();
    compositor.click(60, 60);
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    client.roundtrip().unwrap();

    assert!(pressed(&mut client).is_empty(), "a window heard a key before unlock");
    assert_eq!(
        compositor.screen(0).pixel(60, 60),
        Some(LOCKED_BACKGROUND),
        "the window shows"
    );
    let mut shell = compositor.shell();
    lock_screen(&mut shell);
    assert_eq!(compositor.screen(0).pixel(60, 60), Some(BLUE));

    // The lock screen takes the person's keys as soon as it shows.
    shell.take_keys().unwrap();
    compositor.key(KEY_A, true);
    compositor.key(KEY_A, false);
    shell.roundtrip().unwrap();
    assert_eq!(pressed(&mut shell), [KEY_A], "the lock screen took no text");
}

/// Displays that show the frame sent to them only when the test flips them,
/// as a display controller does at its next vertical blank: the frame on its
/// way, and the frame shown, by output.
#[derive(Clone, Default)]
struct Displays(Arc<Mutex<HashMap<u64, Frames>>>);

#[derive(Default)]
struct Frames {
    on_its_way: Option<Image>,
    shown: Option<Image>,
}

impl Backend for Displays {
    fn draw(&mut self, state: &State, output: u64) -> Drawn {
        let Some(image) = state.render_output(output) else {
            return Drawn::Nothing;
        };
        self.0.lock().unwrap().entry(output).or_default().on_its_way = Some(image);
        Drawn::Flip
    }
}

impl Displays {
    /// The `index`th display, or every display where `None`, shows the frame
    /// on its way to it, and the compositor hears so.
    fn flip(&self, compositor: &Compositor, index: Option<usize>) {
        let displays = self.clone();
        compositor.with(move |state| {
            let numbers: Vec<u64> = state.outputs().iter().map(|output| output.number).collect();
            for (at, number) in numbers.into_iter().enumerate() {
                if index.is_some_and(|index| index != at) {
                    continue;
                }
                let flipped = {
                    let mut frames = displays.0.lock().unwrap();
                    let frame = frames.entry(number).or_default();
                    frame.on_its_way.take().map(|image| frame.shown = Some(image)).is_some()
                };
                if flipped {
                    state.frame_shown(number);
                }
            }
        });
    }

    /// What each display shows.
    fn shown(&self, compositor: &Compositor) -> Vec<Option<Image>> {
        let displays = self.clone();
        compositor.with(move |state| {
            let frames = displays.0.lock().unwrap();
            state
                .outputs()
                .iter()
                .map(|output| frames.get(&output.number).and_then(|frame| frame.shown.clone()))
                .collect()
        })
    }
}

/// Two displays whose frames show when the test flips them, and a window
/// that has the keyboard, shown on both.
fn with_held_flips() -> (Compositor, Displays, Client) {
    let displays = Displays::default();
    let backend = displays.clone();
    let compositor = Compositor::start_with(own_uid(), own_uid(), move |state| {
        state.add_output("TEST-1", (640, 480), 60_000);
        state.add_output("TEST-2", (640, 480), 60_000);
        state.set_backend(backend);
    });
    let client = focused_window(&compositor);
    // Until the desktop is still, every frame is shown.
    for _ in 0..5 {
        displays.flip(&compositor, None);
    }
    (compositor, displays, client)
}

#[test]
fn a_lock_is_answered_only_once_every_display_shows_the_session_locked() {
    let (compositor, displays, _client) = with_held_flips();
    let mut authority = Control::at(&with_authority(&compositor, own_uid()));
    assert_ne!(
        displays.shown(&compositor)[0]
            .as_ref()
            .and_then(|image| image.pixel(60, 60)),
        Some(LOCKED_BACKGROUND),
        "the desktop did not show before the lock"
    );

    // The suspend hook locks, and asks something else after: neither is
    // answered while a display still shows the desktop.
    authority.send(r#"{"op":"lock"}"#);
    authority.send(r#"{"op":"snapshot"}"#);
    assert_eq!(authority.frame_within(Duration::from_millis(300)), None);
    displays.flip(&compositor, Some(0));
    assert_eq!(
        authority.frame_within(Duration::from_millis(300)),
        None,
        "answered while the second display showed the desktop"
    );

    displays.flip(&compositor, Some(1));
    assert_eq!(authority.message(), r#"{"reply":"ok"}"#);
    let after = authority.message();
    assert!(after.starts_with(r#"{"reply":"error""#), "{after}");
    for (index, shown) in displays.shown(&compositor).into_iter().enumerate() {
        let shown = shown.unwrap();
        assert!(
            shown.pixels.iter().all(|pixel| *pixel == LOCKED_BACKGROUND),
            "display {index} does not show the session locked"
        );
    }

    // Once the lock shows, a lock again is answered at once.
    authority.send(r#"{"op":"lock"}"#);
    assert_eq!(
        authority.frame_within(Duration::from_millis(300)).as_deref(),
        Some(r#"{"reply":"ok"}"#)
    );
}

#[test]
fn a_display_that_never_shows_a_frame_holds_the_lock_reply_only_so_long() {
    let (compositor, _displays, _client) = with_held_flips();
    let mut authority = Control::at(&with_authority(&compositor, own_uid()));

    let asked = Instant::now();
    authority.send(r#"{"op":"lock"}"#);
    // The reply still comes, within the bound and well before the unlock
    // helper gives up waiting for it, at five seconds.
    assert_eq!(authority.message(), r#"{"reply":"ok"}"#);
    let waited = asked.elapsed();
    assert!(waited >= LOCK_SHOWN_WITHIN, "answered after {waited:?}");
    assert!(waited < Duration::from_secs(3), "answered after {waited:?}");
    assert!(compositor.with(|state| state.locked()));
}
