//! What one client can make the compositor hold (CrowbarOS issue 174).
//!
//! Every connection costs the compositor a descriptor, and so does every
//! descriptor a client sends and every one the compositor makes for a
//! client. Each client is stopped at its budget, a reserve stays for the
//! shell whatever an app sends or asks for, and a socket that cannot accept
//! at all, because the process has no descriptor left, waits and tries
//! again.
//!
//! The compositor is in this process, so these tests take turns: the
//! descriptor table, and its limit, are the whole process's.

mod common;

use std::{
    fs::File,
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::Path,
    sync::{Mutex, PoisonError, mpsc},
    time::{Duration, Instant},
};

use common::{Compositor, connect};
use crowbar_compositor::limits;
use crowbar_probe::{self as probe, Client};
use fulcrum_wire::{
    Arg, Encode, ObjectId,
    protocol::{
        linux_dmabuf_v1::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1, zwp_linux_dmabuf_v1},
        linux_drm_syncobj_v1::{wp_linux_drm_syncobj_manager_v1, wp_linux_drm_syncobj_timeline_v1},
        primary_selection_unstable_v1::{
            zwp_primary_selection_device_manager_v1, zwp_primary_selection_device_v1, zwp_primary_selection_offer_v1,
            zwp_primary_selection_source_v1,
        },
        wayland::{
            wl_buffer, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_display, wl_keyboard,
            wl_seat, wl_shm, wl_shm_pool, wl_surface,
        },
    },
};
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Every descriptor this process can have but one. Dropping it gives them
/// back.
struct AllDescriptorsButOne {
    held: Vec<File>,
    limit: Rlimit,
}

impl AllDescriptorsButOne {
    fn take() -> Self {
        // A low limit, so that this takes hundreds and not a million.
        let limit = getrlimit(Resource::Nofile);
        if limit.current.is_none_or(|current| current > 256) {
            let low = Rlimit {
                current: Some(256),
                maximum: limit.maximum,
            };
            setrlimit(Resource::Nofile, low).unwrap();
        }
        let mut held = vec![File::open("/dev/null").unwrap()];
        while let Ok(copy) = held[0].try_clone() {
            held.push(copy);
        }
        held.pop();
        AllDescriptorsButOne { held, limit }
    }
}

impl Drop for AllDescriptorsButOne {
    fn drop(&mut self) {
        self.held.clear();
        setrlimit(Resource::Nofile, self.limit).unwrap();
    }
}

/// Connects to a socket when the compositor has no descriptor to accept
/// with, and waits until it has failed to. Then the descriptors come back.
fn connect_when_the_compositor_cannot_accept(compositor: &Compositor, socket: &Path) -> UnixStream {
    let before = compositor.with(|state| state.failed_accepts());
    let all_but_one = AllDescriptorsButOne::take();
    let pending = UnixStream::connect(socket).expect("the one descriptor left is for this");
    let deadline = Instant::now() + Duration::from_secs(10);
    while compositor.with(|state| state.failed_accepts()) == before {
        assert!(Instant::now() < deadline, "the compositor never failed to accept");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The connection it could not take stays ready. A compositor that does
    // not wait tries again thousands of times in this long.
    std::thread::sleep(Duration::from_millis(200));
    let tries = compositor.with(|state| state.failed_accepts()) - before;
    drop(all_but_one);
    assert!(
        tries <= 3,
        "the compositor tried {tries} times to accept, and did not wait"
    );
    pending
}

/// Whether a client of this socket gets an answer in ten seconds.
fn answers(socket: &Path) -> bool {
    let (tx, rx) = mpsc::channel();
    let socket = socket.to_owned();
    std::thread::spawn(move || {
        let _ = tx.send(connect(&socket).is_ok());
    });
    rx.recv_timeout(Duration::from_secs(10)) == Ok(true)
}

#[test]
fn the_apps_socket_that_cannot_accept_waits_and_the_compositor_goes_on() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut other = compositor.app();

    let _pending = connect_when_the_compositor_cannot_accept(&compositor, &compositor.parts.apps.socket);

    other.roundtrip().expect("a connected app lost its connection");
    assert!(answers(&compositor.parts.shell.socket), "the shell's socket stopped");
    assert!(
        answers(&compositor.parts.apps.socket),
        "the apps' socket never accepted again after the descriptors came back"
    );
}

