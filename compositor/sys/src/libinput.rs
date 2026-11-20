//! libinput: keyboards, mice, and touchpads, read through the seat.
//!
//! Fulcrum uses libinput's path backend. It names each device itself, and
//! libinput opens it by calling back into Fulcrum, which asks the seat for
//! the descriptor. So every input device the compositor reads is one the
//! seat handed it. ADR 0028 allows this library; this binds what Fulcrum
//! uses of it.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    io,
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
    path::Path,
    ptr::NonNull,
};

#[repr(C)]
struct Interface {
    open_restricted: unsafe extern "C" fn(path: *const c_char, flags: c_int, user_data: *mut c_void) -> c_int,
    close_restricted: unsafe extern "C" fn(fd: c_int, user_data: *mut c_void),
}

#[link(name = "input")]
unsafe extern "C" {
    fn libinput_path_create_context(interface: *const Interface, user_data: *mut c_void) -> *mut c_void;
    fn libinput_path_add_device(libinput: *mut c_void, path: *const c_char) -> *mut c_void;
    fn libinput_path_remove_device(device: *mut c_void);
    fn libinput_device_ref(device: *mut c_void) -> *mut c_void;
    fn libinput_device_unref(device: *mut c_void) -> *mut c_void;
    fn libinput_get_fd(libinput: *mut c_void) -> c_int;
    fn libinput_dispatch(libinput: *mut c_void) -> c_int;
    fn libinput_get_event(libinput: *mut c_void) -> *mut c_void;
    fn libinput_unref(libinput: *mut c_void) -> *mut c_void;
    fn libinput_event_get_type(event: *mut c_void) -> c_int;
    fn libinput_event_destroy(event: *mut c_void);
    fn libinput_event_get_device(event: *mut c_void) -> *mut c_void;
    fn libinput_event_get_keyboard_event(event: *mut c_void) -> *mut c_void;
    fn libinput_event_get_pointer_event(event: *mut c_void) -> *mut c_void;
    fn libinput_event_keyboard_get_time(event: *mut c_void) -> u32;
    fn libinput_event_keyboard_get_key(event: *mut c_void) -> u32;
    fn libinput_event_keyboard_get_key_state(event: *mut c_void) -> c_int;
    fn libinput_event_pointer_get_time(event: *mut c_void) -> u32;
    fn libinput_event_pointer_get_dx(event: *mut c_void) -> f64;
    fn libinput_event_pointer_get_dy(event: *mut c_void) -> f64;
    fn libinput_event_pointer_get_absolute_x_transformed(event: *mut c_void, width: u32) -> f64;
    fn libinput_event_pointer_get_absolute_y_transformed(event: *mut c_void, height: u32) -> f64;
    fn libinput_event_pointer_get_button(event: *mut c_void) -> u32;
    fn libinput_event_pointer_get_button_state(event: *mut c_void) -> c_int;
    fn libinput_event_pointer_has_axis(event: *mut c_void, axis: c_int) -> c_int;
    fn libinput_event_pointer_get_scroll_value(event: *mut c_void, axis: c_int) -> f64;
    fn libinput_device_get_sysname(device: *mut c_void) -> *const c_char;
    fn libinput_device_get_name(device: *mut c_void) -> *const c_char;
    fn libinput_device_has_capability(device: *mut c_void, capability: c_int) -> c_int;
}

const EVENT_DEVICE_ADDED: c_int = 1;
const EVENT_DEVICE_REMOVED: c_int = 2;
const EVENT_KEYBOARD_KEY: c_int = 300;
const EVENT_POINTER_MOTION: c_int = 400;
const EVENT_POINTER_MOTION_ABSOLUTE: c_int = 401;
const EVENT_POINTER_BUTTON: c_int = 402;
const EVENT_POINTER_SCROLL_WHEEL: c_int = 404;
const EVENT_POINTER_SCROLL_FINGER: c_int = 405;
const EVENT_POINTER_SCROLL_CONTINUOUS: c_int = 406;
const CAP_KEYBOARD: c_int = 0;
const CAP_POINTER: c_int = 1;
const AXIS_VERTICAL: c_int = 0;
const AXIS_HORIZONTAL: c_int = 1;
/// The scale for absolute positions: a fraction of the device's range, in
/// parts of this.
const ABSOLUTE_SCALE: u32 = 1 << 20;

/// How libinput's devices are opened and closed: through the seat.
pub trait Opener {
    /// Opens a device. An error is an errno value.
    fn open(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32>;
    fn close(&mut self, fd: OwnedFd);
}

/// One thing that happened on an input device.
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    DeviceAdded {
        sysname: String,
        name: String,
        keyboard: bool,
        pointer: bool,
    },
    DeviceRemoved {
        sysname: String,
    },
    /// `key` is the evdev code.
    Key {
        device: String,
        time: u32,
        key: u32,
        pressed: bool,
    },
    Motion {
        time: u32,
        dx: f64,
        dy: f64,
    },
    /// Where an absolute device points, as a fraction of its range on each
    /// axis.
    MotionAbsolute {
        time: u32,
        x: f64,
        y: f64,
    },
    /// `button` is the evdev code.
    Button {
        time: u32,
        button: u32,
        pressed: bool,
    },
    Scroll {
        time: u32,
        horizontal: f64,
        vertical: f64,
    },
}

