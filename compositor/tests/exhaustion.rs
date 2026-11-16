//! A listener that cannot accept does not end the compositor.
//!
//! Every connection costs the compositor a descriptor, so a compartment that
//! opens enough of them leaves it none to accept with. The compositor is in
//! this process, so the tests fill this process's descriptor table. They take
//! turns, because the table is the whole process's.

mod common;

use std::{
    fs::File,
    os::unix::net::UnixStream,
    path::Path,
    sync::{Mutex, Once, PoisonError, mpsc},
    time::{Duration, Instant},
};

use common::{Compositor, connect};
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// What the compositor logged, in every test of this file.
static LOG: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct ToLog;

impl std::io::Write for ToLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        LOG.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// How many times the compositor has logged that it could not accept.
fn failed_accepts() -> usize {
    static LOGGING: Once = Once::new();
    LOGGING.call_once(|| tracing_subscriber::fmt().with_writer(|| ToLog).init());
    let log = LOG.lock().unwrap();
    String::from_utf8_lossy(&log)
        .matches("could not accept a connection")
        .count()
}

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
        // The kernel also holds a low limit against descriptors that are
        // being passed over a socket, which a Wayland client does.
        setrlimit(Resource::Nofile, self.limit).unwrap();
    }
}

/// Connects to a socket when the compositor has no descriptor to accept
/// with, and waits until the compositor has logged that it could not. Then
/// the descriptors come back.
fn connect_when_the_compositor_cannot_accept(socket: &Path) -> UnixStream {
    let before = failed_accepts();
    let all_but_one = AllDescriptorsButOne::take();
    let pending = UnixStream::connect(socket).expect("the one descriptor left is for this");
    let deadline = Instant::now() + Duration::from_secs(10);
    while failed_accepts() == before {
        assert!(
            Instant::now() < deadline,
            "the compositor never said that it could not accept"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The connection it could not take stays ready. A compositor that does
    // not wait tries again thousands of times in this long.
    std::thread::sleep(Duration::from_millis(200));
    let tries = failed_accepts() - before;
    drop(all_but_one);
    assert!(
        tries <= 3,
        "the compositor tried {tries} times to accept, and did not wait"
    );
    pending
}

/// Whether a Wayland client of this socket gets an answer in ten seconds.
fn answers(socket: &Path) -> bool {
    let (tx, rx) = mpsc::channel();
    let socket = socket.to_owned();
    std::thread::spawn(move || {
        let _ = tx.send(connect(&socket).is_ok());
    });
    rx.recv_timeout(Duration::from_secs(10)) == Ok(true)
}

#[test]
fn a_compartment_that_uses_up_the_descriptors_does_not_end_the_compositor() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let compositor = Compositor::start();
    // One connection for the Warden, kept for the whole test. One that is
    // dropped gives the compositor a descriptor back, at a time of its own.
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

    let _pending = connect_when_the_compositor_cannot_accept(&greedy.path);

    compositor.with(|_| ());
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

    let _pending = connect_when_the_compositor_cannot_accept(&compositor.parts.shell.socket);

    compositor.with(|_| ());
    assert!(
        answers(&compositor.parts.shell.socket),
        "the shell's socket never accepted again after the descriptors came back"
    );
}
