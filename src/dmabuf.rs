//! linux-dmabuf: buffers a client hands over as dma-buf descriptors, as a
//! client of a GPU does. Version 4: what the compositor takes is told in
//! feedback, as a table of formats and modifiers in sealed shared memory.
//!
//! Where the GLES renderer draws the outputs, it imports a client's dma-buf
//! as a texture when the buffer is made, in whatever format and layout EGL
//! takes, and draws from it where it is. A surface that shows it holds the
//! buffer until other content replaces it (see `shm.rs`). Only a real
//! dma-buf goes to the import (see `fulcrum_sys::dmabuf::is_dmabuf`).
//!
//! Where the GPU has not imported it, the compositor takes single-plane
//! `XR24` and `AR24` buffers laid out in rows (the `LINEAR` modifier), maps
//! them, and copies their pixels at each commit as it copies shared memory,
//! through the same guarded mapping. Every client of a GPU can make such a
//! buffer.
//!
//! The table (ADR 0020's, with ADR 0023's) decides who is offered it: the
//! shell, and a compartment whose needs include the GPU.

use std::{
    borrow::Cow,
    collections::HashMap,
    io::{Seek, SeekFrom},
    os::fd::{AsFd, OwnedFd},
    rc::Rc,
};

use fulcrum_sys::{
    gles::{self, DmabufPlane, Gles},
    shm::Mapping,
};
use fulcrum_wire::{
    NewId,
    protocol::{
        linux_dmabuf_v1::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1, zwp_linux_dmabuf_v1},
        wayland::wl_buffer,
    },
};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    globals::GlobalKind,
    image::format,
    shm::{Buffer, Source},
};

/// The `zwp_linux_dmabuf_v1` version Fulcrum offers.
pub const DMABUF_VERSION: u32 = 4;

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

pub const FOURCC_XRGB8888: u32 = fourcc(b"XR24");
pub const FOURCC_ARGB8888: u32 = fourcc(b"AR24");
/// Rows, with no tiling.
pub const MODIFIER_LINEAR: u64 = 0;

/// What the compositor takes with no GPU's import, first in the format
/// table. The CPU copies them.
pub const FORMATS: [(u32, u64); 2] = [(FOURCC_XRGB8888, MODIFIER_LINEAR), (FOURCC_ARGB8888, MODIFIER_LINEAR)];

/// The most entries the format table holds: a tranche names them by 16-bit
/// index.
const MAX_FORMATS: usize = 1 << 16;

pub(crate) type ParamsId = u64;

/// One plane a client added.
pub(crate) struct Plane {
    pub fd: OwnedFd,
    offset: u32,
    stride: u32,
    modifier: u64,
}

/// A `zwp_linux_buffer_params_v1`: planes on their way to a buffer.
#[derive(Default)]
pub(crate) struct Params {
    pub planes: [Option<Plane>; 4],
    used: bool,
}

#[derive(Default)]
pub(crate) struct Dmabufs {
    pub params: HashMap<ParamsId, Params>,
    next: ParamsId,
    /// The format table, sealed, and its size.
    table: Option<(OwnedFd, u32)>,
    /// What the table lists, in its order.
    formats: Vec<(u32, u64)>,
    /// The `dev_t` of the device clients should allocate on.
    device: u64,
    /// The GPU that imports dma-bufs: the renderer's.
    gpu: Option<Gles>,
    /// For the tests, whose machines have no dma-bufs to make: memfds go to
    /// the GPU's import too.
    import_memfds: bool,
}

/// A buffer made from params: where its pixels are, and the first plane's
/// layout, which the CPU's copy reads.
struct Made {
    buffer: crate::shm::BufferId,
    source: Source,
    offset: u32,
    stride: u32,
}

impl State {
    /// Offers linux-dmabuf, with `device` as the `dev_t` of the DRM device
    /// clients should allocate on, and buffers the CPU copies. The table still
    /// decides who sees it. Once offered, a second offer does nothing.
    pub fn offer_dmabuf(&mut self, device: u64) {
        self.offer_dmabuf_with(device, FORMATS.to_vec(), None);
    }

    /// Offers linux-dmabuf with the GPU's own import: every format and
    /// layout EGL imports, shown without a copy. The renderer that draws the
    /// outputs must draw with `gles` or a clone of it, whose context the
    /// textures belong to.
    pub fn offer_dmabuf_on_gpu(&mut self, device: u64, gles: Gles) {
        let mut formats = FORMATS.to_vec();
        for format in gles.dmabuf_formats() {
            let entry = (format.format, format.modifier);
            if !formats.contains(&entry) && formats.len() < MAX_FORMATS {
                formats.push(entry);
            }
        }
        crate::log::info!("linux-dmabuf imported by the GPU formats={}", formats.len());
        self.offer_dmabuf_with(device, formats, Some(gles));
    }

