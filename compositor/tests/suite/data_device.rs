//! Copy and paste and drag and drop work between the shell and every app,
//! as on any desktop (ADR 0032).

use crate::common;

use std::io::{Read, Write};

use common::Compositor;
use crowbar_compositor::{
    FRAME_FOCUSED,
    data_device::{CLIENT_MIME_BYTES, SOURCE_MIME_TYPES},
};
use crowbar_probe::Client;
use fulcrum_wire::{
    Arg, Fixed, ObjectId,
    protocol::wayland::{
        wl_data_device,
        wl_data_device_manager::{self, dnd_action},
        wl_data_offer, wl_data_source, wl_display, wl_pointer, wl_seat, wl_surface,
    },
};

const BTN_LEFT: u32 = 0x110;
const DRAG_MIME: &str = "text/plain;charset=utf-8";

/// The data device manager and a data device for the seat.
fn data_device(client: &mut Client) -> (u32, u32) {
    data_device_version(client, 3)
}

fn data_device_version(client: &mut Client, version: u32) -> (u32, u32) {
    let manager = client.bind(wl_data_device_manager::NAME, version).unwrap();
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

/// The shell and the apps share one selection: what an app copies is
/// offered to the shell when it has the keyboard, and what the shell copies
/// is offered to an app.
#[test]
fn the_shell_and_an_app_share_one_selection_both_ways() {
    let compositor = Compositor::start();
    // The app's window at 48,48, and the shell's above it at 80,80.
    let mut pair = pair_of(compositor.app(), compositor.shell(), 3);
    let (app, app_device) = (&mut pair.copier, pair.copier_device);
    let copied = source(app, pair.copier_manager, &["text/plain;charset=utf-8"]);
    set_selection(app, app_device, Some(copied));
    app.roundtrip().unwrap();

    // The shell's window took the keyboard as it opened, so it is offered
    // the app's selection at once.
    let (_, mime_types) = last_selection(&mut pair.paster, pair.paster_device).expect("the shell is offered it");
    assert_eq!(mime_types, ["text/plain;charset=utf-8"]);

    // The shell copies, which replaces the app's selection.
    let (shell_manager, _) = data_device(&mut pair.paster);
    let from_shell = source(&mut pair.paster, shell_manager, &["text/uri-list"]);
    set_selection(&mut pair.paster, pair.paster_device, Some(from_shell));
    pair.paster.roundtrip().unwrap();
    assert_eq!(seen(&mut pair.copier, "wl_data_source"), [(copied, "cancelled")]);

    // A click gives the app's window the keyboard, and the shell's
    // selection with it.
    compositor.click(60, 60);
    let (_, mime_types) = last_selection(&mut pair.copier, pair.copier_device).expect("the app is offered it");
    assert_eq!(mime_types, ["text/uri-list"]);
}

#[test]
fn a_new_selection_cancels_the_one_it_replaces() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
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
fn a_drag_without_an_implicit_pointer_grab_is_cancelled() {
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let (manager, device) = data_device(&mut client);
    let window = client
        .open_toplevel("app", "test.app", (300, 200), 0xffff_ffff)
        .unwrap();
    let dragged = source(&mut client, manager, &["text/uri-list"]);

    drag_request(&mut client, device, dragged, window.surface, None, 0);

    assert_eq!(seen(&mut client, "wl_data_source"), [(dragged, "cancelled")]);
}

#[test]
fn a_held_button_does_not_authorize_a_drag_from_the_wrong_surface() {
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);
    let other_surface = pair.copier.create_surface().unwrap();
    let serial = press_on(&compositor, &mut pair.copier, 60, 60);
    let dragged = source(&mut pair.copier, pair.copier_manager, &[DRAG_MIME]);
    drag_request(
        &mut pair.copier,
        pair.copier_device,
        dragged,
        other_surface,
        None,
        serial,
    );

    assert_eq!(seen(&mut pair.copier, wl_data_source::NAME), [(dragged, "cancelled")]);
    release_drag_button(&compositor);
}

