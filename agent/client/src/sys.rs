//! What the client asks of the kernel that std does not offer: waiting on
//! several descriptors at once, hearing Ctrl-C as something to act on, and
//! the terminal's echo. All of the client's `unsafe` code is here.

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

/// The terminal shows nothing typed, for as long as this is held: a secret
/// is being typed. Dropping it puts the terminal back as it was.
pub struct Hidden {
    before: Option<libc::termios>,
}

impl Hidden {
    /// Turns echo off on standard input, when it is a terminal.
    pub fn typing() -> Hidden {
        // SAFETY: a zeroed `termios` is a valid one for `tcgetattr` to fill.
        let mut settings: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `settings` is a `termios`, and descriptor 0 is the process's own.
        if unsafe { libc::tcgetattr(0, &mut settings) } != 0 {
            return Hidden { before: None };
        }
        let before = settings;
        settings.c_lflag &= !libc::ECHO;
        // SAFETY: as above.
        unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, &settings) };
        Hidden { before: Some(before) }
    }
}

impl Drop for Hidden {
    fn drop(&mut self) {
        if let Some(before) = &self.before {
            // SAFETY: `before` is the `termios` the terminal had.
            unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, before) };
        }
    }
}
