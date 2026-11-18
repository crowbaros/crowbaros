//! Shared-memory buffers: `wl_shm`, `wl_shm_pool`, and `wl_buffer`.
//!
//! A client makes a pool from a file it shares, and buffers inside it. When a
//! surface commits a buffer, the compositor copies its pixels and releases it
//! at once, so the client may draw into it again. Every read of the pool goes
//! through `fulcrum-sys`'s guarded mapping: a client that truncates its file
//! gets a protocol error, and the compositor goes on.

use std::{borrow::Cow, os::fd::OwnedFd};

use fulcrum_sys::shm::Mapping;
use fulcrum_wire::protocol::wayland::{wl_buffer, wl_shm, wl_shm_pool};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    image::{Image, format},
};

/// The largest pool, in bytes. Two 4K frames fit with room to spare.
pub const MAX_POOL_SIZE: usize = 256 * 1024 * 1024;

pub(crate) type PoolId = u64;
pub(crate) type BufferId = u64;

pub(crate) struct Pool {
    /// Its client, whose budget of shared memory it counts against.
    #[allow(dead_code)]
    pub client: ClientId,
    mapping: Mapping,
    fd: OwnedFd,
    /// Buffers made from it that still exist. A pool the client destroyed
    /// lives until its last buffer goes.
    buffers: usize,
    object_alive: bool,
}

pub(crate) struct Buffer {
    pub client: ClientId,
    pub object: u32,
    pool: PoolId,
    offset: usize,
    width: i32,
    height: i32,
    stride: i32,
    format: u32,
}

impl Buffer {
    pub fn size(&self) -> (i32, i32) {
        (self.width, self.height)
    }
}

impl State {
    fn next_resource(&mut self) -> u64 {
        self.next_resource += 1;
        self.next_resource
    }

    /// A client bound `wl_shm`: tells it the formats.
    pub(crate) fn shm_bound(&mut self, client_id: ClientId, object: u32) {
        if let Some(client) = self.clients.get_mut(&client_id) {
            for format in [format::ARGB8888, format::XRGB8888] {
                client.send(object, wl_shm::Event::Format { format });
            }
        }
    }

