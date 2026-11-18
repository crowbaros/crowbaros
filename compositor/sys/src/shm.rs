//! Copies client pixels into compositor-owned memory.

use std::{
    fs::File,
    io, mem,
    os::{
        fd::{AsFd, OwnedFd},
        unix::fs::FileExt,
    },
};

use rustix::fs::{OFlags, fcntl_getfl};

use crate::dmabuf::{CpuMapping, is_dmabuf};

enum Storage {
    File(File),
    Dmabuf(CpuMapping),
}

/// A shared file or a linear DMA-BUF with a declared byte length.
pub struct BufferFile {
    storage: Storage,
    len: usize,
}

impl BufferFile {
    /// A shared-memory pool backed by a readable regular file.
    pub fn new(fd: OwnedFd, len: usize) -> io::Result<Self> {
        let file = File::from(fd);
        let access = fcntl_getfl(&file)? & (OFlags::ACCMODE | OFlags::PATH);
        if access != OFlags::RDONLY && access != OFlags::RDWR {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the pool needs a readable file",
            ));
        }
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the pool needs a regular file",
            ));
        }
        check_file_size(metadata.len(), len)?;
        Ok(Self {
            storage: Storage::File(file),
            len,
        })
    }

    /// A linear DMA-BUF, or a regular file backing a software test buffer.
    pub fn from_dmabuf(fd: OwnedFd, len: usize) -> io::Result<Self> {
        if !is_dmabuf(fd.as_fd()) {
            return Self::new(fd, len);
        }
        let mapping = CpuMapping::new(fd, len)?;
        Ok(Self {
            storage: Storage::Dmabuf(mapping),
            len,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Changes the declared length of a pool after the client grows its file.
    pub fn grow(&mut self, len: usize) -> io::Result<()> {
        let Storage::File(file) = &self.storage else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a DMA-BUF has a fixed size",
            ));
        };
        if len < self.len {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "a pool only grows"));
        }
        check_file_size(file.metadata()?.len(), len)?;
        self.len = len;
        Ok(())
    }

    /// Copies four-byte pixels into tightly packed rows, preserving the byte
    /// order and skipping source padding. An incomplete read leaves a
    /// partial copy in the destination and returns an I/O error.
    pub fn read_pixels(&self, offset: usize, stride: usize, width: usize, pixels: &mut [u32]) -> io::Result<()> {
        let bad_layout = || io::Error::new(io::ErrorKind::InvalidInput, "pixel rows do not fit the buffer");
        if width == 0 || pixels.is_empty() || !pixels.len().is_multiple_of(width) {
            return Err(bad_layout());
        }
        let rows = pixels.len() / width;
        let row_bytes = width.checked_mul(size_of::<u32>()).ok_or_else(bad_layout)?;
        let end = stride
            .checked_mul(rows - 1)
            .and_then(|distance| offset.checked_add(distance))
            .and_then(|last_row| last_row.checked_add(row_bytes));
        if stride < row_bytes || end.is_none_or(|end| end > self.len) {
            return Err(bad_layout());
        }

        // SAFETY: pixels is an exclusive slice of initialized u32s. Every byte
        // can be written independently because all bit patterns are valid.
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(pixels.as_mut_ptr().cast::<u8>(), mem::size_of_val(pixels)) };
        match &self.storage {
            Storage::File(file) => read_rows(offset, stride, row_bytes, bytes, |at, row| {
                file.read_exact_at(row, at as u64)
            }),
            Storage::Dmabuf(mapping) => mapping.with_cpu_read(|mapping| {
                read_rows(offset, stride, row_bytes, bytes, |at, row| mapping.read_at(at, row))
            }),
        }
    }
}

fn read_rows(
    offset: usize,
    stride: usize,
    row_bytes: usize,
    destination: &mut [u8],
    mut read: impl FnMut(usize, &mut [u8]) -> io::Result<()>,
) -> io::Result<()> {
    if stride == row_bytes {
        return read(offset, destination);
    }
    for (row, destination) in destination.chunks_exact_mut(row_bytes).enumerate() {
        read(offset + row * stride, destination)?;
    }
    Ok(())
}

fn check_file_size(file_size: u64, len: usize) -> io::Result<()> {
    if len == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "an empty pool"));
    }
    if file_size < len as u64 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the file is shorter than the pool",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memfd(len: u64) -> OwnedFd {
        let fd = rustix::fs::memfd_create("fulcrum-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        rustix::fs::ftruncate(&fd, len).unwrap();
        fd
    }

    #[test]
    fn a_buffer_reads_its_rows_at_the_given_offset() {
        let fd = memfd(64);
        rustix::io::pwrite(&fd, &12u32.to_le_bytes(), 5).unwrap();
        rustix::io::pwrite(&fd, &34u32.to_le_bytes(), 13).unwrap();
        let buffer = BufferFile::new(fd.try_clone().unwrap(), 64).unwrap();
        let mut pixels = [0; 2];
        buffer.read_pixels(5, 8, 1, &mut pixels).unwrap();
        assert_eq!(pixels.map(u32::from_le), [12, 34]);
        assert_eq!(rustix::fs::seek(&fd, rustix::fs::SeekFrom::Current(0)).unwrap(), 0);
    }

    #[test]
    fn a_pool_only_grows_to_the_size_of_its_file() {
        let fd = memfd(8);
        let mut buffer = BufferFile::new(fd.try_clone().unwrap(), 4).unwrap();
        assert!(buffer.grow(2).is_err());
        assert!(buffer.grow(12).is_err());
        assert_eq!(buffer.len(), 4);

        rustix::fs::ftruncate(fd, 12).unwrap();
        buffer.grow(12).unwrap();
        assert_eq!(buffer.len(), 12);
    }

    #[test]
    fn an_empty_pool_is_refused() {
        assert!(BufferFile::new(memfd(0), 0).is_err());
    }
}
