//! `crowbar-shell-connect`: connects the separately supervised shell to the
//! compositor without inheriting seat access.
//!
//! It runs as the shell's account, connects to the compositor's Wayland
//! socket and its control channel, makes sure the compositor's account serves
//! both, and becomes the shell with the two connections, as WAYLAND_SOCKET
//! and CROWBAR_SHELL_CONTROL_FD.

use crate::sys;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::time::Duration;

const DIRECTORY: &str = "/run/crowbar-trusted/compositor";
const COMPOSITOR: &str = "crowbar-compositor";
const SHELL_PROGRAM: &str = "/usr/local/bin/crowbaros-shell";

/// A connection to the socket at `path`, served by `compositor_uid`. While
/// the compositor is not yet listening, it tries again, `attempts` times in
/// all, `delay` apart.
pub fn connect(path: &Path, compositor_uid: u32, attempts: u32, delay: Duration) -> io::Result<UnixStream> {
    for _ in 0..attempts {
        match UnixStream::connect(path) {
            Ok(channel) => {
                if sys::peer_uid(&channel)? != compositor_uid {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Wrong compositor service identity",
                    ));
                }
                return Ok(channel);
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {
                std::thread::sleep(delay);
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Compositor service did not become ready",
    ))
}

/// `crowbar-shell-connect`, as the shell's runit service starts it.
pub fn main(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("usage: crowbar-shell-connect".into());
    }
    let compositor = sys::require(sys::account_by_name(COMPOSITOR), COMPOSITOR)?;
    let mut channels = vec![];
    for name in ["wayland", "control"] {
        let channel = connect(
            &Path::new(DIRECTORY).join(name),
            compositor.uid,
            100,
            Duration::from_millis(200),
        )
        .map_err(|e| e.to_string())?;
        sys::keep_across_exec(&channel).map_err(|e| e.to_string())?;
        channels.push(channel);
    }
    let error = std::process::Command::new(SHELL_PROGRAM)
        .arg0("crowbaros-shell")
        .env("WAYLAND_SOCKET", channels[0].as_raw_fd().to_string())
        .env("CROWBAR_SHELL_CONTROL_FD", channels[1].as_raw_fd().to_string())
        .exec();
    Err(format!("{SHELL_PROGRAM}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::tests::TempDir;
    use std::os::unix::net::UnixListener;

    const QUICK: Duration = Duration::from_millis(10);

    #[test]
    fn the_shell_connects_to_the_compositors_account() {
        let dir = TempDir::new();
        let path = dir.0.join("wayland");
        let _listener = UnixListener::bind(&path).unwrap();
        assert!(connect(&path, crate::sys::uid(), 3, QUICK).is_ok());
    }

    #[test]
    fn a_socket_served_by_another_account_is_refused() {
        let dir = TempDir::new();
        let path = dir.0.join("wayland");
        let _listener = UnixListener::bind(&path).unwrap();
        let error = connect(&path, crate::sys::uid() + 1, 3, QUICK).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn the_shell_waits_for_the_compositor_and_then_gives_up() {
        let dir = TempDir::new();
        let path = dir.0.join("wayland");
        let started = std::time::Instant::now();
        assert!(connect(&path, crate::sys::uid(), 3, QUICK).is_err());
        assert!(started.elapsed() >= QUICK * 3);
        // A socket that appears while the shell waits is taken.
        let late = path.clone();
        let server = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let listener = UnixListener::bind(&late).unwrap();
            listener.accept().unwrap();
        });
        assert!(connect(&path, crate::sys::uid(), 100, QUICK).is_ok());
        server.join().unwrap();
    }
}
