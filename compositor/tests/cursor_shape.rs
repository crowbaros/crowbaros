//! Cursor shapes: a client names its cursor's shape, and the compositor
//! draws the cursor theme's image of it, as it draws its own pointer.

mod common;

use std::path::PathBuf;

use common::Compositor;
use crowbar_compositor::{image::Image, xcursor_file_for_tests};
use crowbar_probe::{Client, Toplevel};
use fulcrum_wire::{
    ObjectId,
    protocol::cursor_shape_v1::{wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1},
};

use wp_cursor_shape_device_v1::shape;

const WHITE: u32 = 0xffff_ffff;
const ARROW: u32 = 0xff10_2030;
const TEXT: u32 = 0xff40_5060;
const GREEN: u32 = 0xff20_c020;

/// A theme of two cursors, 16 square with their hotspots in the middle: the
/// arrow, and the I-beam under its older name.
fn theme() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "fulcrum-cursor-theme-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let cursors = dir.join("Test/cursors");
    std::fs::create_dir_all(&cursors).unwrap();
    std::fs::write(cursors.join("default"), xcursor_file_for_tests(&[(16, ARROW)])).unwrap();
    std::fs::write(cursors.join("xterm"), xcursor_file_for_tests(&[(16, TEXT)])).unwrap();
    dir
}

fn themed() -> (Compositor, PathBuf) {
    let dir = theme();
    let path = dir.clone();
    let compositor = Compositor::start_with(common::own_uid(), common::own_uid(), move |state| {
        state.add_output("TEST-1", common::OUTPUT_SIZE, 60_000);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
        state.use_cursor_theme(vec![path], "Test", 16);
    });
    (compositor, dir)
}

fn screen(compositor: &Compositor) -> Image {
    compositor.with(|state| {
        let number = state.outputs()[0].number;
        state.render_output_with_cursor(number).unwrap()
    })
}

/// A client with a window at 48,48 of 200x100, its pointer, and a cursor
/// shape device for it.
fn window(compositor: &Compositor) -> (Client, Toplevel, u32, crowbar_probe::CompartmentListener) {
    let (mut client, listener) = compositor.compartment("c1");
    let pointer = client.listen_to_pointer().unwrap();
    let window = client.open_toplevel("app", "test.app", (200, 100), GREEN).unwrap();
    let manager = client.bind(wp_cursor_shape_manager_v1::NAME, 2).unwrap();
    let device = client.new_id(wp_cursor_shape_device_v1::NAME);
    client
        .request(
            manager,
            wp_cursor_shape_manager_v1::Request::GetPointer {
                cursor_shape_device: device,
                pointer: ObjectId(pointer),
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    (client, window, device.0, listener)
}

fn set_shape(client: &mut Client, device: u32, shape: u32) {
    client
        .request(
            device,
            wp_cursor_shape_device_v1::Request::SetShape { serial: 0, shape },
        )
        .unwrap();
}

#[test]
fn the_compositor_draws_the_themes_arrow_as_its_own_pointer() {
    let (compositor, dir) = themed();
    compositor.pointer(300, 300);

    let screen = screen(&compositor);

    // The hotspot, the middle of 16 square, is at the pointer.
    assert_eq!(screen.pixel(293, 293), Some(ARROW));
    assert_eq!(screen.pixel(306, 306), Some(ARROW));
    assert_ne!(screen.pixel(309, 309), Some(ARROW));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_shape_the_client_names_is_drawn_while_the_pointer_is_on_it() {
    let (compositor, dir) = themed();
    let (mut client, _window, device, _listener) = window(&compositor);
    compositor.pointer(100, 100);
    client.roundtrip().unwrap();

    set_shape(&mut client, device, shape::TEXT);
    client.roundtrip().unwrap();
    assert_eq!(screen(&compositor).pixel(100, 100), Some(TEXT), "the I-beam");

    // A shape the theme has not is the arrow.
    set_shape(&mut client, device, shape::ZOOM_IN);
    client.roundtrip().unwrap();
    assert_eq!(screen(&compositor).pixel(100, 100), Some(ARROW));

    // Off the window, the compositor's own pointer comes back.
    set_shape(&mut client, device, shape::TEXT);
    client.roundtrip().unwrap();
    compositor.pointer(600, 600);
    assert_eq!(screen(&compositor).pixel(600, 600), Some(ARROW));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_client_the_pointer_is_not_on_cannot_name_the_shape() {
    let (compositor, dir) = themed();
    let (mut client, _window, device, _listener) = window(&compositor);
    compositor.pointer(600, 600);
    client.roundtrip().unwrap();

    set_shape(&mut client, device, shape::TEXT);
    client.roundtrip().unwrap();

    assert_eq!(screen(&compositor).pixel(600, 600), Some(ARROW));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn with_no_theme_a_shape_is_the_compositors_square() {
    let compositor = Compositor::start();
    let (mut client, _window, device, _listener) = window(&compositor);
    compositor.pointer(100, 100);
    client.roundtrip().unwrap();

    set_shape(&mut client, device, shape::TEXT);
    client.roundtrip().unwrap();

    assert_eq!(screen(&compositor).pixel(105, 105), Some(WHITE));
}

#[test]
fn a_shape_that_is_not_one_is_refused() {
    for (version, bad) in [(2, 0), (2, shape::ALL_RESIZE + 1), (1, shape::DND_ASK)] {
        let compositor = Compositor::start();
        let (mut client, _listener) = compositor.compartment("c1");
        let pointer = client.listen_to_pointer().unwrap();
        let manager = client.bind(wp_cursor_shape_manager_v1::NAME, version).unwrap();
        let device = client.new_id(wp_cursor_shape_device_v1::NAME);
        client
            .request(
                manager,
                wp_cursor_shape_manager_v1::Request::GetPointer {
                    cursor_shape_device: device,
                    pointer: ObjectId(pointer),
                },
            )
            .unwrap();
        set_shape(&mut client, device.0, bad);

        assert!(
            client.roundtrip().is_err(),
            "shape {bad} of version {version} was taken"
        );
        assert_eq!(
            client.error.clone().unwrap().code,
            wp_cursor_shape_device_v1::error::INVALID_SHAPE
        );
    }
}

#[test]
fn the_gpu_draws_a_shape_as_the_cpu_does() {
    let (compositor, dir) = themed();
    let (mut client, _window, device, _listener) = window(&compositor);
    compositor.pointer(100, 100);
    client.roundtrip().unwrap();
    set_shape(&mut client, device, shape::TEXT);
    client.roundtrip().unwrap();

    let both = compositor.with(|state| {
        let gles = fulcrum_sys::gles::Gles::surfaceless().ok()?;
        let mut renderer = crowbar_compositor::render::Renderer::new(gles);
        let number = state.outputs()[0].number;
        let cpu = state.render_output_with_cursor(number)?;
        let gpu = crowbar_compositor::render::render_output_with_cursor_offscreen(state, number, &mut renderer).ok()?;
        Some((cpu, gpu))
    });
    let Some((cpu, gpu)) = both else {
        eprintln!("no EGL: skipped");
        return;
    };

    assert_eq!(gpu.pixel(100, 100), Some(TEXT));
    assert_eq!(gpu.pixel(93, 93), cpu.pixel(93, 93));
    assert_eq!(gpu.pixel(109, 109), cpu.pixel(109, 109));
    let _ = std::fs::remove_dir_all(dir);
}