#[test]
fn the_shells_socket_that_cannot_accept_does_so_again_later() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();

    let _pending = connect_when_the_compositor_cannot_accept(&compositor, &compositor.parts.shell.socket);

    assert!(
        answers(&compositor.parts.shell.socket),
        "the shell's socket never accepted again after the descriptors came back"
    );
}

#[test]
fn a_client_is_stopped_at_its_budget_of_pools() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut client = compositor.app();
    for _ in 0..limits::CLIENT_POOLS {
        client.shm_buffer((1, 1), 0).unwrap();
    }
    client.roundtrip().unwrap();
    // The refusal can come back while the buffer's requests are still being
    // sent, as the compositor closes the connection, or at the roundtrip.
    let refused = client.shm_buffer((1, 1), 0).and_then(|_| client.roundtrip());
    assert!(refused.is_err(), "the compositor took a pool over the budget");
    let error = client.error.clone().unwrap();
    assert_eq!(error.code, wl_display::error::NO_MEMORY, "{error:?}");
    assert!(error.message.contains("pools"), "{error:?}");
    compositor.shell().roundtrip().unwrap();
}

#[test]
fn a_client_is_stopped_at_its_budget_of_shared_memory() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let shm = client.bind(wl_shm::NAME, 1).unwrap();
    let file = rustix::fs::memfd_create("big", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    let pool_size = crowbar_compositor::MAX_POOL_SIZE;
    rustix::fs::ftruncate(&file, pool_size as u64).unwrap();
    let pools = limits::CLIENT_POOL_BYTES / pool_size;
    for _ in 0..pools {
        let id = client.new_id("wl_shm_pool");
        client
            .request(
                shm,
                wl_shm::Request::CreatePool {
                    id,
                    fd: file.try_clone().unwrap(),
                    size: pool_size as i32,
                },
            )
            .unwrap();
    }
    client.roundtrip().unwrap();
    let id = client.new_id("wl_shm_pool");
    client
        .request(
            shm,
            wl_shm::Request::CreatePool {
                id,
                fd: file.try_clone().unwrap(),
                size: 4096,
            },
        )
        .unwrap();
    assert!(client.roundtrip().is_err());
    let error = client.error.clone().unwrap();
    assert_eq!(error.code, wl_display::error::NO_MEMORY, "{error:?}");
}

/// `foot` reserves a pool of 512 MiB for each window and leaves it sparse,
/// so that it scrolls by moving a buffer's offset. A pool costs address
/// space until it is written to, so a client may hold several.
#[test]
fn a_client_may_hold_sparse_pools_as_large_as_foots() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut client = compositor.app();
    let shm = client.bind(wl_shm::NAME, 1).unwrap();
    const FOOT_POOL: i32 = 512 * 1024 * 1024;
    let file = rustix::fs::memfd_create("foot-like", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    rustix::fs::ftruncate(&file, FOOT_POOL as u64).unwrap();
    rustix::io::pwrite(&file, &[0xff; 100 * 100 * 4], 0).unwrap();

    for _ in 0..3 {
        let pool = client.new_id(wl_shm_pool::NAME);
        client
            .request(
                shm,
                wl_shm::Request::CreatePool {
                    id: pool,
                    fd: file.try_clone().unwrap(),
                    size: FOOT_POOL,
                },
            )
            .unwrap();
        let buffer = client.new_id(wl_buffer::NAME);
        client
            .request(
                pool.0,
                wl_shm_pool::Request::CreateBuffer {
                    id: buffer,
                    offset: 0,
                    width: 100,
                    height: 100,
                    stride: 400,
                    format: wl_shm::format::ARGB8888,
                },
            )
            .unwrap();
        let surface = client.create_surface().unwrap();
        client
            .request(
                surface,
                wl_surface::Request::Attach {
                    buffer: Some(ObjectId(buffer.0)),
                    x: 0,
                    y: 0,
                },
            )
            .unwrap();
        client.request(surface, wl_surface::Request::Commit).unwrap();
    }

    client.roundtrip().unwrap();
    let shown = compositor.with(|state| state.render_root_surfaces().len());
    assert_eq!(shown, 3);
}

