//! The primary selection, under the clipboard's rules (ADR 0032): it moves
//! between all the person's apps, and the shell's is its own.

mod common;

use std::io::{Read, Write};

use common::Compositor;
use crowbar_probe::Client;
use fulcrum_wire::{
    Arg, ObjectId,
    protocol::{
        primary_selection_unstable_v1::{
            zwp_primary_selection_device_manager_v1, zwp_primary_selection_device_v1, zwp_primary_selection_offer_v1,
            zwp_primary_selection_source_v1,
        },
        wayland::wl_seat,
    },
};

/// The manager and a device for the seat.
fn device(client: &mut Client) -> (u32, u32) {
    let manager = client.bind(zwp_primary_selection_device_manager_v1::NAME, 1).unwrap();
    let seat = client.bind(wl_seat::NAME, 7).unwrap();
    let device = client.new_id(zwp_primary_selection_device_v1::NAME);
    client
        .request(
            manager,
            zwp_primary_selection_device_manager_v1::Request::GetDevice {
                id: device,
                seat: ObjectId(seat),
            },
        )
        .unwrap();
    (manager, device.0)
}

fn select(client: &mut Client, manager: u32, device: u32, mime_type: &str) -> u32 {
    let source = client.new_id(zwp_primary_selection_source_v1::NAME);
    client
        .request(
            manager,
            zwp_primary_selection_device_manager_v1::Request::CreateSource { id: source },
        )
        .unwrap();
    client
        .request(
            source.0,
            zwp_primary_selection_source_v1::Request::Offer {
                mime_type: mime_type.into(),
            },
        )
        .unwrap();
    client
        .request(
            device,
            zwp_primary_selection_device_v1::Request::SetSelection {
                source: Some(ObjectId(source.0)),
                serial: 0,
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    source.0
}

/// The offer a device was last given, if any, and its MIME types.
fn last_selection(client: &mut Client, device: u32) -> Option<(u32, Vec<String>)> {
    client.roundtrip().unwrap();
    let selection = client
        .events
        .iter()
        .rev()
        .find(|event| event.object == device && event.name == "selection")?;
    let offer = match selection.args.as_slice() {
        [Arg::Object(Some(offer))] => offer.0,
        _ => return None,
    };
    let mime_types = client
        .events
        .iter()
        .filter(|event| event.object == offer && event.name == "offer")
        .filter_map(|event| match event.args.as_slice() {
            [Arg::String(Some(mime_type))] => Some(mime_type.clone()),
            _ => None,
        })
        .collect();
    Some((offer, mime_types))
}

#[test]
fn a_primary_selection_made_in_one_app_is_pasted_in_another() {
    let compositor = Compositor::start();
    let mut selector = compositor.app();
    let mut paster = compositor.app();
    let (manager, selector_device) = device(&mut selector);
    let (_, paster_device) = device(&mut paster);
    selector
        .open_toplevel("selector", "test.selector", (300, 200), 0xffff_0000)
        .unwrap();
    paster
        .open_toplevel("paster", "test.paster", (300, 200), 0xff00_00ff)
        .unwrap();
    let source = select(&mut selector, manager, selector_device, "text/plain");

    compositor.click(200, 200);
    let (offer, mime_types) = last_selection(&mut paster, paster_device).expect("an offer");
    assert_eq!(mime_types, ["text/plain"]);

    let (mut read_end, write_end) = std::io::pipe().unwrap();
    paster
        .request(
            offer,
            zwp_primary_selection_offer_v1::Request::Receive {
                mime_type: "text/plain".into(),
                fd: write_end.into(),
            },
        )
        .unwrap();
    paster.roundtrip().unwrap();
    selector.roundtrip().unwrap();
    let send = selector.take_event(source, "send").expect("the selector was asked");
    let [Arg::String(Some(_)), Arg::Fd(fd)] = send.args.as_slice() else {
        panic!("{:?}", send.args);
    };
    std::fs::File::from(fd.try_clone().unwrap())
        .write_all(b"selected words")
        .unwrap();
    drop(send);
    let mut pasted = String::new();
    read_end.read_to_string(&mut pasted).unwrap();
    assert_eq!(pasted, "selected words");

    // The selector lets its source go: the selection is gone too.
    selector
        .request(source, zwp_primary_selection_source_v1::Request::Destroy)
        .unwrap();
    selector.roundtrip().unwrap();
    assert!(last_selection(&mut paster, paster_device).is_none());
}

#[test]
fn an_apps_primary_selection_is_not_offered_to_the_shell() {
    let compositor = Compositor::start();
    let mut selector = compositor.app();
    let mut other = compositor.shell();
    let (manager, selector_device) = device(&mut selector);
    let (_, other_device) = device(&mut other);
    other
        .open_toplevel("other", "test.other", (300, 200), 0xffff_ffff)
        .unwrap();
    let source = select(&mut selector, manager, selector_device, "text/plain");

    compositor.click(60, 60);

    assert!(last_selection(&mut other, other_device).is_none());
    assert!(
        other
            .events
            .iter()
            .all(|event| event.interface != zwp_primary_selection_offer_v1::NAME),
        "an offer reached the shell"
    );
    selector.roundtrip().unwrap();
    assert!(
        selector.take_event(source, "cancelled").is_none(),
        "the selection stands"
    );
}
