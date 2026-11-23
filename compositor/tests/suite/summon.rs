//! The summon gesture, Super+Space, and the Agent's surface it brings up
//! (ADR 0035): the shell's drawer, which the shell shows, puts away, and
//! moves the keyboard to by asking.

use crate::common;

use common::{Compositor, control::Control, own_uid};
use crowbar_compositor::{agent::AGENT_NAMESPACE, geometry::Rect};
use crowbar_probe::{Client, KeyEvent, LayerRequest, LayerWindow};
use fulcrum_wire::protocol::wlr_layer_shell_unstable_v1::zwlr_layer_surface_v1::{
    self,
    anchor::{BOTTOM, LEFT, RIGHT, TOP},
    keyboard_interactivity::{EXCLUSIVE, ON_DEMAND},
};

const OVERLAY: u32 = 3;

const KEY_SUPER: u32 = 125;
const KEY_SPACE: u32 = 57;

fn chord(compositor: &Compositor, keys: &[u32]) {
    for key in keys {
        compositor.key(*key, true);
    }
    for key in keys.iter().rev() {
        compositor.key(*key, false);
    }
}

/// An app's window at 48,48 that has the keyboard, as a new window does.
fn window(compositor: &Compositor, app_id: &str) -> Client {
    let mut client = compositor.app();
    client.listen_to_keyboard().unwrap();
    client.open_toplevel(app_id, app_id, (200, 100), 0xffc0_2020).unwrap();
    client.roundtrip().unwrap();
    client.take_keys().unwrap();
    client
}

/// The keys a client heard go down or up since it was last asked.
fn keys(client: &mut Client) -> Vec<u32> {
    client.roundtrip().unwrap();
    client
        .take_keys()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            KeyEvent::Key { key, .. } => Some(key),
            _ => None,
        })
        .collect()
}