/// The compositor copies each buffer a client commits. One buffer can be
/// committed on many surfaces, so the copies have a budget of their own.
#[test]
fn a_client_is_stopped_at_its_budget_of_pixels() {
    // Others lower the process's descriptor limit, which would refuse this
    // test's connections.
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    // Four buffers of 256x256.
    compositor.with(|state| state.set_pixel_budget(4 * 256 * 256 * 4));
    let mut client = compositor.app();
    let buffer = client.shm_buffer((256, 256), 0xffff_ffff).unwrap();
    let surfaces: Vec<u32> = (0..6).map(|_| client.create_surface().unwrap()).collect();

    for surface in &surfaces[..4] {
        client.show(*surface, Some(&buffer)).unwrap();
    }
    // A surface's new pixels replace its old ones.
    for _ in 0..3 {
        client.show(surfaces[0], Some(&buffer)).unwrap();
    }
    client.roundtrip().unwrap();
    // A destroyed surface's pixels are let go.
    client.request(surfaces[3], wl_surface::Request::Destroy).unwrap();
    client.show(surfaces[4], Some(&buffer)).unwrap();
    client.roundtrip().unwrap();

    client.show(surfaces[5], Some(&buffer)).unwrap();
    assert!(client.roundtrip().is_err(), "the compositor held more than its budget");
    let error = client.error.clone().unwrap();
    assert_eq!(error.code, wl_display::error::NO_MEMORY, "{error:?}");
    assert!(error.message.contains("pixels"), "{error:?}");
    compositor.shell().roundtrip().unwrap();
}

/// When few descriptors are left, an app is refused before the shell is:
/// the reserve is the shell's. An app that opens connections without end is
/// stopped there, and the shell is still accepted.
#[test]
fn the_reserve_is_kept_for_the_shell() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    // A limit that leaves room for two more connections above the reserve.
    compositor.with(|state| {
        let limit = state.descriptors_held() + limits::RESERVE + 2;
        state.set_descriptor_limit(limit);
    });
    let mut held = Vec::new();
    while let Ok(client) = compositor.try_app() {
        held.push(client);
        assert!(held.len() <= 2, "an app took descriptors from the reserve");
    }
    assert_eq!(held.len(), 2);

    // A connection that ends gives its place back.
    held.remove(0);
    let back = (0..200).any(|_| {
        std::thread::sleep(Duration::from_millis(5));
        match compositor.try_app() {
            Ok(client) => {
                held.push(client);
                true
            }
            Err(_) => false,
        }
    });
    assert!(back, "a closed connection's place never came back");

    for _ in 0..4 {
        held.push(compositor.shell());
    }
    held.last_mut().unwrap().roundtrip().unwrap();
}

/// Leaves the apps room for `more` descriptors before the shell's reserve.
fn leave_apps_room_for(compositor: &Compositor, more: usize) {
    compositor.with(move |state| {
        let limit = state.descriptors_held() + limits::RESERVE + more;
        state.set_descriptor_limit(limit);
    });
}

fn held(compositor: &Compositor) -> usize {
    compositor.with(|state| state.descriptors_held())
}

/// A descriptor to send: it is never read.
fn descriptor() -> OwnedFd {
    File::open("/dev/null").unwrap().into()
}

/// Sends requests in one write, so that the compositor reads them, and the
/// descriptors beside them, together: it sends nothing in between.
fn send_together<R: Encode>(client: &mut Client, requests: Vec<(u32, R)>) -> Result<(), probe::Error> {
    let (mut bytes, mut fds) = (Vec::new(), Vec::new());
    for (object, request) in requests {
        let (message, descriptors) = request.to_message(object)?;
        bytes.extend(message);
        fds.extend(descriptors);
    }
    client.send_raw(bytes, fds)
}

