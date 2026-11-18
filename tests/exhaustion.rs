//! What one compartment can make the compositor hold (CrowbarOS issue 174).
//!
//! Every connection and every pool costs the compositor a descriptor. A
//! compartment is stopped at its budget, a reserve stays for the shell and
//! the Warden, and a listener that cannot accept at all, because the process
//! has no descriptor left, waits and tries again.
//!
//! The compositor is in this process, so these tests take turns: the
//! descriptor table, and its limit, are the whole process's.

mod common;

use std::{
    fs::File,
    os::unix::net::UnixStream,
    path::Path,
    sync::{Mutex, PoisonError, mpsc},
    time::{Duration, Instant},
};

use common::{Compositor, connect};
use crowbar_compositor::limits;
use fulcrum_wire::{
    ObjectId,
    protocol::wayland::{wl_buffer, wl_display, wl_shm, wl_shm_pool, wl_surface},
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
fn a_listener_that_cannot_accept_waits_and_the_compositor_goes_on() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let mut warden = compositor.warden();
    let mut register = |compartment: &str| {
        warden
            .register_compartment(
                &compositor.listener_path(compartment),
                Some("test.principal"),
                Some(compartment),
            )
            .unwrap()
    };
    let other_listener = register("other");
    let greedy = register("greedy");
    let mut other = connect(&other_listener.path).unwrap();

    let _pending = connect_when_the_compositor_cannot_accept(&compositor, &greedy.path);

    other
        .roundtrip()
        .expect("a client of another compartment lost its connection");
    assert!(answers(&other_listener.path), "another compartment's listener stopped");
    assert!(
        answers(&greedy.path),
        "the listener never accepted again after the descriptors came back"
    );
}

#[test]
fn a_trusted_socket_that_cannot_accept_does_so_again_later() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();

    let _pending = connect_when_the_compositor_cannot_accept(&compositor, &compositor.parts.shell.socket);

    assert!(
        answers(&compositor.parts.shell.socket),
        "the shell's socket never accepted again after the descriptors came back"
    );
}

/// Issue 174: a compartment that opens connections without end is stopped
/// at its budget, and everyone else is still accepted.
#[test]
fn a_compartment_that_opens_connections_without_end_is_stopped_at_its_budget() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let greedy = compositor.register("test.principal", "greedy");
    let other = compositor.register("test.principal", "other");

    let mut held = Vec::new();
    let mut refused = 0;
    for _ in 0..limits::COMPARTMENT_CONNECTIONS + 4 {
        match connect(&greedy.path) {
            Ok(client) => held.push(client),
            Err(_) => refused += 1,
        }
    }
    assert_eq!(held.len(), limits::COMPARTMENT_CONNECTIONS);
    assert_eq!(refused, 4);

    // While it is at its budget, the compositor still takes the shell, the
    // Warden, and another compartment, and a connected client still works.
    assert!(compositor.try_shell().is_ok());
    assert!(compositor.try_warden().is_ok());
    let mut stranger = connect(&other.path).unwrap();
    let surface = stranger.create_surface().unwrap();
    let buffer = stranger.shm_buffer((2, 2), 0xffff_ffff).unwrap();
    stranger.show(surface, Some(&buffer)).unwrap();
    stranger.roundtrip().unwrap();

    // A connection that ends gives its place back.
    held.pop();
    let back = (0..200).any(|_| {
        std::thread::sleep(Duration::from_millis(5));
        connect(&greedy.path).is_ok()
    });
    assert!(back, "a closed connection's place never came back");
}

#[test]
fn a_client_is_stopped_at_its_budget_of_pools() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("pools");
    for _ in 0..limits::CLIENT_POOLS {
        client.shm_buffer((1, 1), 0).unwrap();
    }
    client.roundtrip().unwrap();
    client.shm_buffer((1, 1), 0).unwrap();
    assert!(client.roundtrip().is_err());
    let error = client.error.clone().unwrap();
    assert_eq!(error.code, wl_display::error::NO_MEMORY, "{error:?}");
    assert!(error.message.contains("pools"), "{error:?}");
    compositor.shell().roundtrip().unwrap();
}

#[test]
fn a_client_is_stopped_at_its_budget_of_shared_memory() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("bytes");
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
    let (mut client, _listener) = compositor.compartment("foot");
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
    let compositor = Compositor::start();
    // Four buffers of 256x256.
    compositor.with(|state| state.set_pixel_budget(4 * 256 * 256 * 4));
    let (mut client, _listener) = compositor.compartment("pixels");
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

/// When few descriptors are left, a compartment is refused before the
/// trusted parts are: the reserve is theirs.
#[test]
fn the_reserve_is_kept_for_the_shell_and_the_warden() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    let listener = compositor.register("test.principal", "squeezed");
    // The Warden's connection that registered the listener is closing. Wait
    // until it has, or its descriptor comes back after the limit is set.
    let settled = (0..400).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(5));
        compositor.client_count() == 0
    });
    assert!(settled, "the Warden's connection never closed");
    // A limit that leaves room for two more connections above the reserve.
    compositor.with(|state| {
        let limit = state.descriptors_held() + limits::RESERVE + 2;
        state.set_descriptor_limit(limit);
    });
    let mut held = Vec::new();
    while let Ok(client) = connect(&listener.path) {
        held.push(client);
        assert!(held.len() <= 2, "a compartment took descriptors from the reserve");
    }
    assert_eq!(held.len(), 2);
    for _ in 0..4 {
        held.push(compositor.shell());
        held.push(compositor.warden());
    }
}