#[test]
fn dragging_requires_the_serial_of_the_implicit_grab() {
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);
    let serial = press_on(&compositor, &mut pair.copier, 60, 60);
    let dragged = source(&mut pair.copier, pair.copier_manager, &[DRAG_MIME]);
    drag_request(
        &mut pair.copier,
        pair.copier_device,
        dragged,
        pair.copier_surface,
        None,
        serial.wrapping_add(1),
    );

    assert_eq!(seen(&mut pair.copier, wl_data_source::NAME), [(dragged, "cancelled")]);
    release_drag_button(&compositor);
}

/// Expects the compositor to refuse what `send` does with `code`.
fn refused(code: u32, send: impl FnOnce(&mut Client, u32, u32)) {
    let compositor = Compositor::start();
    let mut client = compositor.app();
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
    let exceed_budget = |client: &mut Client, object, mime_type| {
        client.roundtrip().unwrap();
        // The fatal error can reach the client during this write or during
        // the next roundtrip. The refusal check verifies the error code.
        let _ = client.request(object, wl_data_source::Request::Offer { mime_type });
    };
    refused(wl_display::error::NO_MEMORY, |client, manager, _| {
        let many: Vec<String> = (0..SOURCE_MIME_TYPES)
            .map(|n| format!("application/x-test-{n}"))
            .collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let full = source(client, manager, &many);
        exceed_budget(client, full, "application/x-over-limit".into());
    });
    refused(wl_display::error::NO_MEMORY, |client, manager, _| {
        // Under the count for each source, over the bytes for the client.
        let long = format!("application/x-{}", "a".repeat(3000));
        for _ in 0..CLIENT_MIME_BYTES / long.len() {
            source(client, manager, &[&long]);
        }
        let extra = source(client, manager, &[]);
        exceed_budget(client, extra, long);
    });
}

/// Two apps, each with a window and a data device: the copier's window at 48,48 and the paster's above it at 80,80.
struct Pair {
    copier: Client,
    copier_manager: u32,
    copier_device: u32,
    copier_surface: u32,
    paster: Client,
    paster_device: u32,
    paster_surface: u32,
}

fn pair(compositor: &Compositor) -> Pair {
    pair_for_target_version(compositor, 3)
}

fn pair_for_target_version(compositor: &Compositor, target_version: u32) -> Pair {
    pair_of(compositor.app(), compositor.app(), target_version)
}

/// A pair of any two clients, which may be the shell.
fn pair_of(mut copier: Client, mut paster: Client, target_version: u32) -> Pair {
    let (copier_manager, copier_device) = data_device(&mut copier);
    let (_, paster_device) = data_device_version(&mut paster, target_version);
    let copying = copier
        .open_toplevel("copier", "test.copier", (300, 200), 0xffff_0000)
        .unwrap();
    let pasting = paster
        .open_toplevel("paster", "test.paster", (300, 200), 0xff00_00ff)
        .unwrap();
    copier.roundtrip().unwrap();
    paster.roundtrip().unwrap();
    Pair {
        copier,
        copier_manager,
        copier_device,
        copier_surface: copying.surface,
        paster,
        paster_device,
        paster_surface: pasting.surface,
    }
}

fn press_on(compositor: &Compositor, client: &mut Client, x: i32, y: i32) -> u32 {
    client.listen_to_pointer().unwrap();
    client.roundtrip().unwrap();
    compositor.pointer(x, y);
    compositor.with(|state| state.pointer_button(BTN_LEFT, true, 0));
    client
        .take_pointer_events()
        .unwrap()
        .into_iter()
        .find_map(|event| match (event.name, event.args.as_slice()) {
            (
                "button",
                [
                    Arg::Uint(serial),
                    _,
                    Arg::Uint(BTN_LEFT),
                    Arg::Uint(wl_pointer::button_state::PRESSED),
                ],
            ) => Some(*serial),
            _ => None,
        })
        .expect("the pointer press reached the origin surface")
}

fn release_drag_button(compositor: &Compositor) {
    compositor.with(|state| state.pointer_button(BTN_LEFT, false, 0));
}