/// The compositor refused what an app sent, to keep the reserve. The
/// refusal can come back while the requests are still being sent, as the
/// compositor closes the connection, or at the roundtrip.
fn assert_refused_for_the_reserve(client: &mut Client, sent: Result<(), probe::Error>) {
    let refused = sent.and_then(|()| client.roundtrip());
    assert!(refused.is_err(), "the compositor took a descriptor out of the reserve");
    let error = client.error.clone().expect("the refusal is a protocol error");
    assert_eq!(error.code, wl_display::error::NO_MEMORY, "{error:?}");
    assert!(error.message.contains("descriptors are left"), "{error:?}");
}

/// A `wl_display.sync` with descriptors beside it that no request takes.
fn sync_with_descriptors(client: &mut Client, descriptors: usize) -> Result<(), probe::Error> {
    let callback = client.new_id("wl_callback");
    let (bytes, _) = wl_display::Request::Sync { callback }.to_message(1)?;
    client.send_raw(bytes, (0..descriptors).map(|_| descriptor()).collect())
}

/// A descriptor is in the compositor from the moment it arrives, whether a
/// request takes it or not. An app's that come out of the reserve are
/// closed, and the app refused; the shell's are taken.
#[test]
fn an_app_cannot_send_descriptors_out_of_the_reserve() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    let mut app = compositor.app();
    let before = held(&compositor);
    leave_apps_room_for(&compositor, 2);

    sync_with_descriptors(&mut app, 2).unwrap();
    app.roundtrip().unwrap();
    assert_eq!(held(&compositor), before + 2, "descriptors that wait are not counted");

    let sent = sync_with_descriptors(&mut app, 1);
    assert_refused_for_the_reserve(&mut app, sent);
    let deadline = Instant::now() + Duration::from_secs(10);
    while compositor.client_count() > 1 {
        assert!(Instant::now() < deadline, "the refused app is still connected");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        held(&compositor),
        before - 1,
        "the refused app's descriptors stayed open"
    );

    sync_with_descriptors(&mut shell, 8).unwrap();
    shell.roundtrip().unwrap();
    compositor.shell().roundtrip().unwrap();
}

fn dmabuf_params(client: &mut Client) -> u32 {
    let dmabuf = client.bind(zwp_linux_dmabuf_v1::NAME, 4).unwrap();
    let params = client.new_id(zwp_linux_buffer_params_v1::NAME);
    client
        .request(dmabuf, zwp_linux_dmabuf_v1::Request::CreateParams { params_id: params })
        .unwrap();
    client.roundtrip().unwrap();
    params.0
}

fn plane(plane_idx: u32) -> zwp_linux_buffer_params_v1::Request {
    zwp_linux_buffer_params_v1::Request::Add {
        fd: descriptor(),
        plane_idx,
        offset: 0,
        stride: 4,
        modifier_hi: 0,
        modifier_lo: 0,
    }
}

fn add_plane(client: &mut Client, params: u32, plane_idx: u32) -> Result<(), probe::Error> {
    client.request(params, plane(plane_idx))
}

/// A plane added to a dma-buf's params is a descriptor the compositor holds
/// until the buffer is made or the params go.
#[test]
fn an_app_cannot_add_a_dmabuf_plane_out_of_the_reserve() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    let mut app = compositor.app();
    let shell_params = dmabuf_params(&mut shell);
    let params = dmabuf_params(&mut app);
    let before = held(&compositor);
    leave_apps_room_for(&compositor, 2);

    for plane in 0..2 {
        add_plane(&mut app, params, plane).unwrap();
        app.roundtrip().unwrap();
    }
    assert_eq!(held(&compositor), before + 2);

    let sent = add_plane(&mut app, params, 2);
    assert_refused_for_the_reserve(&mut app, sent);

    for plane in 0..4 {
        add_plane(&mut shell, shell_params, plane).unwrap();
    }
    shell.roundtrip().unwrap();
}

