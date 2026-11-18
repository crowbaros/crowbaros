//! The kernel's DRM interface: kernel mode setting with atomic commits, and
//! dumb buffers the CPU draws into.
//!
//! Fulcrum talks to the kernel directly, with the structures from
//! `<drm/drm.h>` and `<drm/drm_mode.h>`, and links no libdrm. This is the
//! subset a compositor needs to light displays: the device's resources, its
//! connectors and their modes, CRTCs and planes and their properties, mode
//! blobs, framebuffers, and atomic commits whose page flips it hears about.

use std::{
    ffi::c_void,
    io,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    ptr::NonNull,
};

use rustix::ioctl::{Updater, ioctl, opcode};

const DRM: u8 = b'd';

#[repr(C)]
#[derive(Default)]
struct GetCap {
    capability: u64,
    value: u64,
}

#[repr(C)]
#[derive(Default)]
struct SetClientCap {
    capability: u64,
    value: u64,
}

#[repr(C)]
#[derive(Default)]
struct CardRes {
    fb_id_ptr: u64,
    crtc_id_ptr: u64,
    connector_id_ptr: u64,
    encoder_id_ptr: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

/// One display mode, as the kernel describes it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModeInfo {
    pub clock: u32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub hskew: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub vscan: u16,
    pub vrefresh: u32,
    pub flags: u32,
    pub kind: u32,
    pub name: [u8; 32],
}

impl ModeInfo {
    /// Whether the monitor prefers this mode.
    pub fn preferred(&self) -> bool {
        self.kind & (1 << 3) != 0
    }

    /// The refresh rate in millihertz, from the pixel clock.
    pub fn refresh_millihertz(&self) -> i32 {
        let total = u64::from(self.htotal) * u64::from(self.vtotal) * u64::from(self.vscan.max(1));
        if total == 0 {
            return (self.vrefresh * 1000) as i32;
        }
        ((u64::from(self.clock) * 1_000_000 + total / 2) / total) as i32
    }
}