/// Super+Space is the compositor's: the shell is told, with the display the
/// pointer is on and where the keyboard is, and the app with the keyboard
/// hears nothing of the Space. It is no tap of Super alone.
#[test]
fn super_space_tells_the_shell_and_no_app_hears_it() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let mut app = window(&compositor, "test.app");

    chord(&compositor, &[KEY_SUPER, KEY_SPACE]);

    assert_eq!(
        control.message(),
        r#"{"event":"summon","output":"TEST-1","width":1280,"height":800,"keyboard":false}"#
    );
    assert!(!keys(&mut app).contains(&KEY_SPACE), "an app heard the summon gesture");
    let next = control.ask(r#"{"op":"launching"}"#);
    assert_eq!(next, r#"{"reply":"ok"}"#, "the gesture also asked for the launcher");

    // Super tapped alone still asks for the launcher.
    chord(&compositor, &[KEY_SUPER]);
    assert_eq!(control.message(), r#"{"event":"launcher"}"#);
}

/// While the desktop is locked the gesture is dropped: the shell is told
/// nothing, and the lock screen hears no Space.
#[test]
fn the_gesture_is_dropped_while_locked() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let mut shell = compositor.shell();
    shell.listen_to_keyboard().unwrap();
    assert_eq!(control.ask(r#"{"op":"lock"}"#), r#"{"event":"locked"}"#);
    assert_eq!(control.message(), r#"{"reply":"ok"}"#);
    shell
        .open_layer(
            LayerRequest {
                namespace: crowbar_compositor::lock::LOCK_NAMESPACE,
                layer: OVERLAY,
                anchor: TOP | BOTTOM | LEFT | RIGHT,
                size: (0, 0),
                exclusive_zone: -1,
                keyboard: EXCLUSIVE,
            },
            0xff00_00ff,
        )
        .unwrap();
    shell.take_keys().unwrap();

    chord(&compositor, &[KEY_SUPER, KEY_SPACE]);

    assert!(
        !keys(&mut shell).contains(&KEY_SPACE),
        "the lock screen heard the gesture"
    );
    assert_eq!(control.ask(r#"{"op":"unlock"}"#), r#"{"event":"unlocked"}"#);
    assert_eq!(control.message(), r#"{"reply":"ok"}"#);

    // Unlocked, the same gesture reaches the shell.
    chord(&compositor, &[KEY_SUPER, KEY_SPACE]);
    assert!(control.message().starts_with(r#"{"event":"summon","#));
}

const DRAWER: u32 = 0xff20_6040;

/// The Agent's surface as the shell makes it: a drawer on the right, the
/// full height, a third as wide as the display the gesture named, above
/// every window, reserving no room, and taking the keyboard when clicked.
fn drawer(shell: &mut Client, display_width: u32) -> LayerWindow {
    shell
        .open_layer(
            LayerRequest {
                namespace: AGENT_NAMESPACE,
                layer: OVERLAY,
                anchor: TOP | BOTTOM | RIGHT,
                size: (display_width / 3, 0),
                exclusive_zone: -1,
                keyboard: ON_DEMAND,
            },
            DRAWER,
        )
        .unwrap()
}

/// Two displays side by side: TEST-1, 640 by 480, and TEST-2, 900 by 600.
fn two_displays() -> Compositor {
    Compositor::start_with(own_uid(), own_uid(), |state| {
        state.add_output("TEST-1", (640, 480), 60_000);
        state.add_output("TEST-2", (900, 600), 60_000);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
    })
}

/// The gesture names the display the pointer is on, and the Agent's surface
/// the shell then makes goes on that display, wherever the pointer has gone
/// since. It takes no room from the windows.
#[test]
fn the_agents_surface_comes_up_on_the_display_the_pointer_was_on_at_the_summon() {
    let compositor = two_displays();
    let mut control = Control::connect(&compositor);
    let mut shell = compositor.shell();
    compositor.pointer(640 + 100, 100);

    chord(&compositor, &[KEY_SUPER, KEY_SPACE]);
    assert_eq!(
        control.message(),
        r#"{"event":"summon","output":"TEST-2","width":900,"height":600,"keyboard":false}"#
    );
    compositor.pointer(10, 10);
    let surface = drawer(&mut shell, 900);

    assert_eq!(surface.size, (300, 600));
    assert_eq!(
        compositor.with(|state| state.layer_rects_by_namespace()),
        [(AGENT_NAMESPACE.to_string(), Rect::new(640 + 600, 0, 300, 600))]
    );
    let (second, usable) = compositor.with(|state| {
        let output = &state.outputs()[1];
        (output.rect(), state.usable_area(output.number))
    });
    assert_eq!(usable, second, "the Agent's surface took room from the windows");
}

const KEY_A: u32 = 30;

/// One key pressed and let go.
fn tap(compositor: &Compositor, key: u32) {
    compositor.key(key, true);
    compositor.key(key, false);
}

const FOCUS: &str = r#"{"op":"focus_agent"}"#;
const UNFOCUS: &str = r#"{"op":"unfocus_agent"}"#;
const OK: &str = r#"{"reply":"ok"}"#;

/// The gesture's event, on the one test display, with the keyboard in the
/// Agent's surface or not.
fn summoned(keyboard: bool) -> String {
    format!(r#"{{"event":"summon","output":"TEST-1","width":1280,"height":800,"keyboard":{keyboard}}}"#)
}

/// Summoning gives the Agent's surface the keyboard because the shell asks:
/// as it makes the surface, which is not shown yet. The keyboard stays where
/// it is until the surface shows, and the next gesture says where it is.
#[test]
fn the_shell_gives_the_agents_surface_the_keyboard_as_it_shows() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let mut app = window(&compositor, "test.app");
    let mut shell = compositor.shell();
    shell.listen_to_keyboard().unwrap();

    chord(&compositor, &[KEY_SUPER, KEY_SPACE]);
    assert_eq!(control.message(), summoned(false));
    assert_eq!(control.ask(FOCUS), OK);
    app.take_keys().unwrap();
    tap(&compositor, KEY_A);
    assert_eq!(
        keys(&mut app),
        [KEY_A, KEY_A],
        "the window lost the keyboard to a surface not shown"
    );

    drawer(&mut shell, 1280);
    shell.take_keys().unwrap();
    tap(&compositor, KEY_A);
    assert_eq!(
        keys(&mut shell),
        [KEY_A, KEY_A],
        "the Agent's surface did not get the keyboard"
    );
    assert_eq!(keys(&mut app), [], "the window kept the keyboard");

    chord(&compositor, &[KEY_SUPER, KEY_SPACE]);
    assert_eq!(control.message(), summoned(true));
}

/// Two windows, the first at 48,48 and the second, which has the keyboard,
/// offset from it; and the shell with the Agent's surface up and holding
/// the keyboard, as after a summon.
fn summoned_over_two_windows(compositor: &Compositor, control: &mut Control) -> (Client, Client, Client, LayerWindow) {
    let first = window(compositor, "test.first");
    let second = window(compositor, "test.second");
    let mut shell = compositor.shell();
    shell.listen_to_keyboard().unwrap();
    assert_eq!(control.ask(FOCUS), OK);
    let surface = drawer(&mut shell, 1280);
    shell.take_keys().unwrap();
    (first, second, shell, surface)
}

/// A click on a window gives it the keyboard and leaves the Agent's surface
/// up. The shell's `focus_agent` brings the keyboard back to the surface,
/// and its `unfocus_agent` gives it to the window that had it.
#[test]
fn the_keyboard_goes_to_a_clicked_window_back_to_the_surface_and_then_to_the_window_that_had_it() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let (mut first, mut second, mut shell, _surface) = summoned_over_two_windows(&compositor, &mut control);

    compositor.click(60, 60);
    tap(&compositor, KEY_A);
    assert_eq!(
        keys(&mut first),
        [KEY_A, KEY_A],
        "the clicked window did not get the keyboard"
    );
    assert_eq!(
        compositor.screen(0).pixel(1280 - 10, 400),
        Some(DRAWER),
        "the Agent's surface went away"
    );
    chord(&compositor, &[KEY_SUPER, KEY_SPACE]);
    assert_eq!(control.message(), summoned(false));

    assert_eq!(control.ask(FOCUS), OK);
    shell.take_keys().unwrap();
    tap(&compositor, KEY_A);
    assert_eq!(
        keys(&mut shell),
        [KEY_A, KEY_A],
        "the keyboard did not return to the Agent's surface"
    );

    assert_eq!(control.ask(UNFOCUS), OK);
    first.take_keys().unwrap();
    tap(&compositor, KEY_A);
    assert_eq!(
        keys(&mut first),
        [KEY_A, KEY_A],
        "the window that had the keyboard did not get it back"
    );
    assert_eq!(keys(&mut second), [], "another window got the keyboard");
}

/// Takes the Agent's surface down, as the shell does to put it away.
fn put_away(shell: &mut Client, surface: &LayerWindow) {
    shell
        .request(surface.layer_surface, zwlr_layer_surface_v1::Request::Destroy)
        .unwrap();
    shell.roundtrip().unwrap();
}

/// The shell puts the surface away on one socket and gives the keyboard
/// back on another, so either may reach the compositor first. The window
/// that had the keyboard gets it back both ways.
#[test]
fn the_keyboard_goes_back_whether_the_surface_goes_before_the_shell_asks_or_after() {
    for surface_first in [true, false] {
        let compositor = Compositor::start();
        let mut control = Control::connect(&compositor);
        let (mut first, mut second, mut shell, surface) = summoned_over_two_windows(&compositor, &mut control);

        if surface_first {
            put_away(&mut shell, &surface);
            assert_eq!(control.ask(UNFOCUS), OK);
        } else {
            assert_eq!(control.ask(UNFOCUS), OK);
            put_away(&mut shell, &surface);
        }

        second.take_keys().unwrap();
        tap(&compositor, KEY_A);
        assert_eq!(
            keys(&mut second),
            [KEY_A, KEY_A],
            "the window that had the keyboard did not get it back (surface first: {surface_first})"
        );
        assert_eq!(keys(&mut first), []);
        assert_ne!(compositor.screen(0).pixel(1280 - 10, 400), Some(DRAWER));
    }
}

/// With the keyboard in a window, the shell's `unfocus_agent` moves
/// nothing: the close control puts the surface away from wherever the
/// keyboard is. And a window that closed is given nothing back.
#[test]
fn giving_the_keyboard_back_takes_it_from_no_window_and_gives_it_to_none_that_closed() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let (mut first, second, _shell, _surface) = summoned_over_two_windows(&compositor, &mut control);

    compositor.click(60, 60);
    assert_eq!(control.ask(UNFOCUS), OK);
    first.take_keys().unwrap();
    tap(&compositor, KEY_A);
    assert_eq!(keys(&mut first), [KEY_A, KEY_A], "the clicked window lost the keyboard");

    // The surface takes the keyboard from the first window, which closes.
    assert_eq!(control.ask(FOCUS), OK);
    drop(first);
    let mut second = second;
    second.roundtrip().unwrap();
    assert_eq!(control.ask(UNFOCUS), OK);
    second.take_keys().unwrap();
    tap(&compositor, KEY_A);
    assert_eq!(
        keys(&mut second),
        [],
        "a window that never had the keyboard was handed it"
    );
    let snapshot = control.ask(r#"{"op":"snapshot"}"#);
    assert!(snapshot.contains(r#""active":null"#), "{snapshot}");
}

/// Locked, the Agent's surface is neither drawn nor heard. Unlocked, it is
/// up as it was, with the keyboard it held.
#[test]
fn after_unlock_the_agents_surface_is_up_with_the_keyboard_as_it_was() {
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let (_first, mut second, mut shell, _surface) = summoned_over_two_windows(&compositor, &mut control);

    assert_eq!(control.ask(r#"{"op":"lock"}"#), r#"{"event":"locked"}"#);
    assert_eq!(control.message(), OK);
    assert_ne!(
        compositor.screen(0).pixel(1280 - 10, 400),
        Some(DRAWER),
        "the Agent's surface shows while locked"
    );
    tap(&compositor, KEY_A);
    assert_eq!(keys(&mut shell), [], "the Agent's surface heard a key while locked");
    for request in [FOCUS, UNFOCUS] {
        let refused = control.ask(request);
        assert!(refused.starts_with(r#"{"reply":"error""#), "{request}: {refused}");
    }

    assert_eq!(control.ask(r#"{"op":"unlock"}"#), r#"{"event":"unlocked"}"#);
    assert_eq!(control.message(), OK);
    assert_eq!(compositor.screen(0).pixel(1280 - 10, 400), Some(DRAWER));
    shell.take_keys().unwrap();
    tap(&compositor, KEY_A);
    assert_eq!(
        keys(&mut shell),
        [KEY_A, KEY_A],
        "the Agent's surface lost the keyboard to the lock"
    );
    assert_eq!(keys(&mut second), []);

    // And the window that had the keyboard before it is still remembered.
    assert_eq!(control.ask(UNFOCUS), OK);
    tap(&compositor, KEY_A);
    assert_eq!(keys(&mut second), [KEY_A, KEY_A]);
}

/// A fullscreen window covers the bar and the dock, and not the Agent's
/// surface: the drawer is drawn over it, takes the pointer there, and the
/// window keeps the size it had.
#[test]
fn the_agents_surface_is_above_a_fullscreen_window_and_takes_no_room_from_it() {
    const KEY_F: u32 = 33;
    const WINDOW: u32 = 0xff30_c030;
    let compositor = Compositor::start();
    let mut control = Control::connect(&compositor);
    let mut app = compositor.app();
    app.listen_to_keyboard().unwrap();
    let window = app.open_toplevel("app", "test.app", (200, 100), WINDOW).unwrap();
    chord(&compositor, &[KEY_SUPER, KEY_F]);
    app.roundtrip().unwrap();
    app.redraw(&window, (1, 1), WINDOW).unwrap();
    let fullscreen = compositor.with(|state| state.window_rects()[0]);
    assert_eq!((fullscreen.width, fullscreen.height), (1280, 800));

    let mut shell = compositor.shell();
    shell.listen_to_keyboard().unwrap();
    chord(&compositor, &[KEY_SUPER, KEY_SPACE]);
    assert_eq!(control.message(), summoned(false));
    drawer(&mut shell, 1280);

    let screen = compositor.screen(0);
    assert_eq!(
        screen.pixel(1280 - 10, 400),
        Some(DRAWER),
        "the window covers the Agent's surface"
    );
    assert_eq!(screen.pixel(400, 400), Some(WINDOW));
    assert_eq!(compositor.with(|state| state.window_rects()[0]), fullscreen);

    // A click on the drawer, over the window, gives the drawer the keyboard.
    compositor.click(1280 - 10, 400);
    shell.take_keys().unwrap();
    app.take_keys().unwrap();
    tap(&compositor, KEY_A);
    assert_eq!(keys(&mut shell), [KEY_A, KEY_A]);
    assert_eq!(keys(&mut app), []);
}