/// An app at the reserve that lets go of a descriptor and sends another in
/// its place has taken nothing, whether the compositor reads the two
/// requests one at a time or together, and whether or not the shell is
/// using its reserve.
#[test]
fn an_app_at_the_reserve_may_send_a_descriptor_in_place_of_one_it_lets_go() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut app = compositor.app();
    let params: Vec<u32> = (0..3).map(|_| dmabuf_params(&mut app)).collect();
    let before = held(&compositor);
    leave_apps_room_for(&compositor, 1);
    add_plane(&mut app, params[0], 0).unwrap();
    app.roundtrip().unwrap();
    assert_eq!(held(&compositor), before + 1);

    let swap = |app: &mut Client, old: u32, new: u32| {
        send_together(
            app,
            vec![(old, zwp_linux_buffer_params_v1::Request::Destroy), (new, plane(0))],
        )
        .unwrap();
        app.roundtrip().unwrap();
    };
    swap(&mut app, params[0], params[1]);
    assert_eq!(held(&compositor), before + 1);

    // The shell takes two descriptors from its reserve.
    compositor.with(|state| {
        let limit = state.descriptors_held() + limits::RESERVE - 2;
        state.set_descriptor_limit(limit);
    });
    swap(&mut app, params[1], params[2]);
    assert_eq!(held(&compositor), before + 1);

    // One more is still one too many.
    let sent = add_plane(&mut app, params[2], 1);
    assert_refused_for_the_reserve(&mut app, sent);
}

/// The same for a shared-memory pool, whose file is a descriptor the app
/// sent like any other: a pool in the place of one the app destroys takes
/// nothing, while one more is refused.
#[test]
fn an_app_at_the_reserve_may_make_a_pool_in_place_of_one_it_destroys() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut app = compositor.app();
    let shm = app.bind(wl_shm::NAME, 1).unwrap();
    let pool = |app: &mut Client| -> (u32, Vec<u8>, Vec<OwnedFd>) {
        let file = rustix::fs::memfd_create("a-pool", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        rustix::fs::ftruncate(&file, 4096).unwrap();
        let id = app.new_id(wl_shm_pool::NAME);
        let (bytes, fds) = wl_shm::Request::CreatePool {
            id,
            fd: file,
            size: 4096,
        }
        .to_message(shm)
        .unwrap();
        (id.0, bytes, fds)
    };
    let before = held(&compositor);
    leave_apps_room_for(&compositor, 1);
    let (mut held_pool, bytes, fds) = pool(&mut app);
    app.send_raw(bytes, fds).unwrap();
    app.roundtrip().unwrap();
    assert_eq!(held(&compositor), before + 1);

    // At the reserve, and then with the shell two into it.
    for shell_took in [0, 2] {
        compositor.with(move |state| {
            let limit = state.descriptors_held() + limits::RESERVE - shell_took;
            state.set_descriptor_limit(limit);
        });
        let (destroy, _) = wl_shm_pool::Request::Destroy.to_message(held_pool).unwrap();
        let (next, create, fds) = pool(&mut app);
        app.send_raw([destroy, create].concat(), fds).unwrap();
        app.roundtrip().unwrap();
        assert_eq!(held(&compositor), before + 1);
        held_pool = next;
    }

    let (_, bytes, fds) = pool(&mut app);
    let sent = app.send_raw(bytes, fds);
    assert_refused_for_the_reserve(&mut app, sent);
}

/// The offer an app's device was last given of a selection.
fn offered(client: &mut Client, device: u32) -> u32 {
    client.roundtrip().unwrap();
    client
        .events
        .iter()
        .rev()
        .find(|event| event.object == device && event.name == "selection")
        .and_then(|event| match event.args.as_slice() {
            [Arg::Object(Some(offer))] => Some(offer.0),
            _ => None,
        })
        .expect("the client with the keyboard is offered the selection")
}

