//! The real shell, run against this compositor, while its only output goes
//! away and comes back (#271).
//!
//! The shell's binary needs a GPU with Vulkan, which CI has not, so this runs
//! only when `CROWBAR_SHELL` names the binary. From `compositor/`:
//!
//! ```sh
//! (cd ../apps/shell && python3 packaging/prepare-gpui.py && cargo build)
//! CROWBAR_SHELL=$PWD/../apps/shell/target/debug/crowbaros-shell \
//!     cargo test --test shell_outputs
//! ```
//!
//! On an NVIDIA GPU, a shell without the fix panics in GPUI's Vulkan backend
//! when the output goes away, as it did on real hardware.

mod common;

use std::{
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use common::{Compositor, OUTPUT_SIZE, own_uid};

/// Starts the shell as its service does: with a connection to the shell's
/// socket in `WAYLAND_SOCKET` and one to the control channel in
/// `CROWBAR_SHELL_CONTROL_FD`. What it says goes to `log`.
fn start_shell(compositor: &Compositor, binary: &str, log: &Path) -> Child {
    let wayland = UnixStream::connect(&compositor.parts.shell.socket).unwrap();
    let control = UnixStream::connect(compositor.dir.join("control")).unwrap();
    for fd in [&wayland, &control] {
        rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty()).unwrap();
    }
    Command::new(binary)
        .env("WAYLAND_SOCKET", wayland.as_raw_fd().to_string())
        .env("CROWBAR_SHELL_CONTROL_FD", control.as_raw_fd().to_string())
        // GPUI picks its Wayland backend by this name. It connects to
        // WAYLAND_SOCKET.
        .env("WAYLAND_DISPLAY", "wayland-unused")
        .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
        .env("RUST_LOG", "crowbaros_shell=info")
        .env("RUST_BACKTRACE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(log).unwrap())
        .spawn()
        .unwrap()
}

/// The shell's layers that show on an output.
fn layers(compositor: &Compositor) -> usize {
    compositor.with(|state| {
        if state.outputs().is_empty() {
            return 0;
        }
        state.layer_rects_by_namespace().len()
    })
}

/// Waits for `probe`, and fails with the shell's log if it never holds or
/// the shell stops first.
fn wait_for(what: &str, shell: &mut Child, log: &Path, mut probe: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !probe() {
        let stopped = shell.try_wait().unwrap();
        if stopped.is_some() || Instant::now() > deadline {
            panic!(
                "{what} never happened; the shell {}:\n{}",
                stopped.map_or("is still running".into(), |status| format!("stopped: {status}")),
                std::fs::read_to_string(log).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Waits up to `timeout` for the shell to stop.
fn stops_within(shell: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = shell.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

#[test]
fn the_shell_waits_for_an_output_and_draws_again_on_it() {
    let Ok(binary) = std::env::var("CROWBAR_SHELL") else {
        eprintln!("CROWBAR_SHELL does not name the shell's binary: skipped");
        return;
    };
    // At 1.5, as a 4K monitor may be: as the output goes, the scale the
    // shell is told drops to 1, which made GPUI resize a window it had
    // already destroyed.
    let compositor = Compositor::start_with(own_uid(), own_uid(), |state| {
        let output = state.add_output("TEST-1", OUTPUT_SIZE, 60_000);
        state.set_output_scale(output, 180).unwrap();
        state.set_backend(crowbar_compositor::backend::headless::Headless::default());
    });
    let first_log = compositor.dir.join("shell-1.log");
    let mut shell = start_shell(&compositor, &binary, &first_log);
    wait_for("the shell's three layers", &mut shell, &first_log, || {
        layers(&compositor) == 3
    });

    // The only output goes away. The compositor closes the shell's layers,
    // and the shell stops cleanly for its service to start it again.
    let number = compositor.with(|state| state.outputs()[0].number);
    compositor.with(move |state| state.remove_output(number));
    let status = stops_within(&mut shell, Duration::from_secs(10));
    if status.is_none() {
        let _ = shell.kill();
    }
    let said = std::fs::read_to_string(&first_log).unwrap();
    assert!(!said.contains("panicked"), "the shell panicked:\n{said}");
    assert!(
        status.is_some_and(|status| status.success()),
        "the shell did not stop cleanly once its layers were closed ({status:?}):\n{said}"
    );

    // Started again with no output, it waits, and makes no layer.
    let second_log = compositor.dir.join("shell-2.log");
    let mut shell = start_shell(&compositor, &binary, &second_log);
    let stopped = stops_within(&mut shell, Duration::from_secs(3));
    let said = std::fs::read_to_string(&second_log).unwrap();
    assert!(
        stopped.is_none(),
        "the shell stopped with no output ({stopped:?}):\n{said}"
    );
    assert!(said.contains("SHELL waits for a display"), "{said}");
    let made = compositor.with(|state| state.layer_rects_by_namespace().len());
    assert_eq!(made, 0, "the shell made layers with no output");

    // The output comes back, and the shell draws its layers on it.
    compositor.with(|state| {
        let output = state.add_output("TEST-1", OUTPUT_SIZE, 60_000);
        state.set_output_scale(output, 180).unwrap();
    });
    wait_for("the shell's layers on the output", &mut shell, &second_log, || {
        layers(&compositor) == 3
    });
    let _ = shell.kill();
    let _ = shell.wait();
}
