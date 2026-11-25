//! Linux system calls std does not expose: peer credentials, signal pipe,
//! polling, and file mode helpers.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};

static SIGNAL_PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn handle_signal(sig: libc::c_int) {
    let fd = SIGNAL_PIPE_WRITE.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = sig as u8;
        // SAFETY: `write` to a non-blocking pipe is async-signal-safe, and
        // `errno` is put back so the call this handler interrupted still
        // reports its own error.
        unsafe {
            let errno = libc::__errno_location();
            let saved = *errno;
            let _ = libc::write(fd, (&raw const byte).cast(), 1);
            *errno = saved;
        }
    }
}

/// Receives `SIGTERM` and `SIGINT` over a non-blocking pipe readable by `poll`.
pub struct SignalPipe {
    read: OwnedFd,
    _write: OwnedFd,
}

impl SignalPipe {
    /// Installs signal handlers for `SIGTERM` and `SIGINT` and returns the
    /// readable pipe end.
    pub fn install() -> io::Result<Self> {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `fds` is a valid two-element array for `pipe2`.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `pipe2` succeeded and gave us two owned descriptors.
        let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        SIGNAL_PIPE_WRITE.store(write.as_raw_fd(), Ordering::Release);

        // SAFETY: `handle_signal` only performs an atomic load and `write(2)`.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handle_signal as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_flags = 0;
            for sig in [libc::SIGTERM, libc::SIGINT] {
                if libc::sigaction(sig, &action, std::ptr::null_mut()) != 0 {
                    SIGNAL_PIPE_WRITE.store(-1, Ordering::Release);
                    return Err(io::Error::last_os_error());
                }
            }
        }

        Ok(Self { read, _write: write })
    }

    pub fn fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }

    /// Reads a pending signal number, if any arrived.
    pub fn take_signal(&self) -> Option<libc::c_int> {
        let mut buf = [0u8; 16];
        // SAFETY: reading from an owned non-blocking pipe descriptor.
        let n = unsafe { libc::read(self.read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 { Some(buf[0] as libc::c_int) } else { None }
    }
}

impl Drop for SignalPipe {
    fn drop(&mut self) {
        SIGNAL_PIPE_WRITE.store(-1, Ordering::Release);
    }
}

/// Human-readable name for a stop signal.
pub fn signal_name(sig: libc::c_int) -> &'static str {
    match sig {
        libc::SIGTERM => "SIGTERM",
        libc::SIGINT => "SIGINT",
        _ => "SIGNAL",
    }
}

/// The user ID of the peer connected to a Unix stream socket, as recorded by
/// the kernel at `connect(2)`.
pub fn peer_uid(socket: &impl AsRawFd) -> io::Result<u32> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` match the kernel's `SO_PEERCRED` struct layout.
    let rc = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

/// Waits on `fds` until at least one descriptor is ready or a signal interrupts.
pub fn poll(fds: &mut [libc::pollfd], timeout_ms: libc::c_int) -> io::Result<usize> {
    // SAFETY: `fds` is a valid mutable slice of `pollfd`.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(0);
        }
        return Err(err);
    }
    Ok(rc as usize)
}

/// Sets the file mode bits on an open descriptor.
pub fn fchmod(fd: &impl AsRawFd, mode: u32) -> io::Result<()> {
    // SAFETY: `fchmod` on a valid open file descriptor.
    if unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sets the file mode bits on a path.
pub fn chmod(path: &Path, mode: u32) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Removes `path` only if it is an existing Unix socket left behind by an
/// earlier process that did not stop cleanly.
pub fn remove_stale_socket(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