#[repr(C)]
#[derive(Default)]
struct GetEncoder {
    encoder_id: u32,
    encoder_type: u32,
    crtc_id: u32,
    possible_crtcs: u32,
    possible_clones: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetConnector {
    encoders_ptr: u64,
    modes_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    count_modes: u32,
    count_props: u32,
    count_encoders: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    pad: u32,
}

#[repr(C)]
struct GetProperty {
    values_ptr: u64,
    enum_blob_ptr: u64,
    prop_id: u32,
    flags: u32,
    name: [u8; 32],
    count_values: u32,
    count_enum_blobs: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetPlaneRes {
    plane_id_ptr: u64,
    count_planes: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetPlane {
    plane_id: u32,
    crtc_id: u32,
    fb_id: u32,
    possible_crtcs: u32,
    gamma_size: u32,
    count_format_types: u32,
    format_type_ptr: u64,
}

#[repr(C)]
#[derive(Default)]
struct ObjGetProperties {
    props_ptr: u64,
    prop_values_ptr: u64,
    count_props: u32,
    obj_id: u32,
    obj_type: u32,
}

#[repr(C)]
#[derive(Default)]
struct Atomic {
    flags: u32,
    count_objs: u32,
    objs_ptr: u64,
    count_props_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    reserved: u64,
    user_data: u64,
}

#[repr(C)]
#[derive(Default)]
struct CreateBlob {
    data: u64,
    length: u32,
    blob_id: u32,
}

#[repr(C)]
#[derive(Default)]
struct FbCmd2 {
    fb_id: u32,
    width: u32,
    height: u32,
    pixel_format: u32,
    flags: u32,
    handles: [u32; 4],
    pitches: [u32; 4],
    offsets: [u32; 4],
    modifier: [u64; 4],
}

#[repr(C)]
#[derive(Default)]
struct CreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

#[repr(C)]
#[derive(Default)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

#[repr(C)]
#[derive(Default)]
struct DestroyDumb {
    handle: u32,
}

/// The object types properties belong to.
pub const OBJECT_CRTC: u32 = 0xcccc_cccc;
pub const OBJECT_CONNECTOR: u32 = 0xc0c0_c0c0;
pub const OBJECT_PLANE: u32 = 0xeeee_eeee;

const CAP_DUMB_BUFFER: u64 = 0x1;
const CAP_ADDFB2_MODIFIERS: u64 = 0x10;
/// `DRM_MODE_FB_MODIFIERS`: the framebuffer's modifier is given.
const FB_MODIFIERS: u32 = 1 << 1;
const CLIENT_CAP_UNIVERSAL_PLANES: u64 = 2;
const CLIENT_CAP_ATOMIC: u64 = 3;

const PAGE_FLIP_EVENT: u32 = 0x01;
const ATOMIC_TEST_ONLY: u32 = 0x0100;
const ATOMIC_NONBLOCK: u32 = 0x0200;
const ATOMIC_ALLOW_MODESET: u32 = 0x0400;
const EVENT_FLIP_COMPLETE: u32 = 0x02;

/// XRGB8888, as a fourcc.
pub const FORMAT_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");

/// What a connector says about itself.
#[derive(Clone, Debug)]
pub struct Connector {
    pub id: u32,
    /// The kernel's name for its kind, such as `Virtual` or `HDMI-A`.
    pub kind: &'static str,
    pub kind_id: u32,
    pub connected: bool,
    pub modes: Vec<ModeInfo>,
    pub encoders: Vec<u32>,
    pub mm_size: (u32, u32),
}

impl Connector {
    /// The name the kernel's connector has on the command line and in logs,
    /// such as `Virtual-1`.
    pub fn name(&self) -> String {
        format!("{}-{}", self.kind, self.kind_id)
    }
}

fn connector_kind(kind: u32) -> &'static str {
    match kind {
        1 => "VGA",
        2 => "DVI-I",
        3 => "DVI-D",
        4 => "DVI-A",
        5 => "Composite",
        6 => "SVIDEO",
        7 => "LVDS",
        8 => "Component",
        9 => "DIN",
        10 => "DP",
        11 => "HDMI-A",
        12 => "HDMI-B",
        13 => "TV",
        14 => "eDP",
        15 => "Virtual",
        16 => "DSI",
        17 => "DPI",
        18 => "Writeback",
        19 => "SPI",
        20 => "USB",
        _ => "Unknown",
    }
}

/// A plane, and the CRTCs it can show on.
#[derive(Clone, Copy, Debug)]
pub struct Plane {
    pub id: u32,
    pub possible_crtcs: u32,
}

/// A DRM device the compositor has opened, through the seat.
pub struct Device {
    fd: OwnedFd,
}

/// A buffer in memory that the CPU draws into and the display scans out.
pub struct DumbBuffer {
    handle: u32,
    pub framebuffer: u32,
    pub width: u32,
    pub height: u32,
    /// Bytes from one row to the next.
    pub pitch: u32,
    map: NonNull<u8>,
    size: usize,
}

/// One flip the kernel finished: the CRTC it was on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flip {
    pub crtc: u32,
    /// When the display showed the frame, on the monotonic clock.
    pub seconds: u32,
    pub microseconds: u32,
    /// The display's vertical blank count at that moment.
    pub sequence: u32,
}

/// One change in an atomic commit: an object, a property, and its value.
pub type Change = (u32, u32, u64);

fn list<T: Default + Clone>(count: u32) -> Vec<T> {
    vec![T::default(); count as usize]
}

impl Device {
    /// Takes a DRM device's descriptor, and asks for atomic mode setting and
    /// every plane.
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        let device = Device { fd };
        device.set_client_cap(CLIENT_CAP_UNIVERSAL_PLANES, 1)?;
        device.set_client_cap(CLIENT_CAP_ATOMIC, 1)?;
        if device.cap(CAP_DUMB_BUFFER)? == 0 {
            return Err(io::Error::other("the device has no dumb buffers"));
        }
        Ok(device)
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    fn cap(&self, capability: u64) -> io::Result<u64> {
        let mut cap = GetCap { capability, value: 0 };
        // SAFETY: the opcode matches the struct, which the kernel fills.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<GetCap>(DRM, 0x0c) }, _>::new(&mut cap),
            )?
        };
        Ok(cap.value)
    }

    fn set_client_cap(&self, capability: u64, value: u64) -> io::Result<()> {
        let mut cap = SetClientCap { capability, value };
        // SAFETY: the opcode matches the struct, which the kernel reads.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::write::<SetClientCap>(DRM, 0x0d) }, _>::new(&mut cap),
            )?;
        }
        Ok(())
    }

    /// The device's CRTCs and connectors.
    pub fn resources(&self) -> io::Result<(Vec<u32>, Vec<u32>)> {
        let mut res = CardRes::default();
        // SAFETY: a first call with no arrays asks only for the counts.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<CardRes>(DRM, 0xa0) }, _>::new(&mut res),
            )?
        };
        let mut crtcs: Vec<u32> = list(res.count_crtcs);
        let mut connectors: Vec<u32> = list(res.count_connectors);
        let mut again = CardRes {
            crtc_id_ptr: crtcs.as_mut_ptr() as u64,
            connector_id_ptr: connectors.as_mut_ptr() as u64,
            count_crtcs: res.count_crtcs,
            count_connectors: res.count_connectors,
            ..Default::default()
        };
        // SAFETY: the arrays hold as many entries as the counts say.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<CardRes>(DRM, 0xa0) }, _>::new(&mut again),
            )?
        };
        crtcs.truncate(again.count_crtcs.min(res.count_crtcs) as usize);
        connectors.truncate(again.count_connectors.min(res.count_connectors) as usize);
        Ok((crtcs, connectors))
    }

    pub fn connector(&self, id: u32) -> io::Result<Connector> {
        let mut first = GetConnector {
            connector_id: id,
            ..Default::default()
        };
        // SAFETY: a first call with no arrays asks only for the counts.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<GetConnector>(DRM, 0xa7) }, _>::new(&mut first),
            )?;
        }
        let mut modes: Vec<ModeInfo> = list(first.count_modes);
        let mut encoders: Vec<u32> = list(first.count_encoders);
        let mut again = GetConnector {
            connector_id: id,
            modes_ptr: modes.as_mut_ptr() as u64,
            encoders_ptr: encoders.as_mut_ptr() as u64,
            count_modes: first.count_modes,
            count_encoders: first.count_encoders,
            ..Default::default()
        };
        // SAFETY: the arrays hold as many entries as the counts say.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<GetConnector>(DRM, 0xa7) }, _>::new(&mut again),
            )?;
        }
        modes.truncate(again.count_modes.min(first.count_modes) as usize);
        encoders.truncate(again.count_encoders.min(first.count_encoders) as usize);
        Ok(Connector {
            id,
            kind: connector_kind(again.connector_type),
            kind_id: again.connector_type_id,
            connected: again.connection == 1,
            modes,
            encoders,
            mm_size: (again.mm_width, again.mm_height),
        })
    }

    /// The CRTCs an encoder can drive, as a bit mask over the device's CRTC
    /// list.
    pub fn encoder_crtcs(&self, id: u32) -> io::Result<u32> {
        let mut encoder = GetEncoder {
            encoder_id: id,
            ..Default::default()
        };
        // SAFETY: the opcode matches the struct, which the kernel fills.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<GetEncoder>(DRM, 0xa6) }, _>::new(&mut encoder),
            )?;
        }
        Ok(encoder.possible_crtcs)
    }

    pub fn planes(&self) -> io::Result<Vec<Plane>> {
        let mut res = GetPlaneRes::default();
        // SAFETY: a first call with no array asks only for the count.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<GetPlaneRes>(DRM, 0xb5) }, _>::new(&mut res),
            )?;
        }
        let mut ids: Vec<u32> = list(res.count_planes);
        let mut again = GetPlaneRes {
            plane_id_ptr: ids.as_mut_ptr() as u64,
            count_planes: res.count_planes,
        };
        // SAFETY: the array holds as many entries as the count says.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<GetPlaneRes>(DRM, 0xb5) }, _>::new(&mut again),
            )?;
        }
        ids.truncate(again.count_planes.min(res.count_planes) as usize);
        let mut planes = Vec::with_capacity(ids.len());
        for id in ids {
            let mut plane = GetPlane {
                plane_id: id,
                ..Default::default()
            };
            // SAFETY: with no format array the kernel fills only the struct.
            unsafe {
                ioctl(
                    &self.fd,
                    Updater::<{ opcode::read_write::<GetPlane>(DRM, 0xb6) }, _>::new(&mut plane),
                )?;
            }
            planes.push(Plane {
                id,
                possible_crtcs: plane.possible_crtcs,
            });
        }
        Ok(planes)
    }

    /// An object's properties: each one's name, ID, and current value.
    pub fn properties(&self, object: u32, kind: u32) -> io::Result<Vec<(String, u32, u64)>> {
        let mut first = ObjGetProperties {
            obj_id: object,
            obj_type: kind,
            ..Default::default()
        };
        // SAFETY: a first call with no arrays asks only for the count.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<ObjGetProperties>(DRM, 0xb9) }, _>::new(&mut first),
            )?;
        }
        let mut ids: Vec<u32> = list(first.count_props);
        let mut values: Vec<u64> = list(first.count_props);
        let mut again = ObjGetProperties {
            props_ptr: ids.as_mut_ptr() as u64,
            prop_values_ptr: values.as_mut_ptr() as u64,
            count_props: first.count_props,
            obj_id: object,
            obj_type: kind,
        };
        // SAFETY: both arrays hold as many entries as the count says.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<ObjGetProperties>(DRM, 0xb9) }, _>::new(&mut again),
            )?;
        }
        let count = again.count_props.min(first.count_props) as usize;
        let mut found = Vec::with_capacity(count);
        for (id, value) in ids.into_iter().zip(values).take(count) {
            let mut property = GetProperty {
                values_ptr: 0,
                enum_blob_ptr: 0,
                prop_id: id,
                flags: 0,
                name: [0; 32],
                count_values: 0,
                count_enum_blobs: 0,
            };
            // SAFETY: with no arrays the kernel fills only the struct.
            unsafe {
                ioctl(
                    &self.fd,
                    Updater::<{ opcode::read_write::<GetProperty>(DRM, 0xaa) }, _>::new(&mut property),
                )?;
            }
            let end = property.name.iter().position(|byte| *byte == 0).unwrap_or(32);
            found.push((String::from_utf8_lossy(&property.name[..end]).into_owned(), id, value));
        }
        Ok(found)
    }

    /// A blob holding a mode, for a CRTC's `MODE_ID`.
    pub fn mode_blob(&self, mode: &ModeInfo) -> io::Result<u32> {
        let mut blob = CreateBlob {
            data: mode as *const ModeInfo as u64,
            length: size_of::<ModeInfo>() as u32,
            blob_id: 0,
        };
        // SAFETY: the data pointer and length name the mode, which lives
        // through the call.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<CreateBlob>(DRM, 0xbd) }, _>::new(&mut blob),
            )?;
        }
        Ok(blob.blob_id)
    }

    /// A dumb buffer of this size, its framebuffer, and its memory mapped.
    pub fn dumb_buffer(&self, width: u32, height: u32) -> io::Result<DumbBuffer> {
        let mut create = CreateDumb {
            width,
            height,
            bpp: 32,
            ..Default::default()
        };
        // SAFETY: the opcode matches the struct, which the kernel fills.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<CreateDumb>(DRM, 0xb2) }, _>::new(&mut create),
            )?;
        }
        let destroy = |device: &Device| {
            let mut destroy = DestroyDumb { handle: create.handle };
            // SAFETY: the handle was made above and is not used again.
            let _ = unsafe {
                ioctl(
                    &device.fd,
                    Updater::<{ opcode::read_write::<DestroyDumb>(DRM, 0xb4) }, _>::new(&mut destroy),
                )
            };
        };
        let mut command = FbCmd2 {
            width,
            height,
            pixel_format: FORMAT_XRGB8888,
            ..Default::default()
        };
        command.handles[0] = create.handle;
        command.pitches[0] = create.pitch;
        // SAFETY: the opcode matches the struct; the handle is the buffer's.
        if let Err(error) = unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<FbCmd2>(DRM, 0xb8) }, _>::new(&mut command),
            )
        } {
            destroy(self);
            return Err(error.into());
        }
        let mut map = MapDumb {
            handle: create.handle,
            ..Default::default()
        };
        // SAFETY: the opcode matches the struct, which the kernel fills.
        let mapped = unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<MapDumb>(DRM, 0xb3) }, _>::new(&mut map),
            )
        }
        .and_then(|()| {
            // SAFETY: a new shared mapping of the buffer, at the offset the
            // kernel gave, aliasing no Rust object.
            unsafe {
                rustix::mm::mmap(
                    std::ptr::null_mut(),
                    create.size as usize,
                    rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE,
                    rustix::mm::MapFlags::SHARED,
                    &self.fd,
                    map.offset,
                )
            }
        });
        let pointer = match mapped {
            Ok(pointer) => pointer,
            Err(error) => {
                self.remove_framebuffer(command.fb_id);
                destroy(self);
                return Err(error.into());
            }
        };
        Ok(DumbBuffer {
            handle: create.handle,
            framebuffer: command.fb_id,
            width,
            height,
            pitch: create.pitch,
            map: NonNull::new(pointer.cast()).ok_or_else(|| io::Error::other("mmap returned null"))?,
            size: create.size as usize,
        })
    }

    /// Whether a framebuffer can be made with a modifier: a GPU buffer whose
    /// layout is tiled.
    pub fn supports_modifiers(&self) -> bool {
        self.cap(CAP_ADDFB2_MODIFIERS).is_ok_and(|value| value != 0)
    }

    /// Makes a framebuffer of a buffer on this device, as a GBM buffer: each
    /// plane's GEM handle, pitch, and offset. `modifier` is given when the
    /// device takes modifiers.
    pub fn add_framebuffer(
        &self,
        width: u32,
        height: u32,
        format: u32,
        planes: &[(u32, u32, u32)],
        modifier: Option<u64>,
    ) -> io::Result<u32> {
        if planes.is_empty() || planes.len() > 4 {
            return Err(io::Error::other("a framebuffer has one to four planes"));
        }
        let mut command = FbCmd2 {
            width,
            height,
            pixel_format: format,
            flags: if modifier.is_some() { FB_MODIFIERS } else { 0 },
            ..Default::default()
        };
        for (index, (handle, pitch, offset)) in planes.iter().enumerate() {
            command.handles[index] = *handle;
            command.pitches[index] = *pitch;
            command.offsets[index] = *offset;
            command.modifier[index] = modifier.unwrap_or(0);
        }
        // SAFETY: the opcode matches the struct, which the kernel reads and
        // fills; the handles are the caller's buffer's.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<FbCmd2>(DRM, 0xb8) }, _>::new(&mut command),
            )?;
        }
        Ok(command.fb_id)
    }

    /// Removes a framebuffer made with [`Device::add_framebuffer`].
    pub fn remove_framebuffer(&self, framebuffer: u32) {
        let mut id = framebuffer;
        // SAFETY: the opcode matches an unsigned int, which the kernel reads.
        let _ = unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<u32>(DRM, 0xaf) }, _>::new(&mut id),
            )
        };
    }

    /// Frees a dumb buffer and its framebuffer.
    pub fn destroy(&self, buffer: DumbBuffer) {
        self.remove_framebuffer(buffer.framebuffer);
        let mut destroy = DestroyDumb { handle: buffer.handle };
        // SAFETY: the handle is the buffer's, and the buffer is consumed.
        let _ = unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<DestroyDumb>(DRM, 0xb4) }, _>::new(&mut destroy),
            )
        };
        drop(buffer);
    }

    /// Commits changes at once. `modeset` allows a change of mode; `test`
    /// only asks whether the kernel would take it. Without `test`, the
    /// commit does not block, and the kernel reports its flip as an event.
    pub fn commit(&self, changes: &[Change], modeset: bool, test: bool) -> io::Result<()> {
        // The kernel wants the changes grouped by object.
        let mut objects: Vec<u32> = Vec::new();
        let mut counts: Vec<u32> = Vec::new();
        let mut properties: Vec<u32> = Vec::new();
        let mut values: Vec<u64> = Vec::new();
        let mut sorted = changes.to_vec();
        sorted.sort_by_key(|(object, _, _)| *object);
        for (object, property, value) in sorted {
            if objects.last() != Some(&object) {
                objects.push(object);
                counts.push(0);
            }
            *counts.last_mut().expect("pushed above") += 1;
            properties.push(property);
            values.push(value);
        }
        let mut flags = if test {
            ATOMIC_TEST_ONLY
        } else {
            ATOMIC_NONBLOCK | PAGE_FLIP_EVENT
        };
        if modeset {
            flags |= ATOMIC_ALLOW_MODESET;
        }
        let mut atomic = Atomic {
            flags,
            count_objs: objects.len() as u32,
            objs_ptr: objects.as_ptr() as u64,
            count_props_ptr: counts.as_ptr() as u64,
            props_ptr: properties.as_ptr() as u64,
            prop_values_ptr: values.as_ptr() as u64,
            reserved: 0,
            user_data: 0,
        };
        // SAFETY: the four arrays live through the call, and their lengths
        // agree with the counts.
        unsafe {
            ioctl(
                &self.fd,
                Updater::<{ opcode::read_write::<Atomic>(DRM, 0xbc) }, _>::new(&mut atomic),
            )?
        };
        Ok(())
    }

    /// The flips the kernel has finished since the last read. The device's
    /// descriptor is readable when there are some.
    pub fn read_flips(&self) -> io::Result<Vec<Flip>> {
        let mut buffer = [0u8; 1024];
        let read = match rustix::io::read(&self.fd, &mut buffer) {
            Ok(read) => read,
            Err(rustix::io::Errno::AGAIN) => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut flips = Vec::new();
        let mut at = 0;
        // Each event: type, length, then for a flip the user data, the time,
        // the sequence, and the CRTC.
        while at + 8 <= read {
            let kind = u32::from_ne_bytes(buffer[at..at + 4].try_into().expect("four bytes"));
            let length = u32::from_ne_bytes(buffer[at + 4..at + 8].try_into().expect("four bytes")) as usize;
            if length < 8 || at + length > read {
                break;
            }
            if kind == EVENT_FLIP_COMPLETE && length >= 32 {
                let word = |offset: usize| {
                    u32::from_ne_bytes(buffer[at + offset..at + offset + 4].try_into().expect("four bytes"))
                };
                flips.push(Flip {
                    seconds: word(16),
                    microseconds: word(20),
                    sequence: word(24),
                    crtc: word(28),
                });
            }
            at += length;
        }
        Ok(flips)
    }
}