pub struct Input {
    context: NonNull<c_void>,
    // Boxed twice so that the pointer libinput holds stays put.
    _opener: Box<Box<dyn Opener>>,
    /// The devices libinput has, each with a reference held here, so that a
    /// device libinput removes on its own, as when the seat takes it back,
    /// is not freed before this hears of it.
    devices: Vec<(String, NonNull<c_void>)>,
    /// Events read from libinput that `dispatch` has not returned yet.
    pending: Vec<InputEvent>,
}

unsafe extern "C" fn open_restricted(path: *const c_char, flags: c_int, user_data: *mut c_void) -> c_int {
    // SAFETY: libinput passes back the user data given at creation, the
    // opener, which lives as long as the context; and a valid C string.
    let opener = unsafe { &mut *(user_data as *mut Box<dyn Opener>) };
    let path = unsafe { CStr::from_ptr(path) };
    let path = Path::new(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(
        path.to_bytes(),
    ));
    match opener.open(path, flags) {
        Ok(fd) => fd.into_raw_fd(),
        Err(errno) => -errno.abs(),
    }
}

unsafe extern "C" fn close_restricted(fd: c_int, user_data: *mut c_void) {
    // SAFETY: as above; and libinput gives back a descriptor `open` made and
    // never uses it again.
    let opener = unsafe { &mut *(user_data as *mut Box<dyn Opener>) };
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    opener.close(fd);
}

static INTERFACE: Interface = Interface {
    open_restricted,
    close_restricted,
};

fn text(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    // SAFETY: libinput returns a valid C string that lives with its device.
    unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned()
}

impl Input {
    pub fn new(opener: Box<dyn Opener>) -> io::Result<Self> {
        let mut opener = Box::new(opener);
        let user_data = &mut *opener as *mut Box<dyn Opener> as *mut c_void;
        // SAFETY: the interface is static, and the user data outlives the
        // context: both are dropped together in `Drop`, the context first.
        let context = unsafe { libinput_path_create_context(&INTERFACE, user_data) };
        let context = NonNull::new(context).ok_or_else(|| io::Error::other("no libinput context"))?;
        Ok(Input {
            context,
            _opener: opener,
            devices: Vec::new(),
            pending: Vec::new(),
        })
    }