fn drag_request(client: &mut Client, device: u32, source: u32, origin: u32, icon: Option<u32>, serial: u32) {
    client
        .request(
            device,
            wl_data_device::Request::StartDrag {
                source: Some(ObjectId(source)),
                origin: ObjectId(origin),
                icon: icon.map(ObjectId),
                serial,
            },
        )
        .unwrap();
}

impl Pair {
    fn start_drag(&mut self, compositor: &Compositor, actions: u32, icon: Option<u32>) -> u32 {
        let serial = press_on(compositor, &mut self.copier, 60, 60);
        let dragged = source(&mut self.copier, self.copier_manager, &[DRAG_MIME]);
        self.copier
            .request(dragged, wl_data_source::Request::SetActions { dnd_actions: actions })
            .unwrap();
        drag_request(
            &mut self.copier,
            self.copier_device,
            dragged,
            self.copier_surface,
            icon,
            serial,
        );
        self.copier.roundtrip().unwrap();
        dragged
    }

    /// Pressing on the origin raised it. Enter the strip of the target
    /// window which is still visible at the right.
    fn drag_to_paster(&mut self, compositor: &Compositor) -> (u32, u32) {
        compositor.pointer(360, 100);
        self.paster.roundtrip().unwrap();
        let enter = self
            .paster
            .take_event(self.paster_device, "enter")
            .expect("the drop target was entered");
        let [
            Arg::Uint(serial),
            Arg::Object(Some(surface)),
            Arg::Fixed(x),
            Arg::Fixed(y),
            Arg::Object(Some(offer)),
        ] = enter.args.as_slice()
        else {
            panic!("unexpected drag enter: {enter:?}");
        };
        assert_eq!(surface.0, self.paster_surface);
        assert_eq!((*x, *y), (Fixed::from_int(280), Fixed::from_int(20)));
        (*serial, offer.0)
    }
}

fn offer_actions(client: &mut Client, offer: u32, actions: u32, preferred: u32) {
    client
        .request(
            offer,
            wl_data_offer::Request::SetActions {
                dnd_actions: actions,
                preferred_action: preferred,
            },
        )
        .unwrap();
}

fn accept_drag(client: &mut Client, serial: u32, offer: u32) {
    client
        .request(
            offer,
            wl_data_offer::Request::Accept {
                serial,
                mime_type: Some(DRAG_MIME.into()),
            },
        )
        .unwrap();
}

fn assert_action(client: &mut Client, object: u32, expected: u32) {
    client.roundtrip().unwrap();
    let action = client
        .events
        .iter()
        .rev()
        .find(|event| event.object == object && event.name == "action")
        .expect("the chosen action was announced");
    assert!(matches!(action.args.as_slice(), [Arg::Uint(v)] if *v == expected));
}

