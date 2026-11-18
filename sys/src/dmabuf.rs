//! Reading a dma-buf with the CPU.
//!
//! Where no GPU imports a client's dma-buf, the compositor maps it and
//! copies its pixels, as it does shared memory, through the same guarded
//! mapping (see `shm.rs`). Around each read it asks the kernel to wait for
//! the GPU work writing the buffer, as `DMA_BUF_IOCTL_SYNC` does. A descriptor
//! that is not a dma-buf, as a memfd, has no such work, and the request
//! fails harmlessly.

use std::os::fd::BorrowedFd;

use rustix::ioctl::{Setter, ioctl, opcode};

/// `struct dma_buf_sync`.
#[repr(C)]
struct Sync {
    flags: u64,
}

const SYNC_READ: u64 = 1;
const SYNC_START: u64 = 0;
const SYNC_END: u64 = 4;

fn sync(fd: BorrowedFd<'_>, flags: u64) {
    // SAFETY: the opcode is `_IOW('b', 0, struct dma_buf_sync)`, which the
    // kernel only reads.
    let _ = unsafe { ioctl(fd, Setter::<{ opcode::write::<Sync>(b'b', 0) }, _>::new(Sync { flags })) };
}

/// Before the CPU reads: waits for the GPU to finish writing.
pub fn begin_read(fd: BorrowedFd<'_>) {
    sync(fd, SYNC_START | SYNC_READ);
}

/// After the CPU has read.
pub fn end_read(fd: BorrowedFd<'_>) {
    sync(fd, SYNC_END | SYNC_READ);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memfd_is_not_a_dma_buf_and_syncing_it_does_nothing() {
        let fd = rustix::fs::memfd_create("not-a-dmabuf", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        begin_read(std::os::fd::AsFd::as_fd(&fd));
        end_read(std::os::fd::AsFd::as_fd(&fd));
    }
}