    /// The descriptor to watch; readable when there are events.
    pub fn fd(&self) -> BorrowedFd<'_> {
        // SAFETY: the context's descriptor lives as long as the context.
        unsafe { BorrowedFd::borrow_raw(libinput_get_fd(self.context.as_ptr()) as RawFd) }
    }

    /// Adds a device by its path, such as `/dev/input/event3`. Returns false
    /// if libinput would not take it.
    pub fn add_device(&mut self, path: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let Ok(path_c) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: a valid context and C string.
        let device = unsafe { libinput_path_add_device(self.context.as_ptr(), path_c.as_ptr()) };
        let Some(device) = NonNull::new(device) else {
            return false;
        };
        // SAFETY: a device libinput just added, kept alive by the reference
        // taken here until `forget` drops it.
        let sysname = unsafe {
            libinput_device_ref(device.as_ptr());
            text(libinput_device_get_sysname(device.as_ptr()))
        };
        self.devices.push((sysname, device));
        true
    }

    /// Removes a device by its sysname, such as `event3`.
    pub fn remove_device(&mut self, sysname: &str) {
        self.pump();
        if let Some(index) = self.devices.iter().position(|(name, _)| name == sysname) {
            let (_, device) = self.devices.remove(index);
            // SAFETY: a device libinput still has, as `pump` just read every
            // removal it made; the reference held here is then dropped.
            unsafe {
                libinput_path_remove_device(device.as_ptr());
                libinput_device_unref(device.as_ptr());
            }
        }
    }

    pub fn has_device(&self, sysname: &str) -> bool {
        self.devices.iter().any(|(name, _)| name == sysname)
    }

    /// Removes and closes every device, as when the session is switched
    /// away; the caller adds them again when it comes back.
    ///
    /// libinput's own suspend is not used: on resume it drops every device
    /// if one of them is gone, as a keyboard unplugged while another virtual
    /// terminal showed.
    pub fn remove_all(&mut self) {
        self.pump();
        for (_, device) in self.devices.drain(..) {
            // SAFETY: as in `remove_device`.
            unsafe {
                libinput_path_remove_device(device.as_ptr());
                libinput_device_unref(device.as_ptr());
            }
        }
    }

    /// Reads what the devices sent, and returns it with what was read before.
    pub fn dispatch(&mut self) -> Vec<InputEvent> {
        self.pump();
        std::mem::take(&mut self.pending)
    }

    /// Reads every event libinput has into `pending`, and forgets each device
    /// libinput removed, whether asked to or on its own.
    fn pump(&mut self) {
        let context = self.context.as_ptr();
        // SAFETY: a valid context.
        unsafe { libinput_dispatch(context) };
        loop {
            // SAFETY: a valid context; the event is destroyed below.
            let event = unsafe { libinput_get_event(context) };
            if event.is_null() {
                break;
            }
            // SAFETY: every accessor below is called on an event of the type
            // it reads, while the event lives.
            unsafe {
                if libinput_event_get_type(event) == EVENT_DEVICE_REMOVED {
                    self.forget(libinput_event_get_device(event));
                }
                if let Some(read) = Self::read(event) {
                    self.pending.push(read);
                }
                libinput_event_destroy(event);
            }
        }
    }

    /// Drops the reference held on a device libinput removed.
    fn forget(&mut self, device: *mut c_void) {
        if let Some(index) = self.devices.iter().position(|(_, kept)| kept.as_ptr() == device) {
            let (_, device) = self.devices.remove(index);
            // SAFETY: the reference taken in `add_device`; the event that
            // names the device holds another until it is destroyed.
            unsafe { libinput_device_unref(device.as_ptr()) };
        }
    }

    /// # Safety
    ///
    /// `event` is a valid event.
    unsafe fn read(event: *mut c_void) -> Option<InputEvent> {
        unsafe {
            let kind = libinput_event_get_type(event);
            let device = libinput_event_get_device(event);
            let sysname = text(libinput_device_get_sysname(device));
            Some(match kind {
                EVENT_DEVICE_ADDED => InputEvent::DeviceAdded {
                    name: text(libinput_device_get_name(device)),
                    keyboard: libinput_device_has_capability(device, CAP_KEYBOARD) != 0,
                    pointer: libinput_device_has_capability(device, CAP_POINTER) != 0,
                    sysname,
                },
                EVENT_DEVICE_REMOVED => InputEvent::DeviceRemoved { sysname },
                EVENT_KEYBOARD_KEY => {
                    let key = libinput_event_get_keyboard_event(event);
                    InputEvent::Key {
                        device: sysname,
                        time: libinput_event_keyboard_get_time(key),
                        key: libinput_event_keyboard_get_key(key),
                        pressed: libinput_event_keyboard_get_key_state(key) == 1,
                    }
                }
                EVENT_POINTER_MOTION => {
                    let pointer = libinput_event_get_pointer_event(event);
                    InputEvent::Motion {
                        time: libinput_event_pointer_get_time(pointer),
                        dx: libinput_event_pointer_get_dx(pointer),
                        dy: libinput_event_pointer_get_dy(pointer),
                    }
                }
                EVENT_POINTER_MOTION_ABSOLUTE => {
                    let pointer = libinput_event_get_pointer_event(event);
                    InputEvent::MotionAbsolute {
                        time: libinput_event_pointer_get_time(pointer),
                        x: libinput_event_pointer_get_absolute_x_transformed(pointer, ABSOLUTE_SCALE)
                            / f64::from(ABSOLUTE_SCALE),
                        y: libinput_event_pointer_get_absolute_y_transformed(pointer, ABSOLUTE_SCALE)
                            / f64::from(ABSOLUTE_SCALE),
                    }
                }
                EVENT_POINTER_BUTTON => {
                    let pointer = libinput_event_get_pointer_event(event);
                    InputEvent::Button {
                        time: libinput_event_pointer_get_time(pointer),
                        button: libinput_event_pointer_get_button(pointer),
                        pressed: libinput_event_pointer_get_button_state(pointer) == 1,
                    }
                }
                EVENT_POINTER_SCROLL_WHEEL | EVENT_POINTER_SCROLL_FINGER | EVENT_POINTER_SCROLL_CONTINUOUS => {
                    let pointer = libinput_event_get_pointer_event(event);
                    let value = |axis| {
                        if libinput_event_pointer_has_axis(pointer, axis) != 0 {
                            libinput_event_pointer_get_scroll_value(pointer, axis)
                        } else {
                            0.0
                        }
                    };
                    InputEvent::Scroll {
                        time: libinput_event_pointer_get_time(pointer),
                        horizontal: value(AXIS_HORIZONTAL),
                        vertical: value(AXIS_VERTICAL),
                    }
                }
                _ => return None,
            })
        }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        // SAFETY: the references held here are dropped, then the context is
        // unreferenced once, and with it its devices; the opener it calls
        // back into is dropped after this.
        unsafe {
            for (_, device) in self.devices.drain(..) {
                libinput_device_unref(device.as_ptr());
            }
            libinput_unref(self.context.as_ptr());
        }
    }
}

/// A descriptor's number, for code that maps descriptors to seat devices.
pub fn raw(fd: &OwnedFd) -> RawFd {
    fd.as_raw_fd()
}