/// An app with a window, which has the keyboard, and an offer of the
/// selection it copied itself.
fn app_offered_its_own_selection(compositor: &Compositor) -> (Client, u32) {
    let mut app = compositor.app();
    app.open_toplevel("copier", "test.copier", (64, 48), 0xffff_0000)
        .unwrap();
    let manager = app.bind(wl_data_device_manager::NAME, 3).unwrap();
    let seat = app.bind(wl_seat::NAME, 7).unwrap();
    let device = app.new_id(wl_data_device::NAME);
    app.request(
        manager,
        wl_data_device_manager::Request::GetDataDevice {
            id: device,
            seat: ObjectId(seat),
        },
    )
    .unwrap();
    let source = app.new_id(wl_data_source::NAME);
    app.request(
        manager,
        wl_data_device_manager::Request::CreateDataSource { id: source },
    )
    .unwrap();
    app.request(
        source.0,
        wl_data_source::Request::Offer {
            mime_type: "text/plain".into(),
        },
    )
    .unwrap();
    app.request(
        device.0,
        wl_data_device::Request::SetSelection {
            source: Some(ObjectId(source.0)),
            serial: 0,
        },
    )
    .unwrap();
    let offer = offered(&mut app, device.0);
    (app, offer)
}

/// A paste hands the compositor a descriptor, which waits in the source's
/// connection until the source reads it.
#[test]
fn an_app_cannot_paste_with_descriptors_out_of_the_reserve() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let (mut app, offer) = app_offered_its_own_selection(&compositor);
    let receive = |count: usize| -> Vec<(u32, wl_data_offer::Request)> {
        (0..count)
            .map(|_| {
                let request = wl_data_offer::Request::Receive {
                    mime_type: "text/plain".into(),
                    fd: descriptor(),
                };
                (offer, request)
            })
            .collect()
    };
    let before = held(&compositor);
    leave_apps_room_for(&compositor, 2);

    send_together(&mut app, receive(2)).unwrap();
    app.roundtrip().unwrap();
    assert_eq!(held(&compositor), before, "a descriptor passed on is still held");

    let sent = send_together(&mut app, receive(3));
    assert_refused_for_the_reserve(&mut app, sent);
    compositor.shell().roundtrip().unwrap();
}

/// A paste's descriptor goes on to the source's client, which may be
/// another app that never reads it. Refusing the app that pasted would not
/// take it back, so it is not passed on out of the reserve.
#[test]
fn an_app_cannot_pass_descriptors_to_another_app_out_of_the_reserve() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let (mut copier, _) = app_offered_its_own_selection(&compositor);
    // A new window takes the keyboard, and is offered the selection.
    let mut paster = compositor.app();
    paster
        .open_toplevel("paster", "test.paster", (64, 48), 0xff00_ff00)
        .unwrap();
    let manager = paster.bind(wl_data_device_manager::NAME, 3).unwrap();
    let seat = paster.bind(wl_seat::NAME, 7).unwrap();
    let device = paster.new_id(wl_data_device::NAME);
    paster
        .request(
            manager,
            wl_data_device_manager::Request::GetDataDevice {
                id: device,
                seat: ObjectId(seat),
            },
        )
        .unwrap();
    let offer = offered(&mut paster, device.0);
    let receive = |count: usize| -> Vec<(u32, wl_data_offer::Request)> {
        (0..count)
            .map(|_| {
                let request = wl_data_offer::Request::Receive {
                    mime_type: "text/plain".into(),
                    fd: descriptor(),
                };
                (offer, request)
            })
            .collect()
    };
    let asked_to_send = |copier: &mut Client| {
        copier.roundtrip().unwrap();
        copier.events.iter().filter(|event| event.name == "send").count()
    };
    leave_apps_room_for(&compositor, 2);

    send_together(&mut paster, receive(2)).unwrap();
    paster.roundtrip().unwrap();
    assert_eq!(asked_to_send(&mut copier), 2);

    let sent = send_together(&mut paster, receive(3));
    assert_refused_for_the_reserve(&mut paster, sent);
    assert_eq!(
        asked_to_send(&mut copier),
        2,
        "a descriptor out of the reserve was passed on to another app"
    );
    compositor.shell().roundtrip().unwrap();
}

