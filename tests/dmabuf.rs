//! linux-dmabuf, as a client of a GPU sees it: feedback that says what the
//! compositor takes, and buffers made from dma-bufs that are shown like any
//! other.
//!
//! The buffers here are memfds, which exercise the CPU copy through
//! positioned reads. CI's machines have no GPU to make a real dma-buf.

mod common;

use common::{Compositor, TEST_DEVICE};
use crowbar_probe::Client;
use fulcrum_wire::{
    Arg, NewId, ObjectId,
    protocol::{
        linux_dmabuf_v1::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1, zwp_linux_dmabuf_v1},
        wayland::{wl_shm, wl_surface},
    },
};
use std::os::fd::OwnedFd;

const XR24: u32 = u32::from_le_bytes(*b"XR24");
const AR24: u32 = u32::from_le_bytes(*b"AR24");
const LINEAR: u64 = 0;
/// A tiled layout, as Intel's X tiling.
const TILED: u64 = 0x0100_0000_0000_0001;

/// A "dma-buf" of `width` x `height` pixels of one colour, rows `stride`
/// bytes apart.
fn dmabuf(width: i32, height: i32, stride: u32, argb: u32) -> OwnedFd {
    let fd = rustix::fs::memfd_create("test-dmabuf", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    let mut bytes = vec![0u8; (stride as usize) * (height as usize)];
    for row in bytes.chunks_mut(stride as usize) {
        for pixel in row[..(width as usize) * 4].chunks_mut(4) {
            pixel.copy_from_slice(&argb.to_le_bytes());
        }
    }
    rustix::io::write(&fd, &bytes).unwrap();
    fd
}

fn params(client: &mut Client) -> u32 {
    let dmabuf = client.bind(zwp_linux_dmabuf_v1::NAME, 4).unwrap();
    let params = client.new_id(zwp_linux_buffer_params_v1::NAME);
    client
        .request(dmabuf, zwp_linux_dmabuf_v1::Request::CreateParams { params_id: params })
        .unwrap();
    params.0
}

fn add(client: &mut Client, params: u32, plane: u32, fd: OwnedFd, stride: u32, modifier: u64) {
    client
        .request(
            params,
            zwp_linux_buffer_params_v1::Request::Add {
                fd,
                plane_idx: plane,
                offset: 0,
                stride,
                modifier_hi: (modifier >> 32) as u32,
                modifier_lo: modifier as u32,
            },
        )
        .unwrap();
}

/// A buffer made at once, with an ID the client chose.
fn create_immed(client: &mut Client, params: u32, size: (i32, i32), format: u32) -> u32 {
    let buffer = client.new_id("wl_buffer");
    client
        .request(
            params,
            zwp_linux_buffer_params_v1::Request::CreateImmed {
                buffer_id: buffer,
                width: size.0,
                height: size.1,
                format,
                flags: 0,
            },
        )
        .unwrap();
    buffer.0
}

fn show(client: &mut Client, surface: u32, buffer: u32) {
    client
        .request(
            surface,
            wl_surface::Request::Attach {
                buffer: Some(ObjectId(buffer)),
                x: 0,
                y: 0,
            },
        )
        .unwrap();
    client.request(surface, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();
}

#[test]
fn feedback_says_what_the_compositor_takes_on_which_device() {
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    let dmabuf = shell.bind(zwp_linux_dmabuf_v1::NAME, 4).unwrap();
    let feedback = shell.new_id(zwp_linux_dmabuf_feedback_v1::NAME);
    shell
        .request(
            dmabuf,
            zwp_linux_dmabuf_v1::Request::GetDefaultFeedback { id: feedback },
        )
        .unwrap();
    shell.roundtrip().unwrap();

    let events: Vec<_> = shell
        .events
        .drain(..)
        .filter(|event| event.object == feedback.0)
        .collect();
    let names: Vec<&str> = events.iter().map(|event| event.name).collect();
    assert_eq!(
        names,
        [
            "format_table",
            "main_device",
            "tranche_target_device",
            "tranche_flags",
            "tranche_formats",
            "tranche_done",
            "done"
        ]
    );
    let [Arg::Fd(table), Arg::Uint(size)] = events[0].args.as_slice() else {
        panic!("{:?}", events[0].args);
    };
    assert_eq!(*size, 32);
    let mut bytes = vec![0u8; 32];
    rustix::io::pread(table, &mut bytes, 0).unwrap();
    let entry = |at: usize| {
        (
            u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap()),
            u64::from_ne_bytes(bytes[at + 8..at + 16].try_into().unwrap()),
        )
    };
    assert_eq!([entry(0), entry(16)], [(XR24, LINEAR), (AR24, LINEAR)]);
    // The client can only read the table.
    assert!(rustix::io::pwrite(table, &[0], 0).is_err(), "the table can be written");
    assert!(matches!(&events[1].args[..], [Arg::Array(device)] if *device == TEST_DEVICE.to_ne_bytes()));
    assert!(matches!(&events[4].args[..], [Arg::Array(indices)] if *indices == [0, 0, 1, 0]));
}

#[test]
fn a_dmabuf_buffer_is_shown_like_shared_memory() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment_with_gpu("c1");
    let window = client
        .open_toplevel("dmabuf", "test.dmabuf", (64, 48), 0xff00_00ff)
        .unwrap();
    let params = params(&mut client);
    add(
        &mut client,
        params,
        0,
        dmabuf(64, 48, 64 * 4, 0xffc0_2020),
        64 * 4,
        LINEAR,
    );
    let buffer = create_immed(&mut client, params, (64, 48), XR24);

    show(&mut client, window.surface, buffer);

    let screen = compositor.screen(0);
    assert_eq!(screen.pixel(48 + 10, 48 + 10), Some(0xffc0_2020));
}

