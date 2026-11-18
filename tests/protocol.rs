//! What the compositor does with what a client sends: the requests it
//! carries out, and the ones it refuses. A refusal is `wl_display.error`,
//! then the end of that client's connection. Every other client goes on
//! being served.

mod common;

use common::{Compositor, OUTPUT_SIZE};
use crowbar_probe::Client;
use fulcrum_wire::{
    Arg, ArgWriter, DynamicNewId, Header, NewId,
    codec::MAX_MESSAGE_SIZE,
    protocol::{
        linux_dmabuf_v1::zwp_linux_dmabuf_v1,
        wayland::{wl_display, wl_output, wl_registry},
    },
};

/// Sends a request and expects the compositor to refuse it with `code`.
fn refused(compositor: &Compositor, code: u32, send: impl FnOnce(&mut Client)) -> String {
    let (mut client, _listener) = compositor.compartment(&format!("c{}", rand_id()));
    send(&mut client);
    let result = client.roundtrip();
    assert!(result.is_err(), "the compositor accepted it");
    let error = client.error.clone().expect("a protocol error, not a closed socket");
    assert_eq!(error.code, code, "{error:?}");
    // The compositor goes on serving everyone else.
    compositor.shell().roundtrip().unwrap();
    error.message
}

fn rand_id() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[test]
fn a_roundtrip_answers_and_frees_the_callback() {
    let compositor = Compositor::start();
    let mut client = compositor.shell();
    for _ in 0..100 {
        client.roundtrip().unwrap();
    }
    assert!(client.error.is_none());
}

#[test]
fn an_output_says_what_it_is_when_bound() {
    let compositor = Compositor::start();
    let mut client = compositor.shell();
    let output = client.bind(wl_output::NAME, 4).unwrap();
    client.roundtrip().unwrap();
    let names: Vec<&str> = client
        .events
        .iter()
        .filter(|event| event.object == output)
        .map(|event| event.name)
        .collect();
    assert_eq!(names, ["geometry", "mode", "scale", "name", "description", "done"]);
    let mode = client.events.iter().find(|event| event.name == "mode").unwrap();
    assert!(matches!(
        mode.args.as_slice(),
        [Arg::Uint(_), Arg::Int(width), Arg::Int(height), Arg::Int(60_000)]
            if (*width, *height) == OUTPUT_SIZE
    ));
}

#[test]
fn a_request_on_an_object_that_does_not_exist_is_refused() {
    let compositor = Compositor::start();
    let message = refused(&compositor, wl_display::error::INVALID_OBJECT, |client| {
        client.request(42, wl_output::Request::Release).unwrap();
    });
    assert!(message.contains("invalid object 42"), "{message}");
}

#[test]
fn an_opcode_the_interface_does_not_have_is_refused() {
    let compositor = Compositor::start();
    refused(&compositor, wl_display::error::INVALID_METHOD, |client| {
        let bytes = Header {
            object: 1,
            opcode: 9,
            size: 8,
        }
        .encode()
        .to_vec();
        client.send_raw(bytes, Vec::new()).unwrap();
    });
}

#[test]
fn a_request_newer_than_the_objects_version_is_refused() {
    let compositor = Compositor::start();
    refused(&compositor, wl_display::error::INVALID_METHOD, |client| {
        // wl_output.release is version 3; bind version 2.
        let output = client.bind(wl_output::NAME, 2).unwrap();
        client.request(output, wl_output::Request::Release).unwrap();
    });
}

#[test]
fn malformed_arguments_are_refused() {
    let compositor = Compositor::start();
    refused(&compositor, wl_display::error::INVALID_METHOD, |client| {
        // wl_registry.bind with a string length past the message's end.
        let mut out = ArgWriter::default();
        out.uint(1);
        out.uint(400);
        let (bytes, _) = out.finish(2, 0).unwrap();
        client.send_raw(bytes, Vec::new()).unwrap();
    });
}

#[test]
fn a_header_with_an_impossible_size_is_refused() {
    let compositor = Compositor::start();
    refused(&compositor, wl_display::error::INVALID_METHOD, |client| {
        let mut bytes = Header {
            object: 1,
            opcode: 0,
            size: 8,
        }
        .encode()
        .to_vec();
        bytes[4..8].copy_from_slice(&(((MAX_MESSAGE_SIZE as u32) + 4) << 16).to_ne_bytes());
        client.send_raw(bytes, Vec::new()).unwrap();
    });
}

