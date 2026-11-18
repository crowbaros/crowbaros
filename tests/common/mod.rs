//! Runs the compositor inside the test process, on a thread of its own, and
//! hands out real client connections to it.
//!
//! Clients connect the two ways the compositor allows (ADR 0016): to a
//! trusted part's path socket, or through a listener that a stand-in for the
//! Warden registers. There is no other way in to give a test.
#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex, Once,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

use crowbar_compositor::{
    State,
    connections::{TrustedPart, TrustedParts},
    event_loop::Remote,
};
use crowbar_probe::{self as probe, Client, CompartmentListener};

/// The test output.
pub const OUTPUT_SIZE: (i32, i32) = (1280, 800);
/// One frame on the test output, which runs at 60 Hz.
pub const FRAME: std::time::Duration = std::time::Duration::from_millis(17);

pub struct Compositor {
    /// A directory of this compositor's own. It holds the two trusted
    /// sockets and the listeners the tests register.
    pub dir: PathBuf,
    pub parts: TrustedParts,
    remote: Remote<State>,
    thread: Option<JoinHandle<()>>,
}

/// This process's user ID. In a test it plays the shell and the Warden.
pub fn own_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// A user ID that no process in the tests has.
pub fn another_uid() -> u32 {
    own_uid().wrapping_add(1)
}

/// The directory `XDG_RUNTIME_DIR` names for this process: where a compositor
/// with a public socket would put it. The tests leave it empty.
pub fn runtime_dir() -> PathBuf {
    static SET: Once = Once::new();
    let dir = base_dir().join("runtime");
    SET.call_once(|| {
        // Safety: set once, before any compositor thread starts.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
    });
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn base_dir() -> PathBuf {
    std::env::temp_dir().join(format!("fulcrum-test-{}", std::process::id()))
}

/// How many compositors of this process are running. The last one to stop
/// removes the process's directory.
static RUNNING: Mutex<u32> = Mutex::new(0);

impl Compositor {
    /// A compositor that takes this process for both the shell and the Warden.
    pub fn start() -> Self {
        Self::start_trusting(own_uid(), own_uid())
    }

    /// A compositor that takes these user IDs for the shell and the Warden.
    pub fn start_trusting(shell_uid: u32, warden_uid: u32) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = base_dir().join(format!("compositor-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        {
            let mut running = RUNNING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *running += 1;
            runtime_dir();
            std::fs::create_dir_all(&dir).unwrap();
        }
        let parts = TrustedParts {
            shell: TrustedPart {
                socket: dir.join("shell"),
                uid: shell_uid,
            },
            warden: TrustedPart {
                socket: dir.join("warden"),
                uid: warden_uid,
            },
        };
        let (ready_tx, ready_rx) = mpsc::channel();
        let listen_on = parts.clone();
        let thread = std::thread::spawn(move || {
            // The tests provoke refusals on purpose; their warnings are noise.
            crowbar_compositor::log::set_level(crowbar_compositor::log::Level::Error);
            let mut state = State::new().unwrap();
            state.add_output("TEST-1", OUTPUT_SIZE, 60_000);
            state.set_backend(crowbar_compositor::backend::headless::Headless::default());
            state.offer_dmabuf();
            state.listen(&listen_on).unwrap();
            ready_tx.send(state.remote()).unwrap();
            state.run().unwrap();
        });
        let remote = ready_rx.recv().unwrap();
        Compositor {
            dir,
            parts,
            remote,
            thread: Some(thread),
        }
    }

    /// Runs `f` on the compositor thread and returns its result.
    pub fn with<T: Send + 'static>(&self, f: impl FnOnce(&mut State) -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        self.remote.send(Box::new(move |state| {
            let _ = tx.send(f(state));
        }));
        rx.recv().unwrap()
    }

    /// Connects to the shell's socket.
    pub fn try_shell(&self) -> Result<Client, probe::Error> {
        Client::connect(&self.parts.shell.socket)
    }

    /// The shell: a client of the shell's socket.
    pub fn shell(&self) -> Client {
        self.try_shell().expect("the compositor refused the shell")
    }

    /// Connects to the Warden's socket.
    pub fn try_warden(&self) -> Result<Client, probe::Error> {
        Client::connect(&self.parts.warden.socket)
    }

    /// A stand-in for the Warden: a client of the Warden's socket.
    pub fn warden(&self) -> Client {
        self.try_warden().expect("the compositor refused the Warden")
    }

    /// Where a compartment's listener goes.
    pub fn listener_path(&self, compartment: &str) -> PathBuf {
        self.dir.join(format!("compartment-{compartment}"))
    }

    /// The stand-in for the Warden registers a listener for one compartment.
    pub fn register(&self, principal: &str, compartment: &str) -> CompartmentListener {
        self.warden()
            .register_compartment(&self.listener_path(compartment), Some(principal), Some(compartment))
            .unwrap()
    }

    /// A client inside a new compartment whose needs do not include the GPU.
    pub fn compartment(&self, compartment: &str) -> (Client, CompartmentListener) {
        let listener = self.register("test.principal", compartment);
        (connect(&listener.path).unwrap(), listener)
    }

    /// A client inside a new compartment whose needs include the GPU. The
    /// test says so in place of the broker.
    pub fn compartment_with_gpu(&self, compartment: &str) -> (Client, CompartmentListener) {
        let listener = self.register("test.principal", compartment);
        let id = compartment.to_owned();
        self.with(move |state| state.compartment(&id).unwrap().record_gpu_need().unwrap());
        (connect(&listener.path).unwrap(), listener)
    }

    /// How many clients the compositor holds.
    pub fn client_count(&self) -> usize {
        self.with(|state| state.client_count())
    }

    /// Frames that reached the display, and times the compositor looked for
    /// something to draw.
    pub fn drawing(&self) -> (u64, u64) {
        self.with(|state| state.drawing())
    }

    /// Waits until the compositor has stopped drawing: no frame and no look
    /// for one in several frame times. A compositor that never stops fails
    /// the test.
    pub fn wait_until_still(&self) -> (u64, u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let before = self.drawing();
            std::thread::sleep(FRAME * 6);
            if self.drawing() == before {
                return before;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the compositor is still drawing after ten seconds with nothing changing"
            );
        }
    }

    /// The pointer moves to a place on the desktop.
    pub fn pointer(&self, x: i32, y: i32) {
        self.with(move |state| state.pointer_moved(f64::from(x), f64::from(y)));
    }

    /// What an output shows now, drawn by the CPU renderer.
    pub fn screen(&self, output: usize) -> crowbar_compositor::image::Image {
        self.with(move |state| {
            let number = state.outputs()[output].number;
            state.render_output(number).unwrap()
        })
    }
}

/// Connects to a socket by its path, as a program that found it would.
pub fn connect(path: &Path) -> Result<Client, probe::Error> {
    Client::connect(path)
}

impl Drop for Compositor {
    fn drop(&mut self) {
        self.remote.send(Box::new(|state| state.stop()));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
        // A test that failed while it held this lock leaves it poisoned, and
        // the count is still right.
        let mut running = RUNNING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *running -= 1;
        if *running == 0 {
            let _ = std::fs::remove_dir_all(base_dir());
        }
    }
}
