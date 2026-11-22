//! The compositor draws only when something changed. A still desktop draws no
//! frames and wakes on no timer (ADR 0020). The spike redrew on a timer.

use crate::common;

use std::time::{Duration, Instant};

use common::{Compositor, FRAME, OUTPUT_SIZE};

const WHITE: u32 = 0xffff_ffff;
const RED: u32 = 0xffff_0000;

#[test]
fn a_still_desktop_draws_no_frames_and_does_not_look_for_any() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    client.open_toplevel("still", "test.still", (300, 200), WHITE).unwrap();
    let before = compositor.wait_until_still();
    assert!(before.0 > 0, "the window was never drawn");

    // Sixty frame times, with a window on screen and nothing changing.
    std::thread::sleep(FRAME * 60);

    assert_eq!(compositor.drawing(), before);
}

#[test]
fn a_window_that_changes_is_drawn_once() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client
        .open_toplevel("change", "test.change", (300, 200), WHITE)
        .unwrap();
    let (frames, _) = compositor.wait_until_still();

    client.redraw(&window, (300, 200), RED).unwrap();

    assert_eq!(compositor.wait_until_still().0, frames + 1);
}

#[test]
fn a_commit_that_changes_nothing_draws_no_frame_and_is_still_answered() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client.open_toplevel("same", "test.same", (300, 200), WHITE).unwrap();
    let (frames, _) = compositor.wait_until_still();

    client.commit_unchanged(window.surface).unwrap();
    while client.frames_done == 0 {
        client.wait_for_events(std::time::Duration::from_secs(1)).unwrap();
    }

    assert_eq!(compositor.wait_until_still().0, frames);
}

/// A client may commit every time it is told a frame was drawn. If its
/// commits change nothing, no frame is drawn, and the compositor must still
/// hold it to the display's rate and not spin with it.
#[test]
fn a_client_that_commits_on_every_frame_is_held_to_the_displays_rate() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client.open_toplevel("busy", "test.busy", (300, 200), WHITE).unwrap();
    let (frames, _) = compositor.wait_until_still();

    let start = Instant::now();
    let mut answered = 0;
    while start.elapsed() < FRAME * 30 {
        client.commit_unchanged(window.surface).unwrap();
        while client.frames_done == answered {
            client.wait_for_events(FRAME * 4).unwrap();
        }
        answered = client.frames_done;
    }

    // Thirty frame times passed. At the display's rate that is thirty
    // answers; a compositor that spins gives thousands.
    assert!((1..=45).contains(&answered), "{answered} answers in thirty frame times");
    assert_eq!(
        compositor.wait_until_still().0,
        frames,
        "a frame was drawn for no change"
    );
}

#[test]
fn a_window_that_closes_is_drawn_away_and_then_nothing_more() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    client.open_toplevel("gone", "test.gone", (300, 200), WHITE).unwrap();
    let (frames, _) = compositor.wait_until_still();

    drop(client);

    let after = compositor.wait_until_still();
    assert_eq!(after.0, frames + 1);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(compositor.drawing(), after);
}

/// A menu that closes, or any other part of a window that goes away, has to
/// be drawn away. The window itself commits nothing when that happens.
#[test]
fn a_part_of_a_window_that_is_destroyed_is_drawn_away() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client.open_toplevel("parts", "test.parts", (300, 200), WHITE).unwrap();
    let part = client.open_subsurface(&window, (100, 50), RED).unwrap();
    let (frames, _) = compositor.wait_until_still();

    client
        .request(part, fulcrum_wire::protocol::wayland::wl_surface::Request::Destroy)
        .unwrap();
    client.roundtrip().unwrap();

    assert_eq!(compositor.wait_until_still().0, frames + 1);
}

/// A window that grows across the edge of its output has to be drawn on the
/// output it grew onto, and drawn away there when it shrinks back.
#[test]
fn a_window_that_grows_onto_another_output_is_drawn_on_both() {
    let compositor = Compositor::start();
    compositor.with(|state| {
        state.add_output("TEST-2", OUTPUT_SIZE, 60_000);
    });
    let mut client = compositor.app();
    let window = client.open_toplevel("grows", "test.grows", (300, 200), WHITE).unwrap();
    let (frames, _) = compositor.wait_until_still();

    // The first output is 1280 wide, and the second is to its right.
    client.redraw(&window, (1400, 200), RED).unwrap();
    let grown = compositor.wait_until_still().0;
    assert_eq!(grown, frames + 2, "one frame on each output");

    client.redraw(&window, (300, 200), WHITE).unwrap();
    assert_eq!(compositor.wait_until_still().0, grown + 2, "one frame on each output");
}

/// The pointer is not drawn by the test backend, so moving it changes
/// nothing on screen. The compositor looks, finds nothing, and stops.
#[test]
fn moving_the_pointer_over_a_still_desktop_draws_no_frame() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    client.open_toplevel("still", "test.still", (300, 200), WHITE).unwrap();
    let (frames, _) = compositor.wait_until_still();

    for x in 0..50 {
        compositor.pointer(100 + x, 100);
    }

    let after = compositor.wait_until_still();
    assert_eq!(after.0, frames);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(compositor.drawing(), after);
}