#[test]
fn a_buffer_the_compositor_names_is_shown_too() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment_with_gpu("c1");
    let window = client
        .open_toplevel("dmabuf", "test.dmabuf", (32, 32), 0xff00_00ff)
        .unwrap();
    let params = params(&mut client);
    add(
        &mut client,
        params,
        0,
        dmabuf(32, 32, 32 * 4, 0xff20_c020),
        32 * 4,
        LINEAR,
    );
    client
        .request(
            params,
            zwp_linux_buffer_params_v1::Request::Create {
                width: 32,
                height: 32,
                format: AR24,
                flags: 0,
            },
        )
        .unwrap();
    client.roundtrip().unwrap();
    let created = client.take_event(params, "created").expect("created");
    let [Arg::NewId(NewId(buffer))] = created.args.as_slice() else {
        panic!("{:?}", created.args);
    };
    assert!(*buffer >= 0xff00_0000, "the compositor's own ID");

    show(&mut client, window.surface, *buffer);

    assert_eq!(compositor.screen(0).pixel(48 + 5, 48 + 5), Some(0xff20_c020));
}

#[test]
fn a_tiled_buffer_fails_and_is_not_an_error() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment_with_gpu("c1");
    let params = params(&mut client);
    add(&mut client, params, 0, dmabuf(16, 16, 64, 0), 64, TILED);
    client
        .request(
            params,
            zwp_linux_buffer_params_v1::Request::Create {
                width: 16,
                height: 16,
                format: XR24,
                flags: 0,
            },
        )
        .unwrap();
    client.roundtrip().unwrap();

    assert!(client.take_event(params, "failed").is_some());
}

/// Expects the compositor to refuse what `send` does with `code`.
fn refused(code: u32, send: impl FnOnce(&mut Client, u32)) {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment_with_gpu("c1");
    let params = params(&mut client);
    send(&mut client, params);
    assert!(client.roundtrip().is_err(), "the compositor accepted it");
    let error = client.error.clone().expect("a protocol error");
    assert_eq!(error.code, code, "{error:?}");
    compositor.shell().roundtrip().unwrap();
}

#[test]
fn the_params_errors() {
    use zwp_linux_buffer_params_v1::error;
    refused(error::PLANE_IDX, |client, params| {
        add(client, params, 4, dmabuf(1, 1, 4, 0), 4, LINEAR);
    });
    refused(error::PLANE_SET, |client, params| {
        add(client, params, 0, dmabuf(1, 1, 4, 0), 4, LINEAR);
        add(client, params, 0, dmabuf(1, 1, 4, 0), 4, LINEAR);
    });
    refused(error::INCOMPLETE, |client, params| {
        add(client, params, 1, dmabuf(1, 1, 4, 0), 4, LINEAR);
        create_immed(client, params, (1, 1), XR24);
    });
    refused(error::INVALID_FORMAT, |client, params| {
        add(client, params, 0, dmabuf(1, 1, 4, 0), 4, LINEAR);
        create_immed(client, params, (1, 1), u32::from_le_bytes(*b"NV12"));
    });
    refused(error::INVALID_DIMENSIONS, |client, params| {
        add(client, params, 0, dmabuf(1, 1, 4, 0), 4, LINEAR);
        create_immed(client, params, (0, 1), XR24);
    });
    refused(error::OUT_OF_BOUNDS, |client, params| {
        add(client, params, 0, dmabuf(4, 4, 16, 0), 16, LINEAR);
        create_immed(client, params, (4, 8), XR24);
    });
    refused(error::ALREADY_USED, |client, params| {
        add(client, params, 0, dmabuf(1, 1, 4, 0), 4, LINEAR);
        create_immed(client, params, (1, 1), XR24);
        add(client, params, 1, dmabuf(1, 1, 4, 0), 4, LINEAR);
    });
    refused(error::INVALID_WL_BUFFER, |client, params| {
        add(client, params, 0, dmabuf(16, 16, 64, 0), 64, TILED);
        create_immed(client, params, (16, 16), XR24);
    });
}

/// Shortening a regular file used as a dma-buf produces a protocol error.
#[test]
fn a_dmabuf_that_shrinks_cannot_crash_the_compositor() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment_with_gpu("c1");
    let window = client
        .open_toplevel("dmabuf", "test.dmabuf", (64, 64), 0xff00_00ff)
        .unwrap();
    let params = params(&mut client);
    let fd = dmabuf(64, 64, 256, 0xffff_ffff);
    let kept = fd.try_clone().unwrap();
    add(&mut client, params, 0, fd, 256, LINEAR);
    let buffer = create_immed(&mut client, params, (64, 64), XR24);
    client.roundtrip().unwrap();

    rustix::fs::ftruncate(&kept, 0).unwrap();
    client
        .request(
            window.surface,
            wl_surface::Request::Attach {
                buffer: Some(ObjectId(buffer)),
                x: 0,
                y: 0,
            },
        )
        .unwrap();
    client.request(window.surface, wl_surface::Request::Commit).unwrap();

    assert!(client.roundtrip().is_err());
    assert_eq!(client.error.clone().unwrap().code, wl_shm::error::INVALID_FD);
    compositor.shell().roundtrip().unwrap();
}