/// The primary selection's paste, under the same rule, from another app.
#[test]
fn an_app_cannot_paste_the_primary_selection_with_descriptors_out_of_the_reserve() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut app = compositor.app();
    app.open_toplevel("selector", "test.selector", (64, 48), 0xffff_0000)
        .unwrap();
    let manager = app.bind(zwp_primary_selection_device_manager_v1::NAME, 1).unwrap();
    let seat = app.bind(wl_seat::NAME, 7).unwrap();
    let device = app.new_id(zwp_primary_selection_device_v1::NAME);
    app.request(
        manager,
        zwp_primary_selection_device_manager_v1::Request::GetDevice {
            id: device,
            seat: ObjectId(seat),
        },
    )
    .unwrap();
    let source = app.new_id(zwp_primary_selection_source_v1::NAME);
    app.request(
        manager,
        zwp_primary_selection_device_manager_v1::Request::CreateSource { id: source },
    )
    .unwrap();
    app.request(
        source.0,
        zwp_primary_selection_source_v1::Request::Offer {
            mime_type: "text/plain".into(),
        },
    )
    .unwrap();
    app.request(
        device.0,
        zwp_primary_selection_device_v1::Request::SetSelection {
            source: Some(ObjectId(source.0)),
            serial: 0,
        },
    )
    .unwrap();
    app.roundtrip().unwrap();
    // Another app's new window takes the keyboard, and is offered the
    // primary selection: its pastes go on to the first app.
    let mut paster = compositor.app();
    paster
        .open_toplevel("paster", "test.paster", (64, 48), 0xff00_ff00)
        .unwrap();
    let manager = paster.bind(zwp_primary_selection_device_manager_v1::NAME, 1).unwrap();
    let seat = paster.bind(wl_seat::NAME, 7).unwrap();
    let device = paster.new_id(zwp_primary_selection_device_v1::NAME);
    paster
        .request(
            manager,
            zwp_primary_selection_device_manager_v1::Request::GetDevice {
                id: device,
                seat: ObjectId(seat),
            },
        )
        .unwrap();
    let offer = offered(&mut paster, device.0);
    let asked_to_send = |app: &mut Client| {
        app.roundtrip().unwrap();
        app.events.iter().filter(|event| event.name == "send").count()
    };
    let receive = |count: usize| -> Vec<(u32, zwp_primary_selection_offer_v1::Request)> {
        (0..count)
            .map(|_| {
                let request = zwp_primary_selection_offer_v1::Request::Receive {
                    mime_type: "text/plain".into(),
                    fd: descriptor(),
                };
                (offer, request)
            })
            .collect()
    };
    leave_apps_room_for(&compositor, 2);

    send_together(&mut paster, receive(2)).unwrap();
    paster.roundtrip().unwrap();
    assert_eq!(asked_to_send(&mut app), 2);

    let sent = send_together(&mut paster, receive(3));
    assert_refused_for_the_reserve(&mut paster, sent);
    assert_eq!(
        asked_to_send(&mut app),
        2,
        "a descriptor out of the reserve was passed on to another app"
    );
    compositor.shell().roundtrip().unwrap();
}

fn keyboards(client: &mut Client, seat: u32, count: usize) -> Vec<(u32, wl_seat::Request)> {
    (0..count)
        .map(|_| {
            let id = client.new_id(wl_keyboard::NAME);
            (seat, wl_seat::Request::GetKeyboard { id })
        })
        .collect()
}

