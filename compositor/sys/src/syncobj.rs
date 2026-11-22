//! DRM timeline synchronization objects, for explicit sync: a client says
//! which point of a timeline its GPU signals when a buffer is ready, and
//! which point the compositor signals when it is done with it.
//!
//! The ioctls work on any DRM device descriptor, a card or a render node.
//! A handle belongs to the descriptor it was imported on.

use std::{
    io,
    os::fd::{AsRawFd, BorrowedFd, OwnedFd},
};

use rustix::ioctl::{Updater, ioctl, opcode};

const DRM: u8 = b'd';
const CAP_SYNCOBJ_TIMELINE: u64 = 0x14;
/// Wait for a point to be signalled, as an eventfd registration's flags say.
const WAIT_SIGNALLED: u32 = 0;

#[repr(C)]
#[derive(Default)]
struct Handle {
    handle: u32,
    flags: u32,
    fd: i32,
    pad: u32,
    point: u64,
}

#[repr(C)]
#[derive(Default)]
struct Destroy {
    handle: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct TimelineArray {
    handles: u64,
    points: u64,
    count_handles: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Default)]
struct Eventfd {
    handle: u32,
    flags: u32,
    point: u64,
    fd: i32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetCap {
    capability: u64,
    value: u64,
}

/// Whether a DRM device has timeline syncobjs.
pub fn supported(device: BorrowedFd<'_>) -> bool {
    let mut cap = GetCap {
        capability: CAP_SYNCOBJ_TIMELINE,
        value: 0,
    };
    // SAFETY: the opcode matches the struct, which the kernel fills.
    let result = unsafe {
        ioctl(
            device,
            Updater::<{ opcode::read_write::<GetCap>(DRM, 0x0c) }, _>::new(&mut cap),
        )
    };
    result.is_ok() && cap.value != 0
}

/// A timeline imported from a client's descriptor, on one DRM device.
#[derive(Debug)]
pub struct Timeline {
    device: OwnedFd,
    handle: u32,
}

impl Timeline {
    /// Imports a timeline syncobj from its descriptor. A descriptor that is
    /// not one is an error.
    pub fn import(device: BorrowedFd<'_>, fd: BorrowedFd<'_>) -> io::Result<Timeline> {
        let mut handle = Handle {
            fd: fd.as_raw_fd(),
            ..Default::default()
        };
        // SAFETY: the opcode matches the struct, which the kernel reads and
        // fills; the descriptor is live for the call.
        unsafe {
            ioctl(
                device,
                Updater::<{ opcode::read_write::<Handle>(DRM, 0xc2) }, _>::new(&mut handle),
            )?;
        }
        Ok(Timeline {
            device: device.try_clone_to_owned()?,
            handle: handle.handle,
        })
    }

    /// A timeline with no syncobj behind it, for tests of who holds a
    /// timeline on a machine with no DRM device. Every call on it fails.
    #[doc(hidden)]
    pub fn unbacked(device: OwnedFd) -> Timeline {
        Timeline { device, handle: 0 }
    }

    /// The last point signalled.
    pub fn signalled(&self) -> io::Result<u64> {
        let handles = [self.handle];
        let mut points = [0u64];
        let mut array = TimelineArray {
            handles: handles.as_ptr() as u64,
            points: points.as_mut_ptr() as u64,
            count_handles: 1,
            flags: 0,
        };
        // SAFETY: the opcode matches the struct; the arrays it points at
        // hold one entry each and outlive the call.
        unsafe {
            ioctl(
                &self.device,
                Updater::<{ opcode::read_write::<TimelineArray>(DRM, 0xcb) }, _>::new(&mut array),
            )?;
        }
        Ok(points[0])
    }

    /// Signals a point, as when the compositor is done with a buffer.
    pub fn signal(&self, point: u64) -> io::Result<()> {
        let handles = [self.handle];
        let points = [point];
        let mut array = TimelineArray {
            handles: handles.as_ptr() as u64,
            points: points.as_ptr() as u64,
            count_handles: 1,
            flags: 0,
        };
        // SAFETY: the opcode matches the struct; the arrays it points at
        // hold one entry each and outlive the call.
        unsafe {
            ioctl(
                &self.device,
                Updater::<{ opcode::read_write::<TimelineArray>(DRM, 0xcd) }, _>::new(&mut array),
            )?;
        }
        Ok(())
    }

