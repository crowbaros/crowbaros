//! A client's shared memory, mapped so that the client cannot crash the
//! compositor with it.
//!
//! A client hands the compositor a file descriptor and a size, and the
//! compositor maps it. If the client then shrinks the file, reading the
//! mapping past the file's new end raises `SIGBUS`, which would end the
//! compositor. Every read here goes through [`Mapping::read`]: while it runs,
//! a `SIGBUS` handler that knows the mapping replaces it with zero-filled
//! memory, lets the read finish, and reports that the client truncated its
//! file. Any other `SIGBUS` is passed to the handler that was there before.
//!
//! The technique is the one libwayland and Smithay use. This code is adapted
//! from Smithay's `wayland/shm/pool.rs` (MIT; see `LICENSE-smithay.txt`).

use std::{
    cell::Cell,
    io,
    os::fd::{AsFd, BorrowedFd},
    ptr::{self, NonNull},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering, compiler_fence},
    },
};

use rustix::mm::{MapFlags, ProtFlags};

/// The mapping a read is in progress on.
#[derive(Clone, Copy)]
struct Guard {
    start: usize,
    len: usize,
}

thread_local! {
    // Const thread-locals hold no lazy initializer, so the signal handler
    // can use them without allocating.
    static GUARD: Cell<Option<Guard>> = const { Cell::new(None) };
    // Whether a `SIGBUS` hit the read. An atomic, read after a compiler
    // fence: the handler's write is invisible to the optimizer, which would
    // otherwise reuse the value stored before the read.
    static FAULTED: AtomicBool = const { AtomicBool::new(false) };
}

static PREVIOUS_HANDLER: OnceLock<libc::sigaction> = OnceLock::new();

/// The client's file was shorter than it said while the compositor read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truncated;

impl std::fmt::Display for Truncated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the shared memory file is shorter than its pool")
    }
}

impl std::error::Error for Truncated {}

/// A read-only view of a client's shared memory.
pub struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
    /// A `SIGBUS` replaced it with zeros. It is never read again.
    broken: bool,
}

// SAFETY: the mapping is plain memory owned by this value. Moving it to
// another thread moves the ownership; nothing else refers to it.
unsafe impl Send for Mapping {}

impl Mapping {
    /// Maps `len` bytes of `fd` for reading.
    pub fn new(fd: BorrowedFd<'_>, len: usize) -> io::Result<Self> {
        if len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "an empty pool"));
        }
        // SAFETY: a new shared, read-only mapping of the descriptor at an
        // address the kernel chooses. It aliases no Rust object.
        let ptr = unsafe { rustix::mm::mmap(ptr::null_mut(), len, ProtFlags::READ, MapFlags::SHARED, fd, 0)? };
        let ptr = NonNull::new(ptr.cast()).ok_or_else(|| io::Error::other("mmap returned null"))?;
        install_handler();
        Ok(Mapping {
            ptr,
            len,
            broken: false,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Maps the descriptor again at a new, larger size. A pool only grows.
    pub fn grow(&mut self, fd: impl AsFd, len: usize) -> io::Result<()> {
        if len < self.len {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "a pool only grows"));
        }
        let new = Mapping::new(fd.as_fd(), len)?;
        *self = new;
        Ok(())
    }

    /// Reads the mapping. `read` sees the client's bytes as they are now; the
    /// client may change them at any time, so they are pixels to draw and
    /// nothing to trust.
    ///
    /// Returns [`Truncated`] if the client's file was shorter than the
    /// mapping. Then `read` saw zeros past the file's end, and the mapping
    /// stays zeros.
    pub fn read<T>(&mut self, read: impl FnOnce(&[u8]) -> T) -> Result<T, Truncated> {
        if self.broken {
            return Err(Truncated);
        }
        let start = self.ptr.as_ptr() as usize;
        GUARD.with(|guard| {
            assert!(guard.get().is_none(), "a shared-memory read inside another");
            guard.set(Some(Guard { start, len: self.len }));
        });
        FAULTED.with(|faulted| faulted.store(false, Ordering::SeqCst));
        compiler_fence(Ordering::SeqCst);
        // SAFETY: the mapping is `len` readable bytes for as long as `self`
        // lives. A `SIGBUS` while reading it is handled below by replacing
        // the pages with readable zeros, so every byte stays readable.
        let bytes = unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) };
        let result = read(bytes);
        compiler_fence(Ordering::SeqCst);
        GUARD.with(|guard| guard.set(None));
        let faulted = FAULTED.with(|faulted| faulted.swap(false, Ordering::SeqCst));
        if faulted {
            self.broken = true;
            return Err(Truncated);
        }
        Ok(result)
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the mapping was made by `mmap` with this length, and no
        // slice of it outlives a `read` call.
        unsafe {
            let _ = rustix::mm::munmap(self.ptr.as_ptr().cast(), self.len);
        }
    }
}

