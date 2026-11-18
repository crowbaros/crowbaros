//! Damage: the GLES renderer draws only what changed, and what it draws is
//! the frame it would have drawn whole.
//!
//! Each test keeps one target from frame to frame, as a display's buffer
//! does, and holds each frame beside the same scene drawn whole. Where EGL
//! cannot be loaded at all, a test says so and passes. EGL's surfaceless
//! display is one for the whole process: one test runs at a time.

mod common;

use std::sync::{Mutex, MutexGuard};

use common::Compositor;
use crowbar_compositor::{
    State,
    geometry::Rect,
    image::Image,
    render::{OffscreenOutput, Renderer, render_output_offscreen},
};
use fulcrum_sys::gles::Gles;

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

thread_local! {
    /// The renderer and the kept target, on the compositor's thread.
    static KEPT: std::cell::RefCell<Option<(Renderer, OffscreenOutput)>> = const { std::cell::RefCell::new(None) };
}

/// The next frame drawn where it changed, its damage, and the same frame
/// drawn whole. None where there is no EGL.
fn next_frame(compositor: &Compositor) -> Option<(Image, Vec<Rect>, Image)> {
    compositor.with(|state: &mut State| {
        KEPT.with(|kept| {
            let mut kept = kept.borrow_mut();
            if kept.is_none() {
                *kept = Some((Renderer::new(Gles::surfaceless().ok()?), OffscreenOutput::default()));
            }
            let (renderer, output) = kept.as_mut()?;
            let number = state.outputs()[0].number;
            let (drawn, damage) = output.draw(state, number, renderer).unwrap();
            let whole = render_output_offscreen(state, number, renderer).unwrap();
            Some((drawn, damage, whole))
        })
    })
}

fn same(drawn: &Image, whole: &Image) -> bool {
    drawn.pixels == whole.pixels
}

#[test]
fn a_commit_damages_its_window_alone_and_the_frame_is_whole() {
    let _one = one_at_a_time();
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let first = client
        .open_toplevel("one", "test.one", (200, 100), 0xff80_0000)
        .unwrap();
    let _second = client.open_toplevel("two", "test.two", (120, 80), 0xff00_0080).unwrap();
    client.roundtrip().unwrap();
    let Some((drawn, damage, whole)) = next_frame(&compositor) else {
        eprintln!("no EGL: skipped");
        return;
    };
    assert_eq!(damage, [Rect::new(0, 0, 1280, 800)], "the first frame is drawn whole");
    assert!(same(&drawn, &whole));

    client.redraw(&first, (200, 100), 0xff00_8000).unwrap();
    client.roundtrip().unwrap();
    let (drawn, damage, whole) = next_frame(&compositor).unwrap();

    let window = compositor.with(|state| state.window_rects()[0]);
    assert_eq!(damage, [window], "only the window that committed is drawn");
    assert!(same(&drawn, &whole), "the frame drawn where it changed differs");
    assert_eq!(drawn.pixel(window.x + 10, window.y + 10), Some(0xff00_8000));
}

#[test]
fn a_window_that_goes_damages_where_it_was() {
    let _one = one_at_a_time();
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let _below = client
        .open_toplevel("below", "test.below", (300, 200), 0xff80_0000)
        .unwrap();
    let above = client
        .open_toplevel("above", "test.above", (120, 80), 0xff00_0080)
        .unwrap();
    client.roundtrip().unwrap();
    if next_frame(&compositor).is_none() {
        eprintln!("no EGL: skipped");
        return;
    }
    let gone = compositor.with(|state| state.window_rects()[1]);

    client
        .request(
            above.toplevel,
            fulcrum_wire::protocol::xdg_shell::xdg_toplevel::Request::Destroy,
        )
        .unwrap();
    client.roundtrip().unwrap();
    let (drawn, damage, whole) = next_frame(&compositor).unwrap();

    assert!(
        damage.iter().any(|rect| rect.intersection(&gone) == gone),
        "{damage:?} leaves out {gone:?}"
    );
    assert!(same(&drawn, &whole), "the frame drawn where it changed differs");
}

#[test]
fn a_still_output_has_no_damage() {
    let _one = one_at_a_time();
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let _window = client
        .open_toplevel("still", "test.still", (200, 100), 0xff80_0000)
        .unwrap();
    client.roundtrip().unwrap();
    if next_frame(&compositor).is_none() {
        eprintln!("no EGL: skipped");
        return;
    }

    let (drawn, damage, whole) = next_frame(&compositor).unwrap();

    assert!(damage.is_empty(), "{damage:?}");
    assert!(same(&drawn, &whole));
}

#[test]
fn a_focus_change_redraws_both_frames() {
    let _one = one_at_a_time();
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let _first = client
        .open_toplevel("one", "test.one", (200, 100), 0xff80_0000)
        .unwrap();
    let _second = client
        .open_toplevel("two", "test.two", (200, 100), 0xff00_0080)
        .unwrap();
    client.roundtrip().unwrap();
    if next_frame(&compositor).is_none() {
        eprintln!("no EGL: skipped");
        return;
    }
    // A click on the first window gives it the keyboard.
    let first = compositor.with(|state| state.window_rects()[0]);
    compositor.pointer(first.x + 5, first.y + 5);
    compositor.with(|state| state.pointer_button(0x110, true, 0));
    compositor.with(|state| state.pointer_button(0x110, false, 0));
    client.roundtrip().unwrap();

    let (drawn, damage, whole) = next_frame(&compositor).unwrap();

    assert!(!damage.is_empty());
    assert!(same(&drawn, &whole), "the frame drawn where it changed differs");
}
