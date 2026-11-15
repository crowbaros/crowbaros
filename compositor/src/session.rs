//! Alpha lab service connections. The production protocol is issue 49/71.
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::Path,
    sync::Arc,
};

use smithay::reexports::calloop::{Interest, Mode, PostAction, generic::Generic};

use crate::{State, policy::ClientKind, state::ClientState};

impl State {
    /// Independent runit services reconnect through UID-checked path sockets.
    /// The parent directory is provisioned by Base and is not writable by apps.
    pub fn listen_for_shell(&mut self, directory: &Path, shell_uid: u32) -> std::io::Result<()> {
        for (name, control) in [("wayland", false), ("control", true)] {
            let path = directory.join(name);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            let listener = UnixListener::bind(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o660))?;
            listener.set_nonblocking(true)?;
            self.loop_handle
                .insert_source(
                    Generic::new(listener, Interest::READ, Mode::Level),
                    move |_, listener, state| {
                        while let Ok((stream, _)) = listener.accept() {
                            if !peer_is(&stream, shell_uid) {
                                tracing::warn!("refused non-shell peer on trusted socket");
                                continue;
                            }
                            if control {
                                state.serve_control(stream)?;
                            } else {
                                state
                                    .display_handle
                                    .insert_client(stream, Arc::new(ClientState::new(ClientKind::Shell)))
                                    .map_err(std::io::Error::other)?;
                            }
                        }
                        Ok(PostAction::Continue)
                    },
                )
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        Ok(())
    }
}

pub fn peer_is(stream: &std::os::unix::net::UnixStream, uid: u32) -> bool {
    rustix::net::sockopt::socket_peercred(stream).is_ok_and(|peer| peer.uid.as_raw() == uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_comes_from_the_kernel() {
        let (a, _) = std::os::unix::net::UnixStream::pair().unwrap();
        let uid = rustix::process::getuid().as_raw();
        assert!(peer_is(&a, uid));
        assert!(!peer_is(&a, uid.wrapping_add(1)));
    }
}