    /// An eventfd that becomes readable once `point` is signalled, so the
    /// compositor waits for it without blocking.
    pub fn eventfd(&self, point: u64) -> io::Result<OwnedFd> {
        let event = rustix::event::eventfd(
            0,
            rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
        )?;
        let mut register = Eventfd {
            handle: self.handle,
            flags: WAIT_SIGNALLED,
            point,
            fd: event.as_raw_fd(),
            pad: 0,
        };
        // SAFETY: the opcode matches the struct, which the kernel reads; the
        // eventfd is live for the call, and the kernel takes its own
        // reference.
        unsafe {
            ioctl(
                &self.device,
                Updater::<{ opcode::read_write::<Eventfd>(DRM, 0xcf) }, _>::new(&mut register),
            )?;
        }
        Ok(event)
    }
}

impl Drop for Timeline {
    fn drop(&mut self) {
        let mut destroy = Destroy {
            handle: self.handle,
            pad: 0,
        };
        // SAFETY: the handle is this timeline's, and is not used again.
        let _ = unsafe {
            ioctl(
                &self.device,
                Updater::<{ opcode::read_write::<Destroy>(DRM, 0xc0) }, _>::new(&mut destroy),
            )
        };
    }
}

/// Makes a timeline syncobj and exports it, as a client does, for tests.
#[doc(hidden)]
pub fn create_exported(device: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    #[repr(C)]
    #[derive(Default)]
    struct Create {
        handle: u32,
        flags: u32,
    }
    let mut create = Create::default();
    // SAFETY: the opcode matches the struct, which the kernel fills.
    unsafe {
        ioctl(
            device,
            Updater::<{ opcode::read_write::<Create>(DRM, 0xbf) }, _>::new(&mut create),
        )?;
    }
    let mut handle = Handle {
        handle: create.handle,
        flags: 0,
        ..Default::default()
    };
    // SAFETY: the opcode matches the struct, which the kernel fills.
    unsafe {
        ioctl(
            device,
            Updater::<{ opcode::read_write::<Handle>(DRM, 0xc1) }, _>::new(&mut handle),
        )?;
    }
    let mut destroy = Destroy {
        handle: create.handle,
        pad: 0,
    };
    // SAFETY: the handle was made above; the exported descriptor keeps the
    // syncobj alive.
    let _ = unsafe {
        ioctl(
            device,
            Updater::<{ opcode::read_write::<Destroy>(DRM, 0xc0) }, _>::new(&mut destroy),
        )
    };
    // SAFETY: the kernel made a new descriptor, which is ours to close.
    Ok(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(handle.fd) })
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use super::*;

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
    fn a_point_signalled_is_seen_and_wakes_its_eventfd() {
        let Some(device) = render_node().filter(|device| supported(device.as_fd())) else {
            eprintln!("no render node with timeline syncobjs: skipped");
            return;
        };
        let exported = create_exported(device.as_fd()).unwrap();
        let timeline = Timeline::import(device.as_fd(), exported.as_fd()).unwrap();
        let event = timeline.eventfd(3).unwrap();
        let mut count = [0u8; 8];
        assert!(rustix::io::read(&event, &mut count).is_err(), "woke before the point");

        timeline.signal(3).unwrap();

        assert_eq!(timeline.signalled().unwrap(), 3);
        assert!(rustix::io::read(&event, &mut count).is_ok(), "the eventfd did not wake");
    }

    #[test]
    fn a_descriptor_that_is_not_a_syncobj_is_refused() {
        let Some(device) = render_node() else {
            eprintln!("no render node: skipped");
            return;
        };
        let memfd = rustix::fs::memfd_create("not-a-syncobj", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        assert!(Timeline::import(device.as_fd(), memfd.as_fd()).is_err());
    }
}
