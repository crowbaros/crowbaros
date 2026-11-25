//! What the client asks of the kernel that std does not offer: waiting on
//! several descriptors at once and hearing Ctrl-C as something to act on. All
//! of the client's `unsafe` code is here.

use std::io::{Error, Result};
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicI32, Ordering};

/// The user ID the client runs as.
pub fn uid() -> u32 {
    // SAFETY: `getuid` takes nothing and cannot fail.
    unsafe { libc::getuid() }
}

/// Where the handler below writes a byte for each Ctrl-C.
static INTERRUPTS: AtomicI32 = AtomicI32::new(-1);

extern "C" fn interrupted(_signal: libc::c_int) {
    let pipe = INTERRUPTS.load(Ordering::Relaxed);
    // SAFETY: `write` is safe to call in a signal handler, on a descriptor
    // that stays open for the life of the process. A full pipe drops the
    // byte, and one is already waiting to be read.
    unsafe { libc::write(pipe, [0u8].as_ptr().cast(), 1) };
}

/// From here on Ctrl-C (SIGINT) does not end the client: each one makes the
/// descriptor this gives readable, with one byte. No other signal is
/// touched, so a hangup or a kill ends the client as it would any program.
pub fn interrupts() -> Result<RawFd> {
    let mut ends = [0 as RawFd; 2];
    // SAFETY: `ends` is two descriptors long, as `pipe2` writes.
    if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(Error::last_os_error());
    }
    INTERRUPTS.store(ends[1], Ordering::Relaxed);
    // SAFETY: a zeroed `sigaction` is a valid one, and the handler only
    // calls `write`. SA_RESTART keeps a read that was under way going.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = interrupted as extern "C" fn(libc::c_int) as usize;
        action.sa_flags = libc::SA_RESTART;
        if libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) != 0 {
            return Err(Error::last_os_error());
        }
    }
    Ok(ends[0])
}

/// Waits until one of these descriptors can be read, or has closed, and
/// says which, in their order.
pub fn readable(descriptors: &[RawFd]) -> Result<Vec<bool>> {
    let mut polled: Vec<libc::pollfd> = descriptors
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    loop {
        // SAFETY: `polled` is `polled.len()` entries long.
        if unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, -1) } >= 0 {
            return Ok(polled.iter().map(|entry| entry.revents != 0).collect());
        }
        let error = Error::last_os_error();
        // A Ctrl-C ends the wait early, and has left its byte to find.
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// How many bytes wait to be read on a descriptor, with none of them read,
/// or nothing where the kernel cannot say. A descriptor that is readable
/// with no byte waiting is at its end.
pub fn waiting(descriptor: RawFd) -> Option<usize> {
    let mut count: libc::c_int = 0;
    // SAFETY: FIONREAD writes one `int`, which `count` is.
    let asked = unsafe { libc::ioctl(descriptor, libc::FIONREAD, &mut count) };
    (asked == 0).then_some(count.max(0) as usize)
}

/// Reads what waits on a descriptor, up to `into`'s length, with no buffer
/// between: what `readable` says of the descriptor stays true of it. Zero
/// bytes is its end.
pub fn read(descriptor: RawFd, into: &mut [u8]) -> Result<usize> {
    loop {
        // SAFETY: `into` is `into.len()` bytes long.
        let count = unsafe { libc::read(descriptor, into.as_mut_ptr().cast(), into.len()) };
        if count >= 0 {
            return Ok(count as usize);
        }
        let error = Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