fn install_handler() {
    PREVIOUS_HANDLER.get_or_init(|| {
        // SAFETY: `sigaction` with a zeroed struct, a handler that is
        // async-signal-safe (see `on_sigbus`), and a place for the previous
        // handler.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_sigbus as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_NODEFER;
            let mut previous: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(libc::SIGBUS, &action, &mut previous) != 0 {
                panic!("could not install the SIGBUS handler: {}", io::Error::last_os_error());
            }
            previous
        }
    });
}

/// Restores the previous handler and raises the signal again, for a `SIGBUS`
/// that is not a shared-memory read.
fn reraise() {
    // SAFETY: both calls are async-signal-safe, and the previous handler was
    // stored before this handler was installed.
    unsafe {
        if let Some(previous) = PREVIOUS_HANDLER.get() {
            libc::sigaction(libc::SIGBUS, previous, ptr::null_mut());
        }
        libc::raise(libc::SIGBUS);
    }
}

extern "C" fn on_sigbus(_signal: libc::c_int, info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
    // SAFETY: the kernel passes a valid `siginfo_t` for a SA_SIGINFO handler.
    let address = unsafe { (*info).si_addr() } as usize;
    // Only async-signal-safe work below: a const thread-local, and `mmap`.
    let guard = GUARD.with(Cell::get);
    match guard {
        Some(active) if address >= active.start && address < active.start + active.len => {
            // SAFETY: the range is exactly the mapping being read, which this
            // thread owns. A fixed private anonymous mapping replaces its
            // pages with zeros, so the read can finish.
            let replaced = unsafe {
                libc::mmap(
                    active.start as *mut libc::c_void,
                    active.len,
                    libc::PROT_READ,
                    libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if replaced == libc::MAP_FAILED {
                reraise();
                return;
            }
            FAULTED.with(|faulted| faulted.store(true, Ordering::SeqCst));
        }
        _ => reraise(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;

    fn memfd(len: u64) -> OwnedFd {
        let fd = rustix::fs::memfd_create("fulcrum-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        rustix::fs::ftruncate(&fd, len).unwrap();
        fd
    }

    #[test]
    fn a_mapping_reads_what_the_client_wrote() {
        let fd = memfd(4096);
        rustix::io::pwrite(&fd, b"pixels", 100).unwrap();
        let mut mapping = Mapping::new(fd.as_fd(), 4096).unwrap();
        let read = mapping.read(|bytes| bytes[100..106].to_vec()).unwrap();
        assert_eq!(read, b"pixels");
    }

    #[test]
    fn a_client_that_truncates_its_file_cannot_crash_the_reader() {
        let fd = memfd(64 * 1024);
        let mut mapping = Mapping::new(fd.as_fd(), 64 * 1024).unwrap();
        rustix::fs::ftruncate(&fd, 4096).unwrap();
        // Reading past the new end would raise SIGBUS and end the process.
        let result = mapping.read(|bytes| bytes.iter().map(|byte| u64::from(*byte)).sum::<u64>());
        assert_eq!(result, Err(Truncated));
        // The mapping stays broken, and says so.
        assert_eq!(mapping.read(|bytes| bytes.len()), Err(Truncated));
    }

    #[test]
    fn a_pool_only_grows() {
        let fd = memfd(8192);
        let mut mapping = Mapping::new(fd.as_fd(), 4096).unwrap();
        assert!(mapping.grow(&fd, 2048).is_err());
        mapping.grow(&fd, 8192).unwrap();
        assert_eq!(mapping.len(), 8192);
    }

    #[test]
    fn an_empty_mapping_is_refused() {
        let fd = memfd(0);
        assert!(Mapping::new(fd.as_fd(), 0).is_err());
    }
}
