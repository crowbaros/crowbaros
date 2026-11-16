//! Runs the compositor inside the test process with no display, and hands out
//! real Wayland client connections to it.
//!
//! The clients connect the two ways the compositor allows (ADR 0016): to a
//! trusted part's path socket, or through a listener that a stand-in for the
//! Warden registers. There is no other way in to give a test.
#![allow(dead_code)]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Once,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

use crowbar_compositor::{
    State,
    backend::{BACKGROUND, Backend, Drawn},
    connections::{TrustedPart, TrustedParts},
};
use crowbar_probe::{self as probe, Client, CompartmentListener};
use smithay::{
    backend::{
        allocator::{Format, Fourcc, Modifier},
        input::{ButtonState, InputTime, KeyState},
        renderer::{
            damage::OutputDamageTracker,
            test::{DummyFramebuffer, DummyRenderer},
        },
    },
    desktop::space::space_render_elements,
    output::Output,
    reexports::{
        calloop::{self, EventLoop},
        wayland_server::Display,
    },
    wayland::dmabuf::DmabufFeedbackBuilder,
};

type Job = Box<dyn FnOnce(&mut State) + Send>;

/// The test output.
pub const OUTPUT_SIZE: (i32, i32) = (1280, 800);
/// One frame on the test output, which runs at 60 Hz.
pub const FRAME: Duration = Duration::from_millis(17);

pub struct Compositor {
    /// A directory of this compositor's own. It holds the two trusted
    /// sockets and the listeners the tests register.
    pub dir: PathBuf,
    pub parts: TrustedParts,
    jobs: calloop::channel::Sender<Job>,
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
        std::fs::create_dir_all(&dir).unwrap();
        // Safety: set once, before any compositor thread starts.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
    });
    dir
}

fn base_dir() -> PathBuf {
    std::env::temp_dir().join(format!("crowbar-compositor-test-{}", std::process::id()))
}

/// Draws with Smithay's renderer that needs no display. It tracks damage as
/// the real backends do, so it draws a frame only when something changed.
#[derive(Default)]
struct Headless {
    renderer: DummyRenderer,
    damage: HashMap<String, OutputDamageTracker>,
}

impl Backend for Headless {
    fn draw(&mut self, state: &State, output: &Output) -> Drawn {
        let Ok(elements) = space_render_elements(&mut self.renderer, [&state.space], output, 1.0) else {
            return Drawn::Nothing;
        };
        let damage = self
            .damage
            .entry(output.name())
            .or_insert_with(|| OutputDamageTracker::from_output(output));
        match damage.render_output(&mut self.renderer, &mut DummyFramebuffer, 1, &elements, BACKGROUND) {
            Ok(result) if result.damage.is_some() => Drawn::Sent,
            _ => Drawn::Nothing,
        }
    }
}

impl Compositor {
    /// A compositor that takes this process for both the shell and the Warden.
    pub fn start() -> Self {
        Self::start_trusting(own_uid(), own_uid())
    }

