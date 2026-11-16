//! The path socket of a trusted part.
//!
//! The trusted parts find each other by path sockets (ADR 0016). Each is
//! in a directory that only its two ends can enter, and that is the first
//! check. This is the second: the listener asks the kernel which user is
//! at the other end, and serves one user only. A compartment that is given
//! the path all the same is refused on its user ID.

use std::fmt;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use crate::sys::checked;

/// A listening path socket that serves one user.
#[derive(Debug)]
pub struct TrustedListener {
    listener: UnixListener,
    serves: u32,
}

/// A caller the listener hung up on: the user the kernel reported is not
/// the one it serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stranger {
    pub uid: u32,
}

impl TrustedListener {
    /// Listens at `path` for the user `serves`.
    pub fn bind(path: &Path, serves: u32) -> io::Result<Self> {
        Ok(TrustedListener {
            listener: UnixListener::bind(path)?,
            serves,
        })
    }

    /// Waits for the next caller. A caller that is not the user this
    /// listener serves is hung up on before it is read from.
    pub fn accept(&self) -> io::Result<Result<UnixStream, Stranger>> {
        let (stream, _) = self.listener.accept()?;
        // SAFETY: an all-zero `ucred` is valid, and the call writes at most
        // `length` bytes of it.
        let mut caller: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = size_of::<libc::ucred>() as libc::socklen_t;
        checked(unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut caller).cast(),
                &mut length,
            )
        })?;
        if caller.uid == self.serves {
            Ok(Ok(stream))
        } else {
            Ok(Err(Stranger { uid: caller.uid }))
        }
    }
}

impl fmt::Display for Stranger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "user {} is not the user this socket serves", self.uid)
    }
}

impl std::error::Error for Stranger {}
