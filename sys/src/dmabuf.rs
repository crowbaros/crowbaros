//! Telling a dma-buf from other files, and reading one with the CPU.
//!
//! Where no GPU imports a client's dma-buf, the compositor maps it and
//! copies its pixels, as it does shared memory, through the same guarded
//! mapping (see `shm.rs`). Around each read it asks the kernel to wait for
//! the GPU work writing the buffer, as `DMA_BUF_IOCTL_SYNC` does. A descriptor
//! that is not a dma-buf, as a memfd, has no such work, and the request
//! fails harmlessly.
//!
//! Only a dma-buf goes to the GPU's import. A software renderer imports a
//! memfd by mapping it, and reads it outside the guarded mapping, so a client
//! that shrank one would crash the compositor. A dma-buf cannot shrink.

use std::os::fd::BorrowedFd;

use rustix::ioctl::{Setter, ioctl, opcode};

/// `struct dma_buf_sync`.
#[repr(C)]
struct Sync {
    flags: u64,
}

/// `DMA_BUF_MAGIC`: the file system every dma-buf is on.
const DMA_BUF_MAGIC: i64 = 0x444d_4142;

/// Whether a descriptor is a dma-buf.
pub fn is_dmabuf(fd: BorrowedFd<'_>) -> bool {
    // `f_type` is narrower on some 32-bit targets.
    #[allow(clippy::unnecessary_cast)]
    rustix::fs::fstatfs(fd).is_ok_and(|stat| stat.f_type as i64 == DMA_BUF_MAGIC)
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
        assert!(!is_dmabuf(std::os::fd::AsFd::as_fd(&fd)));
        begin_read(std::os::fd::AsFd::as_fd(&fd));
        end_read(std::os::fd::AsFd::as_fd(&fd));
    }

    #[test]
    fn a_buffer_gbm_exports_is_a_dma_buf() {
        let Some(node) = std::fs::read_dir("/dev/dri").ok().and_then(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .find(|path| path.to_string_lossy().contains("renderD"))
        }) else {
            eprintln!("no render node: skipped");
            return;
        };
        let Ok(device) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(node)
            .map_err(drop)
            .and_then(|file| crate::gbm::Device::new(file.into()).map_err(drop))
        else {
            eprintln!("no GBM on the render node: skipped");
            return;
        };
        let format = u32::from_le_bytes(*b"XR24");
        let buffer = device
            .create_buffer(4, 4, format, &[], crate::gbm::USE_RENDERING)
            .unwrap();
        let fd = buffer.export(0).unwrap();
        assert!(is_dmabuf(std::os::fd::AsFd::as_fd(&fd)));
    }
}
