//! Output scale (ADR 0020): from 1 to 3 in steps of a quarter, for each
//! display. The desktop is in logical units, an output covers its pixels
//! over its scale, and what it shows is drawn at its pixels.

mod common;

use common::{Compositor, own_uid};
use crowbar_compositor::geometry::Rect;
use crowbar_probe::Client;
use fulcrum_wire::{
    Arg, ObjectId,
    protocol::{
        fractional_scale_v1::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1},
        wayland::wl_output,
    },
};

const GREEN: u32 = 0xff20_c020;

/// A compositor with one output of `size` pixels at `scale_120`.
fn scaled(size: (i32, i32), scale_120: u32) -> Compositor {
    Compositor::start_with(own_uid(), own_uid(), move |state| {
        let output = state.add_output("TEST-1", size, 60_000);
        state.set_output_scale(output, scale_120).unwrap();
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
    })
}

fn screen(compositor: &Compositor) -> crowbar_compositor::image::Image {
    compositor.with(|state| {
        let number = state.outputs()[0].number;
        state.render_output(number).unwrap()
    })
}

/// The last value of an event's one argument on an object.
fn last(client: &mut Client, object: u32, name: &str) -> Option<i64> {
    client.roundtrip().unwrap();
    client
        .events
        .iter()
        .rev()
        .find(|event| event.object == object && event.name == name)
        .and_then(|event| match event.args.as_slice() {
            [Arg::Int(value)] => Some(i64::from(*value)),
            [Arg::Uint(value)] => Some(i64::from(*value)),
            _ => None,
        })
}

fn fractional_scale(client: &mut Client, surface: u32) -> u32 {
    let manager = client.bind(wp_fractional_scale_manager_v1::NAME, 1).unwrap();
    let scale = client.new_id(wp_fractional_scale_v1::NAME);
    client
        .request(
            manager,
            wp_fractional_scale_manager_v1::Request::GetFractionalScale {
                id: scale,
                surface: ObjectId(surface),
            },
        )
        .unwrap();
    scale.0
}

#[test]
fn an_output_at_scale_two_covers_half_its_pixels_and_draws_at_all_of_them() {
    let compositor = scaled((2560, 1600), 240);
    let mut client = compositor.app();
    client.open_toplevel("app", "test.app", (200, 100), GREEN).unwrap();
    client.roundtrip().unwrap();

    let output = compositor.with(|state| state.outputs()[0].rect());
    assert_eq!(output, Rect::new(0, 0, 1280, 800));
    let window = compositor.with(|state| state.window_rects()[0]);
    assert_eq!(
        window,
        Rect::new(48, 48, 200, 100),
        "the window is placed in logical units"
    );

    let screen = screen(&compositor);
    assert_eq!((screen.width, screen.height), (2560, 1600));
    // The window's 200x100 are 400x200 pixels from 96,96.
    assert_eq!(screen.pixel(96, 96), Some(GREEN));
    assert_eq!(screen.pixel(495, 295), Some(GREEN));
    assert_ne!(screen.pixel(497, 297), Some(GREEN));
    // Its frame of 2 is 4 pixels wide.
    assert_eq!(screen.pixel(92, 120), Some(crowbar_compositor::FRAME_FOCUSED));
}

#[test]
fn a_client_hears_the_scale_rounded_up_and_its_surface_the_scale_itself() {
    let compositor = scaled((1920, 1200), 180);
    let mut client = compositor.app();
    let output = client.bind(wl_output::NAME, 4).unwrap();
    let window = client.open_toplevel("app", "test.app", (200, 100), GREEN).unwrap();
    let scale = fractional_scale(&mut client, window.surface);

    assert_eq!(last(&mut client, output, "scale"), Some(2));
    assert_eq!(last(&mut client, scale, "preferred_scale"), Some(180));

    compositor.with(|state| {
        let number = state.outputs()[0].number;
        state.set_output_scale(number, 300).unwrap();
    });

    assert_eq!(last(&mut client, output, "scale"), Some(3));
    assert_eq!(last(&mut client, scale, "preferred_scale"), Some(300));
    let rect = compositor.with(|state| state.outputs()[0].rect());
    assert_eq!(rect, Rect::new(0, 0, 768, 480));
}

#[test]
fn a_scale_is_from_one_to_three_in_quarters() {
    let compositor = Compositor::start();
    let refused = compositor.with(|state| {
        let number = state.outputs()[0].number;
        [0, 119, 132, 165, 390, 3600]
            .into_iter()
            .filter(|scale| state.set_output_scale(number, *scale).is_err())
            .count()
    });
    assert_eq!(refused, 6);
    let taken = compositor.with(|state| {
        let number = state.outputs()[0].number;
        [120, 150, 210, 360]
            .into_iter()
            .filter(|scale| state.set_output_scale(number, *scale).is_ok())
            .count()
    });
    assert_eq!(taken, 4);
}

#[test]
fn outputs_side_by_side_meet_at_their_logical_edges() {
    let compositor = Compositor::start_with(own_uid(), own_uid(), |state| {
        let first = state.add_output("TEST-1", (2560, 1600), 60_000);
        state.set_output_scale(first, 240).unwrap();
        state.add_output("TEST-2", (1920, 1080), 60_000);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
    });

    let rects: Vec<Rect> = compositor.with(|state| state.outputs().iter().map(|output| output.rect()).collect());

    assert_eq!(rects, [Rect::new(0, 0, 1280, 800), Rect::new(1280, 0, 1920, 1080)]);

    // Scaling the first back to 1 moves the second along.
    compositor.with(|state| {
        let number = state.outputs()[0].number;
        state.set_output_scale(number, 120).unwrap();
    });
    let second = compositor.with(|state| state.outputs()[1].rect());
    assert_eq!(second, Rect::new(2560, 0, 1920, 1080));
}

#[test]
fn the_gpu_draws_a_scaled_output_as_the_cpu_does() {
    let compositor = scaled((1600, 1000), 150);
    let mut client = compositor.app();
    client.open_toplevel("app", "test.app", (200, 100), GREEN).unwrap();
    client.roundtrip().unwrap();

    let both = compositor.with(|state| {
        let gles = fulcrum_sys::gles::Gles::surfaceless().ok()?;
        let mut renderer = crowbar_compositor::render::Renderer::new(gles);
        let number = state.outputs()[0].number;
        let cpu = state.render_output(number)?;
        let gpu = crowbar_compositor::render::render_output_offscreen(state, number, &mut renderer).ok()?;
        Some((cpu, gpu))
    });
    let Some((cpu, gpu)) = both else {
        eprintln!("no EGL: skipped");
        return;
    };

    assert_eq!((gpu.width, gpu.height), (1600, 1000));
    // Inside the window, at 60..310 by 60..185 pixels, and on its frame.
    for (x, y) in [(100, 100), (300, 180), (58, 100), (20, 20)] {
        assert_eq!(gpu.pixel(x, y), cpu.pixel(x, y), "at {x},{y}");
    }
    assert_eq!(gpu.pixel(100, 100), Some(GREEN));
}