    fn offer_dmabuf_with(&mut self, device: u64, formats: Vec<(u32, u64)>, gpu: Option<Gles>) {
        if self.dmabufs.table.is_some() {
            return;
        }
        let table = match format_table(&formats) {
            Ok(table) => table,
            Err(error) => {
                crate::log::warning!("no linux-dmabuf: the format table could not be made: {error}");
                return;
            }
        };
        self.dmabufs.table = Some(table);
        self.dmabufs.formats = formats;
        self.dmabufs.device = device;
        self.dmabufs.gpu = gpu;
        self.add_global(&zwp_linux_dmabuf_v1::INFO, DMABUF_VERSION, GlobalKind::Dmabuf);
    }

    /// The GPU that imports dma-bufs, for a test's renderer to draw with.
    #[doc(hidden)]
    pub fn dmabuf_gpu(&self) -> Option<Gles> {
        self.dmabufs.gpu.clone()
    }

    /// Lets memfds go to the GPU's import, as the tests' stand-ins for
    /// dma-bufs. Never in the compositor itself: see the module's comment.
    #[doc(hidden)]
    pub fn import_memfds_for_tests(&mut self) {
        self.dmabufs.import_memfds = true;
    }

    pub(crate) fn dmabuf_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        version: u32,
        request: zwp_linux_dmabuf_v1::Request,
    ) -> Result<(), ProtocolError> {
        match request {
            zwp_linux_dmabuf_v1::Request::Destroy => {}
            zwp_linux_dmabuf_v1::Request::CreateParams { params_id } => {
                self.dmabufs.next += 1;
                let params = self.dmabufs.next;
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    params_id,
                    Cow::Borrowed(zwp_linux_buffer_params_v1::NAME),
                    Some(&zwp_linux_buffer_params_v1::INFO),
                    version,
                    ObjectKind::BufferParams(params),
                )?;
                self.dmabufs.params.insert(params, Params::default());
            }
            zwp_linux_dmabuf_v1::Request::GetDefaultFeedback { id }
            | zwp_linux_dmabuf_v1::Request::GetSurfaceFeedback { id, .. } => {
                self.send_feedback(client_id, id, version)?;
            }
        }
        let _ = object;
        Ok(())
    }

    /// Makes a feedback object and tells it what the compositor takes: one
    /// tranche, on the main device, of every format in the table.
    fn send_feedback(&mut self, client_id: ClientId, id: NewId, version: u32) -> Result<(), ProtocolError> {
        let (fd, size) = match &self.dmabufs.table {
            Some((fd, size)) => (fd.try_clone(), *size),
            None => return Ok(()),
        };
        let device = self.dmabufs.device.to_ne_bytes().to_vec();
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        client.create(
            id,
            Cow::Borrowed(zwp_linux_dmabuf_feedback_v1::NAME),
            Some(&zwp_linux_dmabuf_feedback_v1::INFO),
            version,
            ObjectKind::DmabufFeedback,
        )?;
        let Ok(fd) = fd else {
            return Ok(());
        };
        let indices: Vec<u8> = (0..self.dmabufs.formats.len())
            .flat_map(|index| (index as u16).to_ne_bytes())
            .collect();
        client.send(id.0, zwp_linux_dmabuf_feedback_v1::Event::FormatTable { fd, size });
        client.send(
            id.0,
            zwp_linux_dmabuf_feedback_v1::Event::MainDevice { device: device.clone() },
        );
        client.send(
            id.0,
            zwp_linux_dmabuf_feedback_v1::Event::TrancheTargetDevice { device },
        );
        client.send(id.0, zwp_linux_dmabuf_feedback_v1::Event::TrancheFlags { flags: 0 });
        client.send(id.0, zwp_linux_dmabuf_feedback_v1::Event::TrancheFormats { indices });
        client.send(id.0, zwp_linux_dmabuf_feedback_v1::Event::TrancheDone);
        client.send(id.0, zwp_linux_dmabuf_feedback_v1::Event::Done);
        Ok(())
    }

    pub(crate) fn buffer_params_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        params_id: ParamsId,
        request: zwp_linux_buffer_params_v1::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |code, message: String| ProtocolError { object, code, message };
        let Some(params) = self.dmabufs.params.get_mut(&params_id) else {
            return Ok(());
        };
        match request {
            zwp_linux_buffer_params_v1::Request::Destroy => {
                self.dmabufs.params.remove(&params_id);
            }
            zwp_linux_buffer_params_v1::Request::Add {
                fd,
                plane_idx,
                offset,
                stride,
                modifier_hi,
                modifier_lo,
            } => {
                if params.used {
                    return Err(fail(
                        zwp_linux_buffer_params_v1::error::ALREADY_USED,
                        "the params were already used to make a buffer".into(),
                    ));
                }
                let Some(slot) = params.planes.get_mut(plane_idx as usize) else {
                    return Err(fail(
                        zwp_linux_buffer_params_v1::error::PLANE_IDX,
                        format!("plane {plane_idx} of at most 4"),
                    ));
                };
                if slot.is_some() {
                    return Err(fail(
                        zwp_linux_buffer_params_v1::error::PLANE_SET,
                        format!("plane {plane_idx} was already added"),
                    ));
                }
                *slot = Some(Plane {
                    fd,
                    offset,
                    stride,
                    modifier: (u64::from(modifier_hi) << 32) | u64::from(modifier_lo),
                });
            }
            zwp_linux_buffer_params_v1::Request::Create {
                width,
                height,
                format,
                flags,
            } => match self.make_dmabuf_buffer(object, params_id, width, height, format, flags)? {
                Some(made) => {
                    let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                    let id = client.create_server_object(
                        Cow::Borrowed(wl_buffer::NAME),
                        Some(&wl_buffer::INFO),
                        1,
                        ObjectKind::Buffer(made.buffer),
                    )?;
                    client.send(object, zwp_linux_buffer_params_v1::Event::Created { buffer: NewId(id) });
                    self.add_dmabuf_buffer(client_id, id, made, (width, height), format);
                }
                None => {
                    let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                    client.send(object, zwp_linux_buffer_params_v1::Event::Failed);
                }
            },
            zwp_linux_buffer_params_v1::Request::CreateImmed {
                buffer_id,
                width,
                height,
                format,
                flags,
            } => match self.make_dmabuf_buffer(object, params_id, width, height, format, flags)? {
                Some(made) => {
                    let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                    client.create(
                        buffer_id,
                        Cow::Borrowed(wl_buffer::NAME),
                        Some(&wl_buffer::INFO),
                        1,
                        ObjectKind::Buffer(made.buffer),
                    )?;
                    self.add_dmabuf_buffer(client_id, buffer_id.0, made, (width, height), format);
                }
                None => {
                    return Err(fail(
                        zwp_linux_buffer_params_v1::error::INVALID_WL_BUFFER,
                        "the dma-buf cannot be shown".into(),
                    ));
                }
            },
            // A newer version's request, which the dispatch does not let
            // through.
            zwp_linux_buffer_params_v1::Request::SetSamplingDevice { .. } => {}
        }
        Ok(())
    }

    /// Checks what the params describe, and makes the buffer: imported by
    /// the GPU where it takes it, or else mapped for the CPU to copy. A
    /// protocol error for what the protocol forbids; `None` for a buffer
    /// that is valid but that the compositor cannot show.
    fn make_dmabuf_buffer(
        &mut self,
        object: u32,
        params_id: ParamsId,
        width: i32,
        height: i32,
        fourcc: u32,
        flags: u32,
    ) -> Result<Option<Made>, ProtocolError> {
        let fail = |code, message: String| ProtocolError { object, code, message };
        let on_gpu = self.dmabufs.gpu.is_some();
        let params = self.dmabufs.params.get_mut(&params_id).expect("checked by the caller");
        if params.used {
            return Err(fail(
                zwp_linux_buffer_params_v1::error::ALREADY_USED,
                "the params were already used to make a buffer".into(),
            ));
        }
        params.used = true;
        if !self.dmabufs.formats.iter().any(|(known, _)| *known == fourcc) {
            return Err(fail(
                zwp_linux_buffer_params_v1::error::INVALID_FORMAT,
                format!("format {fourcc:#010x} is not in the format table"),
            ));
        }
        // Planes from the first, with no gaps. How many a format has in a
        // layout is the GPU's to say; with no GPU's import, both formats
        // have one.
        let added = params.planes.iter().filter(|plane| plane.is_some()).count();
        let leading = params.planes.iter().take_while(|plane| plane.is_some()).count();
        if leading == 0 || leading != added || (!on_gpu && added != 1) {
            return Err(fail(
                zwp_linux_buffer_params_v1::error::INCOMPLETE,
                format!("{added} planes were added, not from the first in order, or not as the format has them"),
            ));
        }
        if width <= 0 || height <= 0 {
            return Err(fail(
                zwp_linux_buffer_params_v1::error::INVALID_DIMENSIONS,
                format!("a buffer of {width}x{height}"),
            ));
        }
        let planes: Vec<Plane> = params.planes.iter_mut().filter_map(Option::take).collect();
        let modifier = planes[0].modifier;
        if planes.iter().any(|plane| plane.modifier != modifier) {
            return Err(fail(
                zwp_linux_buffer_params_v1::error::INVALID_FORMAT,
                "the planes have different modifiers".into(),
            ));
        }
        for (index, plane) in planes.iter().enumerate() {
            let size = file_size(&plane.fd);
            let rows = if index == 0 { i64::from(height) } else { 1 };
            let end = i64::from(plane.offset) + i64::from(plane.stride) * rows;
            // The CPU's copy reads whole rows of four bytes a pixel.
            let short_rows = !on_gpu && i64::from(plane.stride) < i64::from(width) * 4;
            if short_rows || size.is_none_or(|size| end as u64 > size) {
                return Err(fail(
                    zwp_linux_buffer_params_v1::error::OUT_OF_BOUNDS,
                    format!(
                        "plane {index} of {width}x{height} at offset {} with stride {} is outside the dma-buf",
                        plane.offset, plane.stride
                    ),
                ));
            }
        }
        // Flags change how the buffer is read, which neither import does.
        if flags != 0 {
            return Ok(None);
        }
        if let Some(texture) = self.import_on_gpu(&planes, width, height, fourcc, modifier) {
            return Ok(Some(Made {
                buffer: self.next_resource(),
                source: Source::Gpu(Rc::new(texture)),
                offset: planes[0].offset,
                stride: planes[0].stride,
            }));
        }
        // The CPU's copy: one plane of rows, in a format it reads.
        let readable = FORMATS.contains(&(fourcc, modifier))
            && planes.len() == 1
            && i64::from(planes[0].stride) >= i64::from(width) * 4;
        if !readable {
            return Ok(None);
        }
        let plane = planes.into_iter().next().expect("one plane");
        let size = file_size(&plane.fd).unwrap_or(0) as usize;
        let Ok(mapping) = Mapping::new(plane.fd.as_fd(), size) else {
            return Ok(None);
        };
        Ok(Some(Made {
            buffer: self.next_resource(),
            source: Source::Dmabuf { mapping, fd: plane.fd },
            offset: plane.offset,
            stride: plane.stride,
        }))
    }

    /// The GPU's texture of a dma-buf, where there is a GPU's import and it
    /// takes the buffer.
    fn import_on_gpu(
        &self,
        planes: &[Plane],
        width: i32,
        height: i32,
        fourcc: u32,
        modifier: u64,
    ) -> Option<gles::Texture> {
        let gles = self.dmabufs.gpu.as_ref()?;
        let real = planes
            .iter()
            .all(|plane| fulcrum_sys::dmabuf::is_dmabuf(plane.fd.as_fd()));
        if !real && !self.dmabufs.import_memfds {
            return None;
        }
        let planes = planes
            .iter()
            .map(|plane| {
                Some(DmabufPlane {
                    fd: plane.fd.try_clone().ok()?,
                    offset: plane.offset,
                    stride: plane.stride,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let dmabuf = gles::Dmabuf {
            width,
            height,
            format: fourcc,
            modifier,
            planes,
        };
        gles.import(&dmabuf)
            .map_err(|error| crate::log::debug!("the GPU did not import a dma-buf: {error}"))
            .ok()
    }

    fn add_dmabuf_buffer(&mut self, client_id: ClientId, object: u32, made: Made, size: (i32, i32), fourcc: u32) {
        let format = if fourcc == FOURCC_ARGB8888 {
            format::ARGB8888
        } else {
            format::XRGB8888
        };
        self.buffers.insert(
            made.buffer,
            Buffer::new(
                client_id,
                object,
                made.source,
                made.offset as usize,
                size,
                made.stride as i32,
                format,
            ),
        );
    }

    /// Descriptors a client's params hold.
    pub(crate) fn params_descriptors(&self, params: ParamsId) -> usize {
        self.dmabufs
            .params
            .get(&params)
            .map_or(0, |params| params.planes.iter().filter(|plane| plane.is_some()).count())
    }
}

/// A file's size, as `lseek` to its end says: a dma-buf's size.
fn file_size(fd: &OwnedFd) -> Option<u64> {
    let mut file = std::fs::File::from(fd.try_clone().ok()?);
    file.seek(SeekFrom::End(0)).ok()
}

/// The format table: for each entry, a format, four bytes of padding, and a
/// modifier. Sealed, so a client can only read it.
fn format_table(formats: &[(u32, u64)]) -> std::io::Result<(OwnedFd, u32)> {
    use rustix::fs::{MemfdFlags, SealFlags};
    let bytes: Vec<u8> = formats
        .iter()
        .flat_map(|(format, modifier)| {
            let mut entry = Vec::with_capacity(16);
            entry.extend_from_slice(&format.to_ne_bytes());
            entry.extend_from_slice(&[0; 4]);
            entry.extend_from_slice(&modifier.to_ne_bytes());
            entry
        })
        .collect();
    let fd = rustix::fs::memfd_create(
        "fulcrum-dmabuf-formats",
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )?;
    rustix::io::write(&fd, &bytes)?;
    rustix::fs::fcntl_add_seals(
        &fd,
        SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL,
    )?;
    Ok((fd, bytes.len() as u32))
}