#[test]
fn a_drag_and_drop_moves_data_from_one_app_to_another() {
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);
    pair.paster.listen_to_pointer().unwrap();
    pair.paster.roundtrip().unwrap();
    let actions = dnd_action::COPY | dnd_action::MOVE;
    let dragged = pair.start_drag(&compositor, actions, None);
    let (serial, offer) = pair.drag_to_paster(&compositor);

    let types = pair.paster.take_event(offer, "offer").expect("the offered MIME type");
    assert!(matches!(types.args.as_slice(), [Arg::String(Some(mime))] if mime == DRAG_MIME));
    let actions_event = pair.paster.take_event(offer, "source_actions").unwrap();
    assert!(matches!(actions_event.args.as_slice(), [Arg::Uint(v)] if *v == actions));
    compositor.pointer(364, 103);
    assert!(pair.paster.take_pointer_events().unwrap().is_empty());
    let motion = pair.paster.take_event(pair.paster_device, "motion").unwrap();
    assert!(
        matches!(motion.args.as_slice(), [Arg::Uint(_), Arg::Fixed(x), Arg::Fixed(y)]
        if (*x, *y) == (Fixed::from_int(284), Fixed::from_int(23)))
    );
    let origin_events = pair.copier.take_pointer_events().unwrap();
    assert_eq!(
        origin_events.iter().map(|event| event.name).collect::<Vec<_>>(),
        ["leave"]
    );

    accept_drag(&mut pair.paster, serial, offer);
    offer_actions(&mut pair.paster, offer, actions, dnd_action::MOVE);
    assert_action(&mut pair.paster, offer, dnd_action::MOVE);
    assert_action(&mut pair.copier, dragged, dnd_action::MOVE);
    assert!(pair.copier.events.iter().any(|event| {
        event.object == dragged
            && event.name == "target"
            && matches!(event.args.as_slice(), [Arg::String(Some(v))] if v == DRAG_MIME)
    }));

    release_drag_button(&compositor);
    pair.paster.roundtrip().unwrap();
    pair.copier.roundtrip().unwrap();
    assert!(pair.paster.take_event(pair.paster_device, "drop").is_some());
    assert!(pair.copier.take_event(dragged, "dnd_drop_performed").is_some());
    assert!(pair.copier.take_event(dragged, "dnd_finished").is_none());

    let (mut read_end, write_end) = std::io::pipe().unwrap();
    pair.paster
        .request(
            offer,
            wl_data_offer::Request::Receive {
                mime_type: DRAG_MIME.into(),
                fd: write_end.into(),
            },
        )
        .unwrap();
    pair.paster.roundtrip().unwrap();
    pair.copier.roundtrip().unwrap();
    let send = pair
        .copier
        .take_event(dragged, "send")
        .expect("the source was asked for data");
    let [Arg::String(Some(mime)), Arg::Fd(fd)] = send.args.as_slice() else {
        panic!("unexpected send: {send:?}");
    };
    assert_eq!(mime, DRAG_MIME);
    std::fs::File::from(fd.try_clone().unwrap())
        .write_all(b"dragged words")
        .unwrap();
    drop(send);
    let mut received = String::new();
    read_end.read_to_string(&mut received).unwrap();
    assert_eq!(received, "dragged words");

    pair.paster.request(offer, wl_data_offer::Request::Finish).unwrap();
    pair.paster.request(offer, wl_data_offer::Request::Destroy).unwrap();
    pair.paster.roundtrip().unwrap();
    pair.copier.roundtrip().unwrap();
    assert!(pair.copier.take_event(dragged, "dnd_finished").is_some());
    assert!(pair.copier.take_event(dragged, "cancelled").is_none());
}

#[test]
fn a_drop_with_ask_finishes_with_the_chosen_action() {
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);
    let actions = dnd_action::COPY | dnd_action::MOVE | dnd_action::ASK;
    let dragged = pair.start_drag(&compositor, actions, None);
    let (serial, offer) = pair.drag_to_paster(&compositor);
    accept_drag(&mut pair.paster, serial, offer);
    offer_actions(&mut pair.paster, offer, actions, dnd_action::ASK);
    assert_action(&mut pair.paster, offer, dnd_action::ASK);
    assert_action(&mut pair.copier, dragged, dnd_action::ASK);

    release_drag_button(&compositor);
    pair.paster.roundtrip().unwrap();
    assert!(pair.paster.take_event(pair.paster_device, "drop").is_some());
    offer_actions(&mut pair.paster, offer, actions, dnd_action::COPY);
    pair.paster.request(offer, wl_data_offer::Request::Finish).unwrap();
    pair.paster.roundtrip().unwrap();

    assert_action(&mut pair.copier, dragged, dnd_action::COPY);
    assert!(pair.copier.take_event(dragged, "dnd_finished").is_some());
    assert_action(&mut pair.paster, offer, dnd_action::ASK);
}

