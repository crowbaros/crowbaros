//! The GLES renderer draws what the CPU renderer draws.
//!
//! Each test draws an output both ways and compares them pixel by pixel, a
//! step of rounding apart. The GPU here is Mesa's surfaceless platform: the
//! machine's GPU where it has one, and Mesa's software renderer where it has
//! none, as on CI. Where EGL cannot be loaded at all, a test says so and
//! passes.

mod common;

use common::Compositor;
use crowbar_compositor::{image::Image, render::Renderer};
use crowbar_probe::LayerRequest;
use fulcrum_sys::gles::Gles;
use fulcrum_wire::protocol::wlr_layer_shell_unstable_v1::zwlr_layer_surface_v1::anchor::{LEFT, RIGHT, TOP};

/// Both renderers' frames of the first output: the CPU's, then the GPU's.
fn both(compositor: &Compositor) -> Option<(Image, Image)> {
    compositor.with(|state| {
        let gles = match Gles::surfaceless() {
            Ok(gles) => gles,
            Err(error) => {
                eprintln!("no EGL: skipped ({error})");
                return None;
            }
        };
        let mut renderer = Renderer::new(gles);
        let number = state.outputs()[0].number;
        let cpu = state.render_output(number)?;
        let gpu = crowbar_compositor::render::render_output_offscreen(state, number, &mut renderer).unwrap();
        Some((cpu, gpu))
    })
}

/// Where the two frames differ by more than rounding.
fn differences(cpu: &Image, gpu: &Image) -> Vec<(i32, i32, u32, u32)> {
    assert_eq!((cpu.width, cpu.height), (gpu.width, gpu.height));
    let close =
        |a: u32, b: u32| (0..4).all(|channel| ((a >> (channel * 8)) & 0xff).abs_diff((b >> (channel * 8)) & 0xff) <= 2);
    let mut found = Vec::new();
    for y in 0..cpu.height {
        for x in 0..cpu.width {
            let (a, b) = (cpu.pixel(x, y).unwrap(), gpu.pixel(x, y).unwrap());
            if !close(a, b) {
                found.push((x, y, a, b));
            }
        }
    }
    found
}

#[track_caller]
fn assert_same(compositor: &Compositor) {
    let Some((cpu, gpu)) = both(compositor) else { return };
    let found = differences(&cpu, &gpu);
    assert!(
        found.is_empty(),
        "{} pixels differ, the first at {:?} (CPU {:#010x}, GPU {:#010x})",
        found.len(),
        (found[0].0, found[0].1),
        found[0].2,
        found[0].3
    );
}

#[test]
fn windows_and_the_shells_bar_look_the_same_on_the_gpu() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    shell
        .open_layer(
            LayerRequest {
                namespace: "test-bar",
                layer: 2,
                anchor: TOP | LEFT | RIGHT,
                size: (0, 44),
                exclusive_zone: 44,
                keyboard: 0,
            },
            0xff20_4060,
        )
        .unwrap();
    let mut client = compositor.app();
    client
        .open_toplevel("one", "test.one", (300, 200), 0xffc0_3030)
        .unwrap();
    client
        .open_toplevel("two", "test.two", (200, 300), 0xff30_c030)
        .unwrap();
    client.roundtrip().unwrap();

    assert_same(&compositor);
}

#[test]
fn a_translucent_subsurface_blends_the_same_on_the_gpu() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client
        .open_toplevel("window", "test.blend", (300, 200), 0xff00_00c0)
        .unwrap();
    // Half-transparent green, premultiplied.
    client.open_subsurface(&window, (100, 100), 0x8000_8000).unwrap();
    client.roundtrip().unwrap();

    assert_same(&compositor);
}

thread_local! {
    /// A renderer kept on the compositor's thread from one frame to the next.
    static KEPT: std::cell::RefCell<Option<Renderer>> = const { std::cell::RefCell::new(None) };
}

/// The GPU's frame of the first output, drawn by the renderer kept on the
/// compositor's thread, and the CPU's beside it.
fn both_kept(compositor: &Compositor) -> Option<(Image, Image)> {
    compositor.with(|state| {
        KEPT.with(|kept| {
            let mut kept = kept.borrow_mut();
            if kept.is_none() {
                *kept = Some(Renderer::new(Gles::surfaceless().ok()?));
            }
            let renderer = kept.as_mut()?;
            let number = state.outputs()[0].number;
            let cpu = state.render_output(number)?;
            let gpu = crowbar_compositor::render::render_output_offscreen(state, number, renderer).unwrap();
            Some((cpu, gpu))
        })
    })
}

#[test]
fn a_texture_is_uploaded_again_when_its_surface_changes() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client
        .open_toplevel("window", "test.change", (120, 80), 0xff80_0000)
        .unwrap();
    client.roundtrip().unwrap();
    let Some((_, before)) = both_kept(&compositor) else {
        eprintln!("no EGL: skipped");
        return;
    };
    assert_eq!(before.pixel(100, 100), Some(0xff80_0000));

    client.redraw(&window, (120, 80), 0xff00_8000).unwrap();
    client.roundtrip().unwrap();
    let (cpu, gpu) = both_kept(&compositor).unwrap();

    assert_eq!(gpu.pixel(100, 100), Some(0xff00_8000), "the old texture was drawn");
    assert!(differences(&cpu, &gpu).is_empty());
}

#[test]
fn only_what_a_client_damaged_is_uploaded_again() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let window = client
        .open_toplevel("window", "test.patch", (120, 80), 0xff80_0000)
        .unwrap();
    client.roundtrip().unwrap();
    if both_kept(&compositor).is_none() {
        eprintln!("no EGL: skipped");
        return;
    }
    // A new colour all over, of which the client damages only a patch.
    let buffer = client.shm_buffer((120, 80), 0xff00_8000).unwrap();
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
                x: 10,
                y: 10,
                width: 20,
                height: 20,
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

    let (_, gpu) = both_kept(&compositor).unwrap();

    let at = compositor.with(|state| state.window_rects()[0]);
    assert_eq!(gpu.pixel(at.x + 15, at.y + 15), Some(0xff00_8000), "the patch");
    // What the client did not damage it may not have changed, and the
    // texture keeps it: only the patch was uploaded.
    assert_eq!(
        gpu.pixel(at.x + 60, at.y + 60),
        Some(0xff80_0000),
        "the rest was uploaded"
    );
}
