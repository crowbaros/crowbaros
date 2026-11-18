//! The data device: inside one compartment, copy and paste work as apps
//! expect (ADR 0020). Between compartments nothing moves before slice 4: a
//! selection is offered to no client of another compartment, and a drag
//! ends as it starts.

mod common;

use std::io::{Read, Write};

use common::{Compositor, connect};
use crowbar_probe::Client;
use fulcrum_wire::{
    Arg, ObjectId,
    protocol::wayland::{wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_display, wl_seat},
};

/// The data device manager and a data device for the seat.
fn data_device(client: &mut Client) -> (u32, u32) {
    let manager = client.bind(wl_data_device_manager::NAME, 3).unwrap();
    let seat = client.bind(wl_seat::NAME, 7).unwrap();
    let device = client.new_id(wl_data_device::NAME);
    client
        .request(
            manager,
            wl_data_device_manager::Request::GetDataDevice {
                id: device,
                seat: ObjectId(seat),
            },
        )
        .unwrap();
    (manager, device.0)
}

fn source(client: &mut Client, manager: u32, mime_types: &[&str]) -> u32 {
    let source = client.new_id(wl_data_source::NAME);
    client
        .request(
            manager,
            wl_data_device_manager::Request::CreateDataSource { id: source },
        )
        .unwrap();
    for mime_type in mime_types {
        client
            .request(
                source.0,
                wl_data_source::Request::Offer {
                    mime_type: (*mime_type).into(),
                },
            )
            .unwrap();
    }
    source.0
}

fn set_selection(client: &mut Client, device: u32, source: Option<u32>) {
    client
        .request(
            device,
            wl_data_device::Request::SetSelection {
                source: source.map(ObjectId),
                serial: 0,
            },
        )
        .unwrap();
}

/// The events of one interface the client has seen, by name.
fn seen(client: &mut Client, interface: &str) -> Vec<(u32, &'static str)> {
    client.roundtrip().unwrap();
    client
        .events
        .iter()
        .filter(|event| event.interface == interface)
        .map(|event| (event.object, event.name))
        .collect()
}

#[test]
fn a_selection_is_offered_to_no_other_client_even_one_with_the_keyboard() {
    let compositor = Compositor::start();
    let (mut copier, _first) = compositor.compartment("c1");
    let (mut other, _second) = compositor.compartment("c2");
    let (manager, device) = data_device(&mut copier);
    let (_, other_device) = data_device(&mut other);
    other
        .open_toplevel("other", "test.other", (300, 200), 0xffff_ffff)
        .unwrap();

    let copied = source(&mut copier, manager, &["text/plain;charset=utf-8"]);
    set_selection(&mut copier, device, Some(copied));
    compositor.click(60, 60);

    // As it gains the keyboard it hears that it has no selection.
    assert_eq!(seen(&mut other, "wl_data_device"), [(other_device, "selection")]);
    assert!(last_selection(&mut other, other_device).is_none());
    assert_eq!(seen(&mut other, "wl_data_offer"), []);
    // The copier's selection stands: nothing cancelled it.
    assert_eq!(seen(&mut copier, "wl_data_source"), []);
}

#[test]
fn a_new_selection_cancels_the_one_it_replaces() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let (manager, device) = data_device(&mut client);

    let first = source(&mut client, manager, &["text/plain"]);
    set_selection(&mut client, device, Some(first));
    let second = source(&mut client, manager, &["text/plain"]);
    set_selection(&mut client, device, Some(second));
    set_selection(&mut client, device, None);

    assert_eq!(
        seen(&mut client, "wl_data_source"),
        [(first, "cancelled"), (second, "cancelled")]
    );
}

#[test]
fn a_drag_is_refused_as_it_starts() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let (manager, device) = data_device(&mut client);
    let window = client
        .open_toplevel("app", "test.app", (300, 200), 0xffff_ffff)
        .unwrap();
    let dragged = source(&mut client, manager, &["text/uri-list"]);

    client
        .request(
            device,
            wl_data_device::Request::StartDrag {
                source: Some(ObjectId(dragged)),
                origin: ObjectId(window.surface),
                icon: None,
                serial: 0,
            },
        )
        .unwrap();

    assert_eq!(seen(&mut client, "wl_data_source"), [(dragged, "cancelled")]);
}

