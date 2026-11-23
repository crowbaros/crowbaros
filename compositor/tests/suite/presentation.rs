//! `wp_presentation`: a client learns when its content was shown.

use crate::common;

use common::Compositor;
use crowbar_probe::Client;
use fulcrum_wire::{
    Arg, ObjectId,
    protocol::{
        presentation_time::{wp_presentation, wp_presentation_feedback},
        wayland::{wl_output, wl_surface},
    },
};

fn feedback(client: &mut Client, presentation: u32, surface: u32) -> u32 {
    let feedback = client.new_id(wp_presentation_feedback::NAME);
    client
        .request(
            presentation,
            wp_presentation::Request::Feedback {
                surface: ObjectId(surface),
                callback: feedback,
            },
        )
        .unwrap();
    feedback.0
}

fn names(client: &mut Client, feedback: u32) -> Vec<&'static str> {
    client.roundtrip().unwrap();
    client
        .events
        .iter()
        .filter(|event| event.object == feedback)
        .map(|event| event.name)
        .collect()
}

#[test]
fn binding_says_the_clock_is_the_monotonic_one() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let presentation = client.bind(wp_presentation::NAME, 2).unwrap();
    client.roundtrip().unwrap();

    let event = client.take_event(presentation, "clock_id").expect("the clock");
    assert!(matches!(event.args[..], [Arg::Uint(1)]), "{:?}", event.args);
}

#[test]
fn drawn_content_is_presented_on_its_output() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let presentation = client.bind(wp_presentation::NAME, 2).unwrap();
    let output = client.bind(wl_output::NAME, 4).unwrap();
    let window = client.open_toplevel("app", "test.app", (100, 60), 0xffff_ffff).unwrap();

    let feedback = feedback(&mut client, presentation, window.surface);
    client.redraw(&window, (100, 60), 0xff00_0000).unwrap();
    compositor.wait_until_still();

    assert_eq!(names(&mut client, feedback), ["sync_output", "presented"]);
    let events: Vec<_> = client.events.iter().filter(|event| event.object == feedback).collect();
    assert!(matches!(events[0].args[..], [Arg::Object(Some(ObjectId(bound)))] if bound == output));
    match events[1].args[..] {
        [
            Arg::Uint(_),
            Arg::Uint(seconds),
            Arg::Uint(_),
            Arg::Uint(refresh),
            _,
            _,
            Arg::Uint(flags),
        ] => {
            assert!(seconds > 0, "no time");
            assert_eq!(u64::from(refresh), 1_000_000_000_000 / 60_000, "a 60 Hz output");
            assert_eq!(flags, 0, "the headless backend has no display to vouch for");
        }
        ref other => panic!("{other:?}"),
    }
}

#[test]
fn content_replaced_before_it_was_drawn_is_discarded() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let presentation = client.bind(wp_presentation::NAME, 2).unwrap();
    // A surface with no role is on no output, so nothing draws it: each
    // commit's content waits until the next replaces it.
    let surface = client.create_surface().unwrap();
    let first = feedback(&mut client, presentation, surface);
    client.request(surface, wl_surface::Request::Commit).unwrap();

    let second = feedback(&mut client, presentation, surface);
    client.request(surface, wl_surface::Request::Commit).unwrap();

    assert_eq!(names(&mut client, first), ["discarded"]);
    assert!(names(&mut client, second).is_empty(), "the second is still waiting");
}

#[test]
fn a_surface_that_goes_discards_its_feedback() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let presentation = client.bind(wp_presentation::NAME, 2).unwrap();
    let surface = client.create_surface().unwrap();
    let feedback = feedback(&mut client, presentation, surface);
    client.request(surface, wl_surface::Request::Commit).unwrap();

    client.request(surface, wl_surface::Request::Destroy).unwrap();

    assert_eq!(names(&mut client, feedback), ["discarded"]);
}