#[test]
fn a_new_object_with_an_id_in_use_or_in_the_servers_range_is_refused() {
    let compositor = Compositor::start();
    for id in [1u32, 2, 0xff00_0001] {
        refused(&compositor, wl_display::error::INVALID_OBJECT, |client| {
            client
                .request(1, wl_display::Request::Sync { callback: NewId(id) })
                .unwrap();
        });
    }
}

#[test]
fn a_bind_that_misnames_the_interface_or_its_version_is_refused() {
    let compositor = Compositor::start();
    let shell = compositor.shell();
    let output = shell
        .globals
        .iter()
        .find(|global| global.interface == wl_output::NAME)
        .unwrap()
        .name;
    for (interface, version) in [("wl_compositor", 1), ("wl_output", 0), ("wl_output", 99)] {
        refused(&compositor, wl_display::error::INVALID_OBJECT, |client| {
            client
                .request(
                    2,
                    wl_registry::Request::Bind {
                        name: output,
                        id: DynamicNewId {
                            interface: interface.into(),
                            version,
                            id: 100,
                        },
                    },
                )
                .unwrap();
        });
    }
}

/// A global this build advertises but does not carry out yet says so, and
/// never pretends to work. The tests offer linux-dmabuf, which the shell may
/// bind, before Milestone 3 carries it out.
#[test]
fn a_request_this_build_does_not_carry_out_is_an_implementation_error() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    let dmabuf = shell.bind(zwp_linux_dmabuf_v1::NAME, 4).unwrap();
    let params = shell.new_id("zwp_linux_buffer_params_v1");
    shell
        .request(dmabuf, zwp_linux_dmabuf_v1::Request::CreateParams { params_id: params })
        .unwrap();

    assert!(shell.roundtrip().is_err(), "the compositor accepted it");
    let error = shell.error.clone().expect("a protocol error, not a closed socket");
    assert_eq!(error.code, wl_display::error::IMPLEMENTATION, "{error:?}");
    assert!(error.message.contains("not carried out"), "{error:?}");
    compositor.warden().roundtrip().unwrap();
}

/// A client that sends requests and reads no answers fills its queue, and
/// the compositor lets go of it rather than hold ever more for it.
#[test]
fn a_client_that_never_reads_is_disconnected_and_others_are_served() {
    let compositor = Compositor::start();
    let (mut flooder, _listener) = compositor.compartment("flood");
    let mut sent = 0u32;
    let disconnected = loop {
        let mut batch = Vec::new();
        for _ in 0..1000 {
            let (bytes, _) = fulcrum_wire::Encode::to_message(
                wl_display::Request::Sync {
                    callback: NewId(3 + sent),
                },
                1,
            )
            .unwrap();
            batch.extend(bytes);
            sent += 1;
        }
        if flooder.send_raw(batch, Vec::new()).is_err() {
            break true;
        }
        if sent > 200_000 {
            break false;
        }
    };
    assert!(
        disconnected,
        "the compositor kept every answer for a client that never read"
    );
    compositor.shell().roundtrip().unwrap();
}

/// Descriptors a client sends that no request takes cannot pile up.
#[test]
fn a_client_that_floods_descriptors_is_disconnected() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("fds");
    let mut refused = false;
    for _ in 0..40 {
        let fds: Vec<_> = (0..28).map(|_| rustix::pipe::pipe().unwrap().0).collect();
        let (bytes, _) = fulcrum_wire::Encode::to_message(
            wl_display::Request::Sync {
                callback: NewId(1000 + rand_id()),
            },
            1,
        )
        .unwrap();
        if client.send_raw(bytes, fds).is_err() {
            refused = true;
            break;
        }
    }
    let gone = refused || client.roundtrip().is_err();
    assert!(gone, "the compositor held more than its limit of unused descriptors");
    compositor.shell().roundtrip().unwrap();
}

#[test]
fn random_bytes_from_a_client_never_stop_the_compositor() {
    let compositor = Compositor::start();
    let mut seed = 0x1234_5678_9abc_def0u64;
    for _ in 0..30 {
        let (mut client, _listener) = compositor.compartment(&format!("noise{}", rand_id()));
        let bytes: Vec<u8> = (0..256)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect();
        let _ = client.send_raw(bytes, Vec::new());
        let _ = client.roundtrip();
    }
    compositor.shell().roundtrip().unwrap();
}