/// A drag from an app drops on the shell, and a drag from the shell drops
/// on an app.
#[test]
fn a_drag_drops_on_the_shell_and_from_the_shell_on_an_app() {
    for shell_drags in [false, true] {
        let compositor = Compositor::start();
        let mut pair = if shell_drags {
            pair_of(compositor.shell(), compositor.app(), 3)
        } else {
            pair_of(compositor.app(), compositor.shell(), 3)
        };
        let dragged = pair.start_drag(&compositor, dnd_action::COPY, None);
        let (serial, offer) = pair.drag_to_paster(&compositor);
        accept_drag(&mut pair.paster, serial, offer);
        offer_actions(&mut pair.paster, offer, dnd_action::COPY, dnd_action::COPY);
        pair.paster.roundtrip().unwrap();

        release_drag_button(&compositor);
        pair.paster.roundtrip().unwrap();
        pair.copier.roundtrip().unwrap();
        assert!(
            pair.paster.take_event(pair.paster_device, "drop").is_some(),
            "no drop, shell drags: {shell_drags}"
        );
        assert!(pair.copier.take_event(dragged, "dnd_drop_performed").is_some());
        assert!(pair.copier.take_event(dragged, "cancelled").is_none());
    }
}

/// When the pointer leaves the target, its offer no longer reads, and a
/// release over no target cancels the drag.
#[test]
fn an_offer_left_behind_reads_nothing_and_a_drop_on_nothing_is_cancelled() {
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);

    let dragged = pair.start_drag(&compositor, dnd_action::COPY, None);
    let (_, old_offer) = pair.drag_to_paster(&compositor);
    compositor.pointer(1200, 700);
    pair.paster.roundtrip().unwrap();
    assert!(pair.paster.take_event(pair.paster_device, "leave").is_some());
    // Keeping the previous offer cannot let the old target continue
    // receiving after the pointer has left it.
    let (_read_end, write_end) = std::io::pipe().unwrap();
    pair.paster
        .request(
            old_offer,
            wl_data_offer::Request::Receive {
                mime_type: DRAG_MIME.into(),
                fd: write_end.into(),
            },
        )
        .unwrap();
    let _ = pair.paster.roundtrip();
    pair.copier.roundtrip().unwrap();
    assert!(pair.copier.take_event(dragged, "send").is_none());

    release_drag_button(&compositor);
    pair.copier.roundtrip().unwrap();
    assert!(pair.copier.take_event(dragged, "cancelled").is_some());
}

#[test]
fn a_drop_without_an_accepted_mime_type_is_cancelled() {
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);
    let dragged = pair.start_drag(&compositor, dnd_action::COPY, None);
    let (_, offer) = pair.drag_to_paster(&compositor);
    offer_actions(&mut pair.paster, offer, dnd_action::COPY, dnd_action::COPY);
    assert_action(&mut pair.paster, offer, dnd_action::COPY);
    release_drag_button(&compositor);

    pair.paster.roundtrip().unwrap();
    assert!(pair.paster.take_event(pair.paster_device, "drop").is_none());
    pair.copier.roundtrip().unwrap();
    assert!(pair.copier.take_event(dragged, "cancelled").is_some());
}

#[test]
fn a_version_two_target_can_drop_without_action_negotiation() {
    let compositor = Compositor::start();
    let mut pair = pair_for_target_version(&compositor, 2);
    let dragged = pair.start_drag(&compositor, dnd_action::COPY, None);
    let (_, offer) = pair.drag_to_paster(&compositor);
    assert!(pair.paster.take_event(offer, "source_actions").is_none());
    assert_action(&mut pair.copier, dragged, dnd_action::COPY);

    release_drag_button(&compositor);
    pair.paster.roundtrip().unwrap();
    assert!(pair.paster.take_event(pair.paster_device, "drop").is_some());

    // The old client destroys its offer after receiving data. Version 2
    // doesn't have a separate finish request.
    pair.paster.request(offer, wl_data_offer::Request::Destroy).unwrap();
    pair.paster.roundtrip().unwrap();
    pair.copier.roundtrip().unwrap();
    assert!(pair.copier.take_event(dragged, "dnd_finished").is_some());
    assert!(pair.copier.take_event(dragged, "cancelled").is_none());
}

