//! The data device before slice 4: the protocol works and nothing moves
//! between clients. A selection is offered to no one, and a drag ends as it
//! starts.

mod common;

use common::Compositor;
use crowbar_probe::Client;
use fulcrum_wire::{
    ObjectId,
    protocol::wayland::{wl_data_device, wl_data_device_manager, wl_data_source, wl_display, wl_seat},
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
    let (_, _) = data_device(&mut other);
    other
        .open_toplevel("other", "test.other", (300, 200), 0xffff_ffff)
        .unwrap();

    let copied = source(&mut copier, manager, &["text/plain;charset=utf-8"]);
    set_selection(&mut copier, device, Some(copied));
    compositor.click(60, 60);

    assert_eq!(seen(&mut other, "wl_data_device"), []);
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
