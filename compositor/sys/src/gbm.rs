//! GBM: buffers in the GPU's memory that the display can scan out and the
//! GPU can draw into.
//!
//! Each buffer keeps its device alive, so a device is destroyed only after
//! its last buffer.

use std::{
    ffi::{c_int, c_uint, c_void},
    io,
    os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd},
    ptr::NonNull,
    rc::Rc,
    sync::OnceLock,
};

use crate::dl::{Library, functions};

/// `gbm_bo_handle`: a union of pointer and integers, returned by value.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union BoHandle {
    ptr: *mut c_void,
    u32_: u32,
    u64_: u64,
}

functions! {
    Functions {
        fn gbm_create_device(fd: c_int) -> *mut c_void;
        fn gbm_device_destroy(device: *mut c_void);
        fn gbm_device_is_format_supported(device: *mut c_void, format: u32, flags: u32) -> c_int;
        fn gbm_bo_create(device: *mut c_void, width: u32, height: u32, format: u32, flags: u32) -> *mut c_void;
        fn gbm_bo_create_with_modifiers2(
            device: *mut c_void,
            width: u32,
            height: u32,
            format: u32,
            modifiers: *const u64,
            count: c_uint,
            flags: u32,
        ) -> *mut c_void;
        fn gbm_bo_destroy(bo: *mut c_void);
        fn gbm_bo_get_modifier(bo: *mut c_void) -> u64;
        fn gbm_bo_get_plane_count(bo: *mut c_void) -> c_int;
        fn gbm_bo_get_offset(bo: *mut c_void, plane: c_int) -> u32;
        fn gbm_bo_get_stride_for_plane(bo: *mut c_void, plane: c_int) -> u32;
        fn gbm_bo_get_handle_for_plane(bo: *mut c_void, plane: c_int) -> BoHandle;
        fn gbm_bo_get_fd_for_plane(bo: *mut c_void, plane: c_int) -> c_int;
    }
}

/// The buffer can be scanned out by the display.
pub const USE_SCANOUT: u32 = 1 << 0;
/// The GPU can draw into the buffer.
pub const USE_RENDERING: u32 = 1 << 2;
/// The buffer is laid out in rows, with no tiling.
pub const USE_LINEAR: u32 = 1 << 4;

/// The modifier that says nothing about the layout.
pub const MODIFIER_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
/// Rows, with no tiling.
pub const MODIFIER_LINEAR: u64 = 0;

fn functions() -> Option<&'static Functions> {
    static FUNCTIONS: OnceLock<Option<Functions>> = OnceLock::new();
    FUNCTIONS
        .get_or_init(|| {
            let library = Library::open(&[c"libgbm.so.1", c"libgbm.so"])?;
            // A library is never closed, so the functions stay valid.
            Functions::load(|name| library.symbol(name)).ok()
        })
        .as_ref()
}

/// Whether libgbm could be loaded.
pub fn available() -> bool {
    functions().is_some()
}

struct DeviceInner {
    raw: NonNull<c_void>,
    functions: &'static Functions,
    fd: OwnedFd,
}

impl Drop for DeviceInner {
    fn drop(&mut self) {
        // Safety: the device came from `gbm_create_device`, and every buffer
        // holds a reference to this, so none is left.
        unsafe { (self.functions.gbm_device_destroy)(self.raw.as_ptr()) };
    }
}

/// A GBM device on a DRM device's descriptor.
#[derive(Clone)]
pub struct Device(Rc<DeviceInner>);

/// One plane of a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
    pub offset: u32,
    pub stride: u32,
    /// The GEM handle on the device the buffer was made on.
    pub handle: u32,
}

/// A buffer GBM made.
pub struct Buffer {
    raw: NonNull<c_void>,
    device: Device,
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

impl Device {
    /// Makes a GBM device on a DRM device, a card or a render node.
    pub fn new(fd: OwnedFd) -> io::Result<Device> {
        let functions = functions().ok_or_else(|| io::Error::other("libgbm could not be loaded"))?;
        let fd_number = std::os::fd::AsRawFd::as_raw_fd(&fd);
        // Safety: the descriptor is open, and outlives the device.
        let raw = unsafe { (functions.gbm_create_device)(fd_number) };
        let raw = NonNull::new(raw).ok_or_else(|| io::Error::other("gbm_create_device failed"))?;
        Ok(Device(Rc::new(DeviceInner { raw, functions, fd })))
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.0.fd.as_fd()
    }

    /// The device, for EGL's GBM platform.
    pub(crate) fn raw(&self) -> *mut c_void {
        self.0.raw.as_ptr()
    }