#[test]
fn locking_the_session_cancels_an_active_drag() {
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);
    let dragged = pair.start_drag(&compositor, dnd_action::COPY, None);
    let (serial, offer) = pair.drag_to_paster(&compositor);
    accept_drag(&mut pair.paster, serial, offer);
    offer_actions(&mut pair.paster, offer, dnd_action::COPY, dnd_action::COPY);
    pair.paster.roundtrip().unwrap();

    compositor.with(|state| state.lock("test"));
    release_drag_button(&compositor);
    pair.paster.roundtrip().unwrap();
    assert!(pair.paster.take_event(pair.paster_device, "leave").is_some());
    assert!(pair.paster.take_event(pair.paster_device, "drop").is_none());
    pair.copier.roundtrip().unwrap();
    assert!(pair.copier.take_event(dragged, "cancelled").is_some());
}

#[test]
fn unmapping_the_origin_cancels_the_drag() {
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);
    let dragged = pair.start_drag(&compositor, dnd_action::COPY, None);
    pair.drag_to_paster(&compositor);
    pair.copier.show(pair.copier_surface, None).unwrap();
    pair.copier.roundtrip().unwrap();
    pair.paster.roundtrip().unwrap();

    assert!(pair.copier.take_event(dragged, "cancelled").is_some());
    assert!(pair.paster.take_event(pair.paster_device, "leave").is_some());
    release_drag_button(&compositor);
}

#[test]
fn a_drag_icon_follows_the_pointer_and_covers_no_frame() {
    const ICON: u32 = 0xff00_ff00;
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);
    let icon = pair.copier.create_surface().unwrap();
    pair.start_drag(&compositor, dnd_action::COPY, Some(icon));
    let painted = pair.copier.shm_buffer((24, 24), ICON).unwrap();
    pair.copier.show(icon, Some(&painted)).unwrap();
    pair.copier.commit_unchanged(icon).unwrap();
    pair.copier.wait_until(|client| client.frames_done == 1).unwrap();

    let render = || {
        compositor.with(|state| {
            let number = state.outputs()[0].number;
            state.render_output_with_cursor(number).unwrap()
        })
    };
    compositor.pointer(44, 92);
    let image = render();
    assert_eq!(image.pixel(64, 112), Some(ICON));
    assert_eq!(image.pixel(47, 112), Some(FRAME_FOCUSED));

    for _ in 0..2 {
        pair.copier
            .request(icon, wl_surface::Request::Offset { x: 10, y: 0 })
            .unwrap();
        pair.copier.request(icon, wl_surface::Request::Commit).unwrap();
    }
    pair.copier.roundtrip().unwrap();
    assert_eq!(render().pixel(80, 112), Some(ICON));

    release_drag_button(&compositor);
    assert_eq!(render().pixel(80, 112), Some(0xffff_0000));
}

#[test]
fn a_drag_offer_cannot_finish_before_the_drop() {
    let compositor = Compositor::start();
    let mut pair = pair(&compositor);
    pair.start_drag(&compositor, dnd_action::COPY, None);
    let (serial, offer) = pair.drag_to_paster(&compositor);
    accept_drag(&mut pair.paster, serial, offer);
    offer_actions(&mut pair.paster, offer, dnd_action::COPY, dnd_action::COPY);
    pair.paster.request(offer, wl_data_offer::Request::Finish).unwrap();

    assert!(pair.paster.roundtrip().is_err());
    assert_eq!(
        pair.paster.error.clone().unwrap().code,
        wl_data_offer::error::INVALID_FINISH
    );
}

#[test]
fn a_drag_offer_rejects_an_action_mask_and_multiple_preferred_actions() {
    for (actions, preferred, error) in [
        (8, dnd_action::NONE, wl_data_offer::error::INVALID_ACTION_MASK),
        (
            dnd_action::COPY | dnd_action::MOVE,
            dnd_action::COPY | dnd_action::MOVE,
            wl_data_offer::error::INVALID_ACTION,
        ),
    ] {
        let compositor = Compositor::start();
        let mut pair = pair(&compositor);
        pair.start_drag(&compositor, dnd_action::COPY, None);
        let (_, offer) = pair.drag_to_paster(&compositor);
        offer_actions(&mut pair.paster, offer, actions, preferred);

        assert!(pair.paster.roundtrip().is_err());
        assert_eq!(pair.paster.error.clone().unwrap().code, error);
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
fn a_selection_copied_in_one_app_is_pasted_in_another() {
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
    let mut client = compositor.app();
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