    pub(crate) fn shm_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        request: wl_shm::Request,
    ) -> Result<(), ProtocolError> {
        let wl_shm::Request::CreatePool { id, fd, size } = request else {
            // Release: the dispatch destroys the object.
            return Ok(());
        };
        let fail = |code, message: String| ProtocolError { object, code, message };
        let size = usize::try_from(size)
            .ok()
            .filter(|size| *size > 0 && *size <= MAX_POOL_SIZE)
            .ok_or_else(|| fail(wl_shm::error::INVALID_STRIDE, format!("invalid pool size {size}")))?;
        let mapping = Mapping::new(std::os::fd::AsFd::as_fd(&fd), size)
            .map_err(|error| fail(wl_shm::error::INVALID_FD, format!("could not map the pool: {error}")))?;
        let pool = self.next_resource();
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        client.create(
            id,
            Cow::Borrowed(wl_shm_pool::NAME),
            Some(&wl_shm_pool::INFO),
            client.objects.get(&object).map_or(1, |object| object.version),
            ObjectKind::Pool(pool),
        )?;
        self.pools.insert(
            pool,
            Pool {
                client: client_id,
                mapping,
                fd,
                buffers: 0,
                object_alive: true,
            },
        );
        Ok(())
    }

    pub(crate) fn pool_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        pool_id: PoolId,
        request: wl_shm_pool::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |code, message: String| ProtocolError { object, code, message };
        match request {
            wl_shm_pool::Request::CreateBuffer {
                id,
                offset,
                width,
                height,
                stride,
                format,
            } => {
                if format != format::ARGB8888 && format != format::XRGB8888 {
                    return Err(fail(
                        wl_shm::error::INVALID_FORMAT,
                        format!("invalid format {format:#x}"),
                    ));
                }
                let pool_size = self.pools.get(&pool_id).map_or(0, |pool| pool.mapping.len());
                let end = (offset >= 0 && width > 0 && height > 0)
                    .then(|| i64::from(width) * 4)
                    .filter(|row| i64::from(stride) >= *row)
                    .and_then(|_| i64::from(stride).checked_mul(i64::from(height)))
                    .and_then(|bytes| bytes.checked_add(i64::from(offset)));
                if end.is_none_or(|end| end > pool_size as i64) {
                    return Err(fail(
                        wl_shm::error::INVALID_STRIDE,
                        format!(
                            "invalid buffer: offset {offset}, {width}x{height}, stride {stride}, in a pool of {pool_size}"
                        ),
                    ));
                }
                let buffer = self.next_resource();
                let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
                client.create(
                    id,
                    Cow::Borrowed(wl_buffer::NAME),
                    Some(&wl_buffer::INFO),
                    1,
                    ObjectKind::Buffer(buffer),
                )?;
                if let Some(pool) = self.pools.get_mut(&pool_id) {
                    pool.buffers += 1;
                }
                self.buffers.insert(
                    buffer,
                    Buffer {
                        client: client_id,
                        object: id.0,
                        pool: pool_id,
                        offset: offset as usize,
                        width,
                        height,
                        stride,
                        format,
                    },
                );
                Ok(())
            }
            wl_shm_pool::Request::Destroy => {
                if let Some(pool) = self.pools.get_mut(&pool_id) {
                    pool.object_alive = false;
                }
                self.drop_pool_if_unused(pool_id);
                Ok(())
            }
            wl_shm_pool::Request::Resize { size } => {
                let pool = self
                    .pools
                    .get_mut(&pool_id)
                    .expect("the pool exists while its object does");
                let size = usize::try_from(size).unwrap_or(0);
                if size < pool.mapping.len() {
                    return Err(fail(wl_shm::error::INVALID_STRIDE, "shrinking pool invalid".into()));
                }
                if size > MAX_POOL_SIZE {
                    return Err(fail(wl_shm::error::INVALID_STRIDE, format!("invalid pool size {size}")));
                }
                pool.mapping
                    .grow(&pool.fd, size)
                    .map_err(|error| fail(wl_shm::error::INVALID_FD, format!("could not map the pool: {error}")))
            }
        }
    }

    /// The client destroyed a buffer, or the client is gone.
    pub(crate) fn forget_buffer(&mut self, buffer: BufferId) {
        if let Some(buffer) = self.buffers.remove(&buffer) {
            if let Some(pool) = self.pools.get_mut(&buffer.pool) {
                pool.buffers -= 1;
            }
            self.drop_pool_if_unused(buffer.pool);
        }
    }

    /// The client destroyed a pool's object, or the client is gone.
    pub(crate) fn forget_pool(&mut self, pool: PoolId) {
        if let Some(pool_state) = self.pools.get_mut(&pool) {
            pool_state.object_alive = false;
        }
        self.drop_pool_if_unused(pool);
    }

    fn drop_pool_if_unused(&mut self, pool: PoolId) {
        if self
            .pools
            .get(&pool)
            .is_some_and(|pool| !pool.object_alive && pool.buffers == 0)
        {
            self.pools.remove(&pool);
        }
    }

    /// Copies a committed buffer's pixels, and releases the buffer. A pool
    /// whose file the client truncated is a protocol error on the buffer.
    pub(crate) fn import_buffer(&mut self, buffer_id: BufferId) -> Result<Image, ProtocolError> {
        let buffer = self.buffers.get(&buffer_id).expect("a committed buffer exists");
        let (client_id, object) = (buffer.client, buffer.object);
        let (offset, width, height, stride, format) =
            (buffer.offset, buffer.width, buffer.height, buffer.stride, buffer.format);
        let pool = self
            .pools
            .get_mut(&buffer.pool)
            .expect("a buffer's pool lives as long as it");
        let image = pool
            .mapping
            .read(|bytes| Image::from_shm(bytes, offset, width, height, stride, format))
            .map_err(|truncated| ProtocolError {
                object,
                code: wl_shm::error::INVALID_FD,
                message: format!("error accessing SHM buffer: {truncated}"),
            })?;
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.send(object, wl_buffer::Event::Release);
        }
        Ok(image)
    }

    pub(crate) fn buffer_size(&self, buffer: BufferId) -> Option<(i32, i32)> {
        self.buffers.get(&buffer).map(Buffer::size)
    }
}
