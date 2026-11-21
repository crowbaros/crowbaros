//! Ctrl+Alt+Del on Base's desktop, `crowbar-ctrl-alt-del`.
//!
//! On a console the kernel sees Ctrl+Alt+Del and sends SIGINT to init, and
//! runit runs /etc/runit/ctrlaltdel, which reboots. The compositor takes the
//! keyboard from the kernel, so it sees the chord instead, and says so at
//! this service's socket. The service, as root, sends init the same signal.
//! It takes the word of the compositor's account alone, and only one line,
//! `ctrl-alt-del`.

use crate::sys;
use std::io::Read;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::time::Duration;

const SOCKET: &str = "/run/crowbar-ctrl-alt-del/socket";
const COMPOSITOR: &str = "crowbar-compositor";
const REQUEST: &[u8] = b"ctrl-alt-del\n";

/// Whether a peer with this user ID, saying what `stream` holds, is the
/// compositor saying `ctrl-alt-del`. Only the compositor's is read.
pub fn heard(peer: u32, compositor_uid: u32, stream: &mut impl Read) -> bool {
    if peer != compositor_uid {
        return false;
    }
    let mut request = Vec::with_capacity(REQUEST.len());
    stream.take(REQUEST.len() as u64).read_to_end(&mut request).is_ok() && request == REQUEST
}

/// Answers one connection, and reboots if the compositor asked.
pub fn serve_one(listener: &UnixListener, compositor_uid: u32, reboot: &mut impl FnMut()) {
    let Ok((mut connection, _)) = listener.accept() else {
        return;
    };
    let _ = connection.set_read_timeout(Some(Duration::from_secs(2)));
    match sys::peer_uid(&connection) {
        Ok(peer) if heard(peer, compositor_uid, &mut connection) => {
            println!("CTRL-ALT-DEL rebooting");
            reboot();
        }
        Ok(_) => println!("CTRL-ALT-DEL refused"),
        Err(error) => println!("CTRL-ALT-DEL {error}"),
    }
}

/// `crowbar-ctrl-alt-del`, as runit starts it.
pub fn main(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("usage: crowbar-ctrl-alt-del".into());
    }
    let compositor = sys::require(sys::account_by_name(COMPOSITOR), COMPOSITOR)?;
    let listener = sys::listen_as(Path::new(SOCKET), 0, compositor.gid).map_err(|e| e.to_string())?;
    loop {
        serve_one(&listener, compositor.uid, &mut sys::ctrl_alt_del_to_init);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    const COMPOSITOR: u32 = 1000;

    fn said(data: &[u8], peer: u32) -> bool {
        heard(peer, COMPOSITOR, &mut &data[..])
    }

    #[test]
    fn the_compositor_saying_the_chord_is_heard() {
        assert!(said(b"ctrl-alt-del\n", COMPOSITOR));
    }

    #[test]
    fn anyone_else_or_anything_else_is_not() {
        assert!(!said(b"ctrl-alt-del\n", 1003));
        assert!(!said(b"ctrl-alt-del\n", 1001));
        assert!(!said(b"ctrl-alt-del\n", 0));
        assert!(!said(b"reboot\n", COMPOSITOR));
        assert!(!said(b"ctrl-alt-", COMPOSITOR));
        assert!(!said(b"", COMPOSITOR));
    }

    /// Serves one connection at a fresh socket, from a peer that sends
    /// `data`, and says whether it rebooted.
    fn rebooted_over_socket(data: &'static [u8], compositor_uid: u32) -> bool {
        let dir = crate::sys::tests::TempDir::new();
        let path = dir.0.join("socket");
        let listener = UnixListener::bind(&path).unwrap();
        let sender = std::thread::spawn(move || {
            let mut stream = UnixStream::connect(&path).unwrap();
            let _ = stream.write_all(data);
        });
        let mut rebooted = false;
        serve_one(&listener, compositor_uid, &mut || rebooted = true);
        sender.join().unwrap();
        rebooted
    }

    #[test]
    fn over_its_socket_only_the_compositors_account_reboots() {
        assert!(rebooted_over_socket(b"ctrl-alt-del\n", crate::sys::uid()));
        assert!(!rebooted_over_socket(b"ctrl-alt-del\n", crate::sys::uid() + 1));
        assert!(!rebooted_over_socket(b"reboot\n", crate::sys::uid()));
    }
}
