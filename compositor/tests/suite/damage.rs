//! Damage: the GLES renderer draws only what changed, and what it draws is
//! the frame it would have drawn whole.
//!
//! Each test keeps one target from frame to frame, as a display's buffer
//! does, and holds each frame beside the same scene drawn whole. Where EGL
//! cannot be loaded at all, a test says so and passes.

use crate::common;

use common::Compositor;
use crowbar_compositor::{
    State,
    geometry::Rect,
    image::Image,
    render::{OffscreenOutput, Renderer, render_output_offscreen},
};
use fulcrum_sys::gles::Gles;

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
    let compositor = Compositor::start();
    let mut client = compositor.app();
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
    let compositor = Compositor::start();
    let mut client = compositor.app();
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
    let compositor = Compositor::start();
    let mut client = compositor.app();
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
    let compositor = Compositor::start();
    let mut client = compositor.app();
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

#[test]
fn a_commit_that_damages_a_patch_redraws_that_patch_alone() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client
        .open_toplevel("one", "test.one", (200, 100), 0xff80_0000)
        .unwrap();
    client.roundtrip().unwrap();
    if next_frame(&compositor).is_none() {
        eprintln!("no EGL: skipped");
        return;
    }
    // The same window with a green patch, which the client damages alone.
    let patch = Rect::new(20, 10, 30, 20);
    let buffer = client
        .painted_buffer(
            (200, 100),
            |x, y| {
                if patch.contains(x, y) { 0xff00_8000 } else { 0xff80_0000 }
            },
        )
        .unwrap();
    client
        .request(
            window.surface,
            fulcrum_wire::protocol::wayland::wl_surface::Request::Attach {
                buffer: Some(fulcrum_wire::ObjectId(buffer.buffer)),
                x: 0,
                y: 0,
            },
        )
        .unwrap();
    client
        .request(
            window.surface,
            fulcrum_wire::protocol::wayland::wl_surface::Request::DamageBuffer {
                x: patch.x,
                y: patch.y,
                width: patch.width,
                height: patch.height,
            },
        )
        .unwrap();
    client
        .request(
            window.surface,
            fulcrum_wire::protocol::wayland::wl_surface::Request::Commit,
        )
        .unwrap();
    client.roundtrip().unwrap();

    let (drawn, damage, whole) = next_frame(&compositor).unwrap();

    let at = compositor.with(|state| state.window_rects()[0]);
    let expected = patch.translated(at.x, at.y);
    assert_eq!(damage, [expected], "more than the patch was drawn");
    assert!(same(&drawn, &whole), "the frame drawn where it changed differs");
    assert_eq!(drawn.pixel(at.x + 25, at.y + 15), Some(0xff00_8000));
}
