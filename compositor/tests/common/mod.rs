//! Runs the compositor inside the test process with no display backend, and
//! hands out real Wayland client connections to it.
#![allow(dead_code)]

use std::{
    path::PathBuf,
    sync::{Once, mpsc},
    thread::JoinHandle,
};

use crowbar_compositor::{State, probe};
use smithay::reexports::{
    calloop::{self, EventLoop},
    wayland_server::Display,
};

type Job = Box<dyn FnOnce(&mut State) + Send>;

pub struct Compositor {
    pub socket: PathBuf,
    jobs: calloop::channel::Sender<Job>,
    thread: Option<JoinHandle<()>>,
}

static RUNTIME_DIR: Once = Once::new();

/// Sockets go in a private directory, not the person's runtime directory.
fn runtime_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crowbar-compositor-test-{}", std::process::id()));
    RUNTIME_DIR.call_once(|| {
        std::fs::create_dir_all(&dir).unwrap();
        // Set once, before any compositor thread starts.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
    });
    dir
}

impl Compositor {
    pub fn start() -> Self {
        let dir = runtime_dir();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (jobs, jobs_rx) = calloop::channel::channel::<Job>();
        let thread = std::thread::spawn(move || {
            let mut event_loop: EventLoop<State> = EventLoop::try_new().unwrap();
            let display: Display<State> = Display::new().unwrap();
            let mut state = State::new(&mut event_loop, display);
            state.add_output("TEST-1", (1280, 800), 1.0);
            event_loop
                .handle()
                .insert_source(jobs_rx, |event, _, state| match event {
                    calloop::channel::Event::Msg(job) => job(state),
                    calloop::channel::Event::Closed => state.loop_signal.stop(),
                })
                .unwrap();
            ready_tx.send(state.socket_name.clone()).unwrap();
            event_loop
                .run(None, &mut state, |state| {
                    let _ = state.display_handle.flush_clients();
                })
                .unwrap();
        });
        let socket = dir.join(ready_rx.recv().unwrap());
        Compositor {
            socket,
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

    /// A key goes down or up on the compositor's keyboard. `evdev` is the
    /// `KEY_*` code from `linux/input-event-codes.h`.
    pub fn key(&self, evdev: u32, pressed: bool) {
        use smithay::backend::input::{InputTime, KeyState};
        static TIME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        let time = InputTime::from_millis(TIME.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let state = if pressed { KeyState::Pressed } else { KeyState::Released };
        self.with(move |compositor| compositor.handle_key((evdev + 8).into(), state, time));
    }

    /// A client that connects to the public socket, as any program could.
    pub fn ordinary_client(&self) -> probe::Client {
        probe::Client::new(probe::connect(&self.socket).unwrap()).unwrap()
    }

    /// A client that arrives through a security-context listener which an
    /// ordinary client registered.
    pub fn sandboxed_client(&self) -> (probe::Client, probe::Sandbox) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = runtime_dir().join(format!("sandbox-{n}"));
        let sandbox = self.ordinary_client().sandbox(&path).unwrap();
        let client = probe::Client::new(probe::connect(&sandbox.path).unwrap()).unwrap();
        (client, sandbox)
    }

    /// The shell's client: the connection the compositor itself opened.
    pub fn shell_client(&self) -> probe::Client {
        let stream = self.with(|state| state.connect_shell().unwrap());
        probe::Client::new(probe::from_stream(stream).unwrap()).unwrap()
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        let _ = self.jobs.send(Box::new(|state| state.loop_signal.stop()));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