/// Each keyboard is sent the keymap as a descriptor of its own, which waits
/// in the connection until the client reads it. An app that asks for
/// keyboards and reads nothing would otherwise make the compositor hold one
/// for each.
#[test]
fn an_app_cannot_be_sent_keymaps_out_of_the_reserve() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    let mut app = compositor.app();
    let shell_seat = shell.bind(wl_seat::NAME, 7).unwrap();
    let seat = app.bind(wl_seat::NAME, 7).unwrap();
    shell.roundtrip().unwrap();
    app.roundtrip().unwrap();
    let before = held(&compositor);
    leave_apps_room_for(&compositor, 2);

    let two = keyboards(&mut app, seat, 2);
    send_together(&mut app, two).unwrap();
    app.roundtrip().unwrap();
    assert_eq!(held(&compositor), before, "a keymap that was sent is still held");

    let three = keyboards(&mut app, seat, 3);
    let sent = send_together(&mut app, three);
    assert_refused_for_the_reserve(&mut app, sent);

    let eight = keyboards(&mut shell, shell_seat, 8);
    send_together(&mut shell, eight).unwrap();
    shell.roundtrip().unwrap();
}

fn feedbacks(client: &mut Client, dmabuf: u32, count: usize) -> Vec<(u32, zwp_linux_dmabuf_v1::Request)> {
    (0..count)
        .map(|_| {
            let id = client.new_id(zwp_linux_dmabuf_feedback_v1::NAME);
            (dmabuf, zwp_linux_dmabuf_v1::Request::GetDefaultFeedback { id })
        })
        .collect()
}

/// linux-dmabuf's feedback is sent the format table as a descriptor of its
/// own, as a keyboard is sent the keymap.
#[test]
fn an_app_cannot_be_sent_format_tables_out_of_the_reserve() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut shell = compositor.shell();
    let mut app = compositor.app();
    let shell_dmabuf = shell.bind(zwp_linux_dmabuf_v1::NAME, 4).unwrap();
    let dmabuf = app.bind(zwp_linux_dmabuf_v1::NAME, 4).unwrap();
    shell.roundtrip().unwrap();
    app.roundtrip().unwrap();
    leave_apps_room_for(&compositor, 2);

    let two = feedbacks(&mut app, dmabuf, 2);
    send_together(&mut app, two).unwrap();
    app.roundtrip().unwrap();

    let three = feedbacks(&mut app, dmabuf, 3);
    let sent = send_together(&mut app, three);
    assert_refused_for_the_reserve(&mut app, sent);

    let eight = feedbacks(&mut shell, shell_dmabuf, 8);
    send_together(&mut shell, eight).unwrap();
    shell.roundtrip().unwrap();
}

/// How many descriptors this process has open.
fn open_descriptors() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

/// Every timeline shares the compositor's one descriptor of the DRM device,
/// so a client that imports timelines takes no descriptors. This needs a
/// render node with timeline syncobjs, which CI's machines have not: where
/// there is none, it says so and passes.
#[test]
fn a_timeline_takes_no_descriptor() {
    use fulcrum_sys::syncobj;
    use std::os::fd::AsFd;

    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let node = std::fs::read_dir("/dev/dri").ok().and_then(|entries| {
        entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("renderD"))
            })
            .filter_map(|path| std::fs::OpenOptions::new().read(true).write(true).open(path).ok())
            .map(OwnedFd::from)
            .find(|node| syncobj::supported(node.as_fd()))
    });
    let Some(node) = node else {
        eprintln!("no render node with timeline syncobjs: skipped");
        return;
    };
    let offered = node.try_clone().unwrap();
    let compositor = Compositor::start_with(common::own_uid(), common::own_uid(), move |state| {
        state.add_output("TEST-1", common::OUTPUT_SIZE, 60_000);
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
        state.offer_explicit_sync(offered);
    });
    let mut app = compositor.app();
    let manager = app.bind(wp_linux_drm_syncobj_manager_v1::NAME, 1).unwrap();
    app.roundtrip().unwrap();
    let before = open_descriptors();

    for _ in 0..16 {
        let id = app.new_id(wp_linux_drm_syncobj_timeline_v1::NAME);
        let fd = syncobj::create_exported(node.as_fd()).unwrap();
        app.request(
            manager,
            wp_linux_drm_syncobj_manager_v1::Request::ImportTimeline { id, fd },
        )
        .unwrap();
    }
    app.roundtrip().unwrap();

    assert_eq!(compositor.with(|state| state.timelines_held()), 16);
    assert_eq!(open_descriptors(), before, "each timeline took a descriptor");
}