    /// A compositor that takes these user IDs for the shell and the Warden.
    pub fn start_trusting(shell_uid: u32, warden_uid: u32) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        runtime_dir();
        let dir = base_dir().join(format!("compositor-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
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
        let (jobs, jobs_rx) = calloop::channel::channel::<Job>();
        let listen_on = parts.clone();
        let thread = std::thread::spawn(move || {
            let mut event_loop: EventLoop<State> = EventLoop::try_new().unwrap();
            let display: Display<State> = Display::new().unwrap();
            let mut state = State::new(&mut event_loop, display);
            state.add_output("TEST-1", OUTPUT_SIZE, 60_000);
            state.set_backend(Headless::default());
            // No renderer here takes a dmabuf. The global exists so that the
            // tests see who is offered it.
            let formats = [Format {
                code: Fourcc::Argb8888,
                modifier: Modifier::Linear,
            }];
            let feedback = DmabufFeedbackBuilder::new(rustix::fs::makedev(226, 128), formats)
                .build()
                .unwrap();
            state.offer_dmabuf(&feedback);
            event_loop
                .handle()
                .insert_source(jobs_rx, |event, _, state| match event {
                    calloop::channel::Event::Msg(job) => job(state),
                    calloop::channel::Event::Closed => state.loop_signal.stop(),
                })
                .unwrap();
            state.listen(&listen_on).unwrap();
            ready_tx.send(()).unwrap();
            event_loop
                .run(None, &mut state, |state| {
                    let _ = state.display_handle.flush_clients();
                })
                .unwrap();
        });
        ready_rx.recv().unwrap();
        Compositor {
            dir,
            parts,
            jobs,
            thread: Some(thread),
        }
    }

    /// Runs `f` on the compositor thread and returns its result.
    pub fn with<T: Send + 'static>(&self, f: impl FnOnce(&mut State) -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        self.jobs
            .send(Box::new(move |state| {
                let _ = tx.send(f(state));
            }))
            .unwrap();
        rx.recv().unwrap()
    }

    fn time() -> InputTime {
        static TIME: AtomicU32 = AtomicU32::new(1);
        InputTime::from_millis(TIME.fetch_add(1, Ordering::Relaxed))
    }

    /// A key goes down or up on the compositor's keyboard. `evdev` is the
    /// `KEY_*` code from `linux/input-event-codes.h`.
    pub fn key(&self, evdev: u32, pressed: bool) {
        let state = if pressed { KeyState::Pressed } else { KeyState::Released };
        self.with(move |compositor| compositor.handle_key((evdev + 8).into(), state, Self::time()));
    }

    /// The pointer moves to a place on the desktop.
    pub fn pointer(&self, x: i32, y: i32) {
        self.with(move |compositor| compositor.pointer_moved((f64::from(x), f64::from(y)).into(), Self::time()));
    }

    /// The pointer moves to a place on the desktop, and its left button is
    /// pressed and let go.
    pub fn click(&self, x: i32, y: i32) {
        const BTN_LEFT: u32 = 0x110;
        self.pointer(x, y);
        self.with(|compositor| {
            compositor.pointer_button(BTN_LEFT, ButtonState::Pressed, Self::time());
            compositor.pointer_button(BTN_LEFT, ButtonState::Released, Self::time());
        });
    }

    /// Frames that reached the display, and times the compositor looked for
    /// something to draw.
    pub fn drawing(&self) -> (u64, u64) {
        self.with(|state| (state.frames_drawn, state.draw_passes))
    }

    /// Waits until the compositor has stopped drawing: no frame and no look
    /// for one in several frame times. A compositor that never stops fails
    /// the test.
    pub fn wait_until_still(&self) -> (u64, u64) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
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

    /// Connects to the shell's socket.
    pub fn try_shell(&self) -> Result<Client, probe::Error> {
        Client::new(probe::connect(&self.parts.shell.socket)?)
    }

    /// The shell: a client of the shell's socket.
    pub fn shell(&self) -> Client {
        self.try_shell().expect("the compositor refused the shell")
    }

    /// Connects to the Warden's socket.
    pub fn try_warden(&self) -> Result<Client, probe::Error> {
        Client::new(probe::connect(&self.parts.warden.socket)?)
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
    /// test says so in place of the part of Base that will.
    pub fn compartment_with_gpu(&self, compartment: &str) -> (Client, CompartmentListener) {
        let listener = self.register("test.principal", compartment);
        let id = compartment.to_owned();
        self.with(move |state| state.compartment(&id).unwrap().record_gpu_need());
        (connect(&listener.path).unwrap(), listener)
    }
}

/// Connects to a socket by its path, as a program that found it would.
pub fn connect(path: &Path) -> Result<Client, probe::Error> {
    Client::new(probe::connect(path)?)
}

impl Drop for Compositor {
    fn drop(&mut self) {
        let _ = self.jobs.send(Box::new(|state| state.loop_signal.stop()));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
