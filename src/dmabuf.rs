//! linux-dmabuf: buffers a client hands over as dma-buf descriptors, as a
//! client of a GPU does. Version 4: what the compositor takes is told in
//! feedback, as a table of formats and modifiers in sealed shared memory.
//!
//! The compositor takes single-plane `XR24` and `AR24` buffers laid out in
//! rows (the `LINEAR` modifier), maps them, and copies their pixels at each
//! commit as it copies shared memory, through the same guarded mapping. Every
//! client of a GPU can make such a buffer. Importing tiled buffers into the
//! GPU without a copy comes with the GLES renderer's import.
//!
//! The table (ADR 0020's, with ADR 0023's) decides who is offered it: the
//! shell, and a compartment whose needs include the GPU.

use std::{
    borrow::Cow,
    collections::HashMap,
    io::{Seek, SeekFrom},
    os::fd::{AsFd, OwnedFd},
};

use fulcrum_sys::shm::Mapping;
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

/// What the compositor takes, in the order of the format table.
pub const FORMATS: [(u32, u64); 2] = [(FOURCC_XRGB8888, MODIFIER_LINEAR), (FOURCC_ARGB8888, MODIFIER_LINEAR)];

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
    /// The `dev_t` of the device clients should allocate on.
    device: u64,
}

impl State {
    /// Offers linux-dmabuf, with `device` as the `dev_t` of the DRM device
    /// clients should allocate on. The table still decides who sees it.
    pub fn offer_dmabuf(&mut self, device: u64) {
        let table = match format_table() {
            Ok(table) => table,
            Err(error) => {
                crate::log::warning!("no linux-dmabuf: the format table could not be made: {error}");
                return;
            }
        };
        self.dmabufs.table = Some(table);
        self.dmabufs.device = device;
        self.add_global(&zwp_linux_dmabuf_v1::INFO, DMABUF_VERSION, GlobalKind::Dmabuf);
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
        let indices: Vec<u8> = (0..FORMATS.len() as u16).flat_map(u16::to_ne_bytes).collect();
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
            } => match self.make_dmabuf_buffer(client_id, object, params_id, width, height, format, flags)? {
                Some((buffer, mapping, plane)) => {
                    let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                    let id = client.create_server_object(
                        Cow::Borrowed(wl_buffer::NAME),
                        Some(&wl_buffer::INFO),
                        1,
                        ObjectKind::Buffer(buffer),
                    )?;
                    client.send(object, zwp_linux_buffer_params_v1::Event::Created { buffer: NewId(id) });
                    self.add_dmabuf_buffer(client_id, id, buffer, mapping, plane, width, height, format);
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
            } => match self.make_dmabuf_buffer(client_id, object, params_id, width, height, format, flags)? {
                Some((buffer, mapping, plane)) => {
                    let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                    client.create(
                        buffer_id,
                        Cow::Borrowed(wl_buffer::NAME),
                        Some(&wl_buffer::INFO),
                        1,
                        ObjectKind::Buffer(buffer),
                    )?;
                    self.add_dmabuf_buffer(client_id, buffer_id.0, buffer, mapping, plane, width, height, format);
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

    /// Checks what the params describe, and maps it. A protocol error for
    /// what the protocol forbids; `None` for a buffer that is valid but that
    /// the compositor cannot show.
    #[allow(clippy::too_many_arguments)]
    fn make_dmabuf_buffer(
        &mut self,
        client_id: ClientId,
        object: u32,
        params_id: ParamsId,
        width: i32,
        height: i32,
        fourcc: u32,
        flags: u32,
    ) -> Result<Option<(crate::shm::BufferId, Mapping, Plane)>, ProtocolError> {
        let fail = |code, message: String| ProtocolError { object, code, message };
        let params = self.dmabufs.params.get_mut(&params_id).expect("checked by the caller");
        if params.used {
            return Err(fail(
                zwp_linux_buffer_params_v1::error::ALREADY_USED,
                "the params were already used to make a buffer".into(),
            ));
        }
        params.used = true;
        if !FORMATS.iter().any(|(known, _)| *known == fourcc) {
            return Err(fail(
                zwp_linux_buffer_params_v1::error::INVALID_FORMAT,
                format!("format {fourcc:#010x} is not in the format table"),
            ));
        }
        // One plane, as both formats have.
        let added = params.planes.iter().filter(|plane| plane.is_some()).count();
        if params.planes[0].is_none() || added != 1 {
            return Err(fail(
                zwp_linux_buffer_params_v1::error::INCOMPLETE,
                format!("the format has one plane, and {added} were added"),
            ));
        }
        if width <= 0 || height <= 0 {
            return Err(fail(
                zwp_linux_buffer_params_v1::error::INVALID_DIMENSIONS,
                format!("a buffer of {width}x{height}"),
            ));
        }
        let plane = params.planes[0].take().expect("checked above");
        let size = file_size(&plane.fd);
        let end = (i64::from(plane.stride) >= i64::from(width) * 4)
            .then(|| i64::from(plane.offset) + i64::from(plane.stride) * i64::from(height));
        if end.is_none_or(|end| size.is_none_or(|size| end as u64 > size)) {
            return Err(fail(
                zwp_linux_buffer_params_v1::error::OUT_OF_BOUNDS,
                format!(
                    "{width}x{height} at offset {} with stride {} is outside the dma-buf",
                    plane.offset, plane.stride
                ),
            ));
        }
        // Flags change how the buffer is read, which the copy does not do;
        // and a tiled buffer cannot be read in rows.
        if flags != 0 || plane.modifier != MODIFIER_LINEAR {
            return Ok(None);
        }
        let Ok(mapping) = Mapping::new(plane.fd.as_fd(), size.unwrap_or(0) as usize) else {
            return Ok(None);
        };
        let _ = client_id;
        Ok(Some((self.next_resource(), mapping, plane)))
    }

    #[allow(clippy::too_many_arguments)]
    fn add_dmabuf_buffer(
        &mut self,
        client_id: ClientId,
        object: u32,
        buffer: crate::shm::BufferId,
        mapping: Mapping,
        plane: Plane,
        width: i32,
        height: i32,
        fourcc: u32,
    ) {
        let format = if fourcc == FOURCC_ARGB8888 {
            format::ARGB8888
        } else {
            format::XRGB8888
        };
        self.buffers.insert(
            buffer,
            Buffer::new(
                client_id,
                object,
                Source::Dmabuf { mapping, fd: plane.fd },
                plane.offset as usize,
                (width, height),
                plane.stride as i32,
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
fn format_table() -> std::io::Result<(OwnedFd, u32)> {
    use rustix::fs::{MemfdFlags, SealFlags};
    let bytes: Vec<u8> = FORMATS
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
