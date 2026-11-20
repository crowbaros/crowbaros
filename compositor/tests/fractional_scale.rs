//! `wp_fractional_scale_v1`: a surface hears the scale it should draw at.

mod common;

use common::Compositor;
use fulcrum_wire::{
    Arg, ObjectId,
    protocol::fractional_scale_v1::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1},
};

fn get(client: &mut crowbar_probe::Client, manager: u32, surface: u32) -> u32 {
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
fn a_surface_hears_it_should_draw_at_scale_one() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let manager = client.bind(wp_fractional_scale_manager_v1::NAME, 1).unwrap();
    let surface = client.create_surface().unwrap();

    let scale = get(&mut client, manager, surface);
    client.roundtrip().unwrap();

    let event = client.take_event(scale, "preferred_scale").expect("a preferred scale");
    assert!(matches!(event.args[..], [Arg::Uint(120)]), "{:?}", event.args);
}

#[test]
fn a_surface_has_one_fractional_scale_at_a_time() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let manager = client.bind(wp_fractional_scale_manager_v1::NAME, 1).unwrap();
    let surface = client.create_surface().unwrap();
    let first = get(&mut client, manager, surface);
    client.request(first, wp_fractional_scale_v1::Request::Destroy).unwrap();
    get(&mut client, manager, surface);
    client.roundtrip().unwrap();

    get(&mut client, manager, surface);

    assert!(client.roundtrip().is_err());
    assert_eq!(
        client.error.clone().unwrap().code,
        wp_fractional_scale_manager_v1::error::FRACTIONAL_SCALE_EXISTS
    );
}