/// Expects the compositor to refuse what `send` does with `code`.
fn refused(code: u32, send: impl FnOnce(&mut Client, u32, u32)) {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let (manager, device) = data_device(&mut client);
    send(&mut client, manager, device);
    assert!(client.roundtrip().is_err(), "the compositor accepted it");
    let error = client.error.clone().expect("a protocol error");
    assert_eq!(error.code, code, "{error:?}");
    compositor.shell().roundtrip().unwrap();
}

#[test]
fn a_source_is_used_once() {
    refused(wl_data_device::error::USED_SOURCE, |client, manager, device| {
        let copied = source(client, manager, &["text/plain"]);
        set_selection(client, device, Some(copied));
        set_selection(client, device, Some(copied));
    });
}

#[test]
fn a_drag_and_drop_action_must_be_one_the_protocol_has() {
    refused(wl_data_source::error::INVALID_ACTION_MASK, |client, manager, _| {
        let dragged = source(client, manager, &[]);
        client
            .request(dragged, wl_data_source::Request::SetActions { dnd_actions: 8 })
            .unwrap();
    });
}

#[test]
fn a_client_cannot_make_the_compositor_hold_mime_types_without_end() {
    refused(wl_display::error::NO_MEMORY, |client, manager, _| {
        let many: Vec<String> = (0..65).map(|n| format!("application/x-test-{n}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        source(client, manager, &many);
    });
    refused(wl_display::error::NO_MEMORY, |client, manager, _| {
        // Under the count for each source, over the bytes for the client.
        let long = format!("application/x-{}", "a".repeat(3000));
        for _ in 0..30 {
            source(client, manager, &[long.as_str()]);
        }
    });
}

/// Two clients of one compartment, each with a window and a data device:
/// the copier's window at 48,48 and the paster's above it at 80,80.
struct Pair {
    copier: Client,
    copier_manager: u32,
    copier_device: u32,
    paster: Client,
    paster_device: u32,
    _listener: crowbar_probe::CompartmentListener,
}

fn pair(compositor: &Compositor) -> Pair {
    let listener = compositor.register("test.principal", "c1");
    let mut copier = connect(&listener.path).unwrap();
    let mut paster = connect(&listener.path).unwrap();
    let (copier_manager, copier_device) = data_device(&mut copier);
    let (_, paster_device) = data_device(&mut paster);
    copier
        .open_toplevel("copier", "test.copier", (300, 200), 0xffff_0000)
        .unwrap();
    paster
        .open_toplevel("paster", "test.paster", (300, 200), 0xff00_00ff)
        .unwrap();
    copier.roundtrip().unwrap();
    paster.roundtrip().unwrap();
    Pair {
        copier,
        copier_manager,
        copier_device,
        paster,
        paster_device,
        _listener: listener,
    }
}

/// The offers a client was last given on a device: the selection's, if any,
/// and the MIME types each offer named.
fn last_selection(client: &mut Client, device: u32) -> Option<(u32, Vec<String>)> {
    client.roundtrip().unwrap();
    let selection = client
        .events
        .iter()
        .rev()
        .find(|event| event.object == device && event.name == "selection")?;
    let offer = match selection.args.as_slice() {
        [Arg::Object(Some(offer))] => offer.0,
        [Arg::Object(None)] => return None,
        other => panic!("{other:?}"),
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
fn a_selection_is_pasted_inside_one_compartment() {
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);
    let copied = source(&mut pair.copier, pair.copier_manager, &["text/plain;charset=utf-8"]);
    set_selection(&mut pair.copier, pair.copier_device, Some(copied));
    pair.copier.roundtrip().unwrap();

    // A click gives the paster's window the keyboard, and the selection
    // with it.
    compositor.click(200, 200);
    let (offer, mime_types) = last_selection(&mut pair.paster, pair.paster_device).expect("an offer");
    assert_eq!(mime_types, ["text/plain;charset=utf-8"]);

    let (mut read_end, write_end) = std::io::pipe().unwrap();
    pair.paster
        .request(
            offer,
            wl_data_offer::Request::Receive {
                mime_type: "text/plain;charset=utf-8".into(),
                fd: write_end.into(),
            },
        )
        .unwrap();
    pair.paster.roundtrip().unwrap();
    pair.copier.roundtrip().unwrap();
    let send = pair
        .copier
        .take_event(copied, "send")
        .expect("the copier was asked for its data");
    let [Arg::String(Some(mime_type)), Arg::Fd(fd)] = send.args.as_slice() else {
        panic!("{:?}", send.args);
    };
    assert_eq!(mime_type, "text/plain;charset=utf-8");
    std::fs::File::from(fd.try_clone().unwrap())
        .write_all(b"copied words")
        .unwrap();
    drop(send);

    let mut pasted = String::new();
    read_end.read_to_string(&mut pasted).unwrap();
    assert_eq!(pasted, "copied words");
}

#[test]
fn a_selection_set_while_a_client_holds_the_keyboard_is_offered_at_once() {
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);
    compositor.click(200, 200);
    assert!(last_selection(&mut pair.paster, pair.paster_device).is_none());

    let copied = source(&mut pair.copier, pair.copier_manager, &["text/plain"]);
    set_selection(&mut pair.copier, pair.copier_device, Some(copied));
    pair.copier.roundtrip().unwrap();

    assert!(last_selection(&mut pair.paster, pair.paster_device).is_some());

    // When the copier lets its source go, the selection is gone too.
    pair.copier.request(copied, wl_data_source::Request::Destroy).unwrap();
    pair.copier.roundtrip().unwrap();
    assert!(last_selection(&mut pair.paster, pair.paster_device).is_none());
}

#[test]
fn a_selection_s_offer_takes_no_part_in_drag_and_drop() {
    for finish in [true, false] {
        let compositor = Compositor::start();
        let mut pair = pair(&compositor);
        let copied = source(&mut pair.copier, pair.copier_manager, &["text/plain"]);
        set_selection(&mut pair.copier, pair.copier_device, Some(copied));
        pair.copier.roundtrip().unwrap();
        compositor.click(200, 200);
        let (offer, _) = last_selection(&mut pair.paster, pair.paster_device).unwrap();

        let request = if finish {
            wl_data_offer::Request::Finish
        } else {
            wl_data_offer::Request::SetActions {
                dnd_actions: 1,
                preferred_action: 1,
            }
        };
        pair.paster.request(offer, request).unwrap();

        assert!(pair.paster.roundtrip().is_err());
        let expected = if finish {
            wl_data_offer::error::INVALID_FINISH
        } else {
            wl_data_offer::error::INVALID_OFFER
        };
        assert_eq!(pair.paster.error.clone().unwrap().code, expected);
    }
}

#[test]
fn a_client_that_keeps_every_offer_is_offered_no_more_past_its_bound() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let (manager, device) = data_device(&mut client);
    client
        .open_toplevel("app", "test.app", (300, 200), 0xffff_ffff)
        .unwrap();
    compositor.click(60, 60);
    client.roundtrip().unwrap();

    for _ in 0..crowbar_compositor::data_device::CLIENT_OFFERS + 4 {
        let copied = source(&mut client, manager, &["text/plain"]);
        set_selection(&mut client, device, Some(copied));
    }
    client.roundtrip().unwrap();

    let offers = client
        .events
        .iter()
        .filter(|event| event.object == device && event.name == "data_offer")
        .count();
    assert_eq!(offers, crowbar_compositor::data_device::CLIENT_OFFERS);
    assert!(
        last_selection(&mut client, device).is_none(),
        "past the bound it is offered none"
    );
}
