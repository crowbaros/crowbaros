//! Telling a dma-buf from other files, and reading one with the CPU.
//!
//! The CPU fallback maps real dma-bufs and brackets pixel copies with
//! `DMA_BUF_IOCTL_SYNC`. Regular files used by the software backend are read
//! separately with positioned file I/O (see `shm.rs`).
//!
//! Only a dma-buf goes to the GPU's import. A software renderer imports a
//! memfd by mapping it, so a client that shrank one could crash the
//! compositor. A dma-buf has a fixed size for the lifetime of its descriptor.

use std::{
    io,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    ptr::{self, NonNull},
};

use rustix::{
    fs::{SeekFrom, seek},
    ioctl::{Setter, ioctl, opcode},
    mm::{MapFlags, ProtFlags, mmap, munmap},
};

pub(crate) struct CpuMapping {
    fd: OwnedFd,
    start: NonNull<u8>,
    len: usize,
}

// SAFETY: this value exclusively owns the mapping, and no reference into it
// escapes. Moving the owner transfers responsibility for unmapping it.
unsafe impl Send for CpuMapping {}

impl CpuMapping {
    pub(crate) fn new(fd: OwnedFd, len: usize) -> io::Result<Self> {
        if !is_dmabuf(fd.as_fd()) || len == 0 || len > isize::MAX as usize || seek(&fd, SeekFrom::End(0))? < len as u64
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid DMA-BUF mapping"));
        }
        // SAFETY: the kernel chooses the address of a new read-only mapping
        // within the fixed bounds of a verified DMA-BUF.
        let start = unsafe { mmap(ptr::null_mut(), len, ProtFlags::READ, MapFlags::SHARED, &fd, 0)? };
        let Some(start) = NonNull::new(start.cast::<u8>()) else {
            // SAFETY: mmap succeeded at this address with this length.
            let _ = unsafe { munmap(start, len) };
            return Err(io::Error::other("mmap returned null"));
        };
        Ok(Self { fd, start, len })
    }

    pub(crate) fn with_cpu_read(&self, copy: impl FnOnce(&Self) -> io::Result<()>) -> io::Result<()> {
        sync(self.fd.as_fd(), SYNC_START | SYNC_READ)?;
        let result = copy(self);
        let finished = sync(self.fd.as_fd(), SYNC_END | SYNC_READ);
        result.and(finished)
    }

    pub(crate) fn read_at(&self, offset: usize, bytes: &mut [u8]) -> io::Result<()> {
        if offset.checked_add(bytes.len()).is_none_or(|end| end > self.len) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "pixel rows exceed the DMA-BUF",
            ));
        }
        // SAFETY: the source lies inside this live mapping. The destination
        // belongs to the caller and cannot alias the mapping, which has never
        // been exposed as a Rust reference.
        unsafe { ptr::copy_nonoverlapping(self.start.as_ptr().add(offset), bytes.as_mut_ptr(), bytes.len()) };
        Ok(())
    }
}

impl Drop for CpuMapping {
    fn drop(&mut self) {
        // SAFETY: this value owns the mapping of this address and length.
        let _ = unsafe { munmap(self.start.as_ptr().cast(), self.len) };
    }
}

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

fn sync(fd: BorrowedFd<'_>, flags: u64) -> io::Result<()> {
    loop {
        // SAFETY: the opcode is `_IOW('b', 0, struct dma_buf_sync)`, which the
        // kernel only reads.
        let result = unsafe { ioctl(fd, Setter::<{ opcode::write::<Sync>(b'b', 0) }, _>::new(Sync { flags })) };
        match result {
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => continue,
            result => return Ok(result?),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memfd_is_not_a_dma_buf() {
        let fd = rustix::fs::memfd_create("not-a-dmabuf", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        assert!(!is_dmabuf(std::os::fd::AsFd::as_fd(&fd)));
    }

    #[test]
    fn a_linear_buffer_gbm_exports_can_be_read_through_a_cpu_mapping() {
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
            .create_buffer(4, 4, format, &[], crate::gbm::USE_RENDERING | crate::gbm::USE_LINEAR)
            .unwrap();
        let fd = buffer.export(0).unwrap();
        let mapping = CpuMapping::new(fd, 16).unwrap();
        mapping
            .with_cpu_read(|mapping| mapping.read_at(0, &mut [0; 16]))
            .unwrap();
    }
}