impl DumbBuffer {
    /// Writes rows of XRGB8888 pixels into the buffer, row by row, `width`
    /// pixels a row, clipped to the buffer.
    pub fn write(&mut self, pixels: &[u32], width: u32) {
        let rows = (pixels.len() as u32 / width.max(1)).min(self.height);
        let copied = width.min(self.width) as usize;
        for row in 0..rows as usize {
            let source = &pixels[row * width as usize..row * width as usize + copied];
            let at = row * self.pitch as usize;
            // SAFETY: the row starts inside the mapping, and `copied` pixels
            // of four bytes fit in a pitch, which the kernel made at least a
            // row of the buffer's width.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    source.as_ptr().cast::<u8>(),
                    self.map.as_ptr().add(at),
                    (copied * 4).min(self.size - at),
                );
            }
        }
    }
}

impl Drop for DumbBuffer {
    fn drop(&mut self) {
        // SAFETY: the mapping was made with this size, and nothing refers to
        // it after this.
        unsafe {
            let _ = rustix::mm::munmap(self.map.as_ptr().cast::<c_void>(), self.size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_structures_are_the_size_the_kernel_expects() {
        assert_eq!(size_of::<ModeInfo>(), 68);
        assert_eq!(size_of::<CardRes>(), 64);
        assert_eq!(size_of::<GetConnector>(), 80);
        assert_eq!(size_of::<GetProperty>(), 64);
        assert_eq!(size_of::<GetPlane>(), 32);
        assert_eq!(size_of::<ObjGetProperties>(), 32);
        assert_eq!(size_of::<Atomic>(), 56);
        assert_eq!(size_of::<FbCmd2>(), 104);
        assert_eq!(size_of::<CreateDumb>(), 32);
        assert_eq!(size_of::<MapDumb>(), 16);
    }

    #[test]
    fn a_mode_knows_its_refresh_rate() {
        let mode = ModeInfo {
            clock: 148_500,
            htotal: 2200,
            vtotal: 1125,
            vrefresh: 60,
            ..Default::default()
        };
        assert_eq!(mode.refresh_millihertz(), 60_000);
    }

    #[test]
    fn a_connector_is_named_as_the_kernel_names_it() {
        let connector = Connector {
            id: 1,
            kind: connector_kind(15),
            kind_id: 1,
            connected: true,
            modes: Vec::new(),
            encoders: Vec::new(),
            mm_size: (0, 0),
        };
        assert_eq!(connector.name(), "Virtual-1");
    }
}