    pub fn supports(&self, format: u32, flags: u32) -> bool {
        // Safety: the device is live.
        unsafe { (self.0.functions.gbm_device_is_format_supported)(self.raw(), format, flags) != 0 }
    }

    /// Makes a buffer. With modifiers, GBM picks one of them; without, it
    /// picks a layout for `flags`.
    pub fn create_buffer(
        &self,
        width: u32,
        height: u32,
        format: u32,
        modifiers: &[u64],
        flags: u32,
    ) -> io::Result<Buffer> {
        let functions = self.0.functions;
        let raw = if modifiers.is_empty() {
            // Safety: the device is live.
            unsafe { (functions.gbm_bo_create)(self.raw(), width, height, format, flags) }
        } else {
            // Safety: the device is live, and the pointer and count describe
            // the slice.
            unsafe {
                (functions.gbm_bo_create_with_modifiers2)(
                    self.raw(),
                    width,
                    height,
                    format,
                    modifiers.as_ptr(),
                    modifiers.len() as c_uint,
                    flags,
                )
            }
        };
        let raw = NonNull::new(raw).ok_or_else(io::Error::last_os_error)?;
        Ok(Buffer {
            raw,
            device: self.clone(),
            width,
            height,
            format,
        })
    }
}

#[cfg(test)]
impl Device {
    /// A linear buffer for a test, for rendering where the driver makes one.
    /// NVIDIA's GBM makes no buffer that is both linear and for rendering,
    /// and answers `EINVAL`; there the buffer is linear alone.
    pub(crate) fn create_linear_buffer(&self, width: u32, height: u32, format: u32) -> io::Result<Buffer> {
        self.create_buffer(width, height, format, &[], USE_RENDERING | USE_LINEAR)
            .or_else(|_| self.create_buffer(width, height, format, &[], USE_LINEAR))
    }
}

impl Buffer {
    fn functions(&self) -> &'static Functions {
        self.device.0.functions
    }

    pub fn modifier(&self) -> u64 {
        // Safety: the buffer is live.
        unsafe { (self.functions().gbm_bo_get_modifier)(self.raw.as_ptr()) }
    }

    pub fn planes(&self) -> Vec<Plane> {
        let raw = self.raw.as_ptr();
        let functions = self.functions();
        // Safety: the buffer is live, and each plane index is below its count.
        unsafe {
            let count = (functions.gbm_bo_get_plane_count)(raw).max(1);
            (0..count)
                .map(|plane| Plane {
                    offset: (functions.gbm_bo_get_offset)(raw, plane),
                    stride: (functions.gbm_bo_get_stride_for_plane)(raw, plane),
                    handle: (functions.gbm_bo_get_handle_for_plane)(raw, plane).u32_,
                })
                .collect()
        }
    }

    /// A dma-buf descriptor for one plane of the buffer.
    pub fn export(&self, plane: usize) -> io::Result<OwnedFd> {
        // Safety: the buffer is live.
        let fd = unsafe { (self.functions().gbm_bo_get_fd_for_plane)(self.raw.as_ptr(), plane as c_int) };
        if fd < 0 {
            return Err(io::Error::other("gbm_bo_get_fd_for_plane failed"));
        }
        // Safety: GBM returned a new descriptor, which is ours to close.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // Safety: the buffer came from GBM on a device it keeps alive.
        unsafe { (self.functions().gbm_bo_destroy)(self.raw.as_ptr()) };
    }
}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Buffer({}x{}, {:#x})", self.width, self.height, self.format)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A render node, where the machine has one. GBM's tests need a GPU
    /// driver, which CI's runners have not.
    fn render_node() -> Option<OwnedFd> {
        let path = std::fs::read_dir("/dev/dri")
            .ok()?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("renderD"))
            })?;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .ok()
            .map(Into::into)
    }

    #[test]
    fn a_buffer_is_made_with_its_planes_and_exported() {
        // A render node and libgbm are not enough: GBM also needs a driver
        // for the GPU, which a build root may not have.
        let Some(device) = render_node()
            .filter(|_| available())
            .and_then(|fd| Device::new(fd).ok())
        else {
            eprintln!("no render node, libgbm, or GPU driver: skipped");
            return;
        };
        let format = u32::from_le_bytes(*b"XR24");
        let buffer = device.create_linear_buffer(64, 32, format).unwrap();
        let planes = buffer.planes();
        assert_eq!(planes.len(), 1);
        assert!(planes[0].stride >= 64 * 4);
        assert!(buffer.export(0).is_ok());
    }
}
