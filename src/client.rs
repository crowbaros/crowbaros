//! One connected client: its connection, who it is, and its objects.
//!
//! Every client starts with one object, `wl_display` at ID 1. A client makes
//! objects with IDs below [`SERVER_ID_START`], each unused when it is made. An
//! object a client destroys, or one the compositor destroys for it, is
//! confirmed with `wl_display.delete_id`, so the client can use the ID again.
//!
//! A client that breaks the protocol is sent `wl_display.error` and is then
//! disconnected, as libwayland does. Every other client goes on being served.

use std::{borrow::Cow, collections::HashMap, os::unix::net::UnixStream};

use fulcrum_wire::{
    Connection, ConnectionError, Encode, InterfaceInfo, NewId,
    codec::SERVER_ID_START,
    protocol::wayland::{wl_display, wl_registry},
};

use crate::{identity::Identity, log};

pub type ClientId = u64;

/// The most objects one client may hold at once.
pub const MAX_OBJECTS: usize = 16 * 1024;

/// What an object is, and the compositor's state for it.
#[derive(Debug)]
pub(crate) enum ObjectKind {
    Display,
    Registry,
    Callback,
    /// A `wl_output`, for the output with this number. Surfaces read it when
    /// they enter outputs.
    #[allow(dead_code)]
    Output(u64),
    SecurityContextManager,
    SecurityContext(Box<crate::connections::PendingContext>),
    Compositor,
    Surface(crate::surface::SurfaceId),
    Region(crate::surface::RegionId),
    Subcompositor,
    /// A `wl_subsurface`, for the surface it gives the role to.
    Subsurface(crate::surface::SurfaceId),
    Shm,
    Pool(crate::shm::PoolId),
    Buffer(crate::shm::BufferId),
    /// A global's object whose requests this build does not carry out yet,
    /// or a global a test planted. Any request on it is a protocol error.
    Unimplemented,
}

#[derive(Debug)]
pub(crate) struct Object {
    pub interface: Cow<'static, str>,
    /// The generated description, or none for a planted global.
    pub info: Option<&'static InterfaceInfo>,
    pub version: u32,
    pub kind: ObjectKind,
}

pub(crate) struct Client {
    pub id: ClientId,
    pub conn: Connection,
    pub identity: Identity,
    pub objects: HashMap<u32, Object>,
    /// The client broke the protocol, or its connection failed. It is
    /// disconnected once what is queued for it has been sent.
    pub doomed: bool,
    /// Its socket is watched for room to write.
    pub watching_writes: bool,
}

/// Why a client cannot be served any longer.
#[derive(Debug)]
pub(crate) struct ProtocolError {
    pub object: u32,
    pub code: u32,
    pub message: String,
}

impl Client {
    pub fn new(id: ClientId, stream: UnixStream, identity: Identity) -> std::io::Result<Self> {
        let mut objects = HashMap::new();
        objects.insert(
            1,
            Object {
                interface: Cow::Borrowed(wl_display::NAME),
                info: Some(&wl_display::INFO),
                version: 1,
                kind: ObjectKind::Display,
            },
        );
        Ok(Client {
            id,
            conn: Connection::new(stream)?,
            identity,
            objects,
            doomed: false,
            watching_writes: false,
        })
    }

    /// Queues an event on one of this client's objects. A client that
    /// stopped reading is doomed rather than given more.
    pub fn send(&mut self, object: u32, event: impl Encode) {
        if self.doomed {
            return;
        }
        match event.to_message(object) {
            Ok((bytes, fds)) => {
                if let Err(error) = self.conn.queue(bytes, fds) {
                    log::warning!("client {}: {error}; disconnecting it", self.id);
                    self.doomed = true;
                }
            }
            Err(error) => {
                log::error!("client {}: an event could not be written: {error}", self.id);
                self.doomed = true;
            }
        }
    }

    /// Sends the protocol error and dooms the client.
    pub fn post_error(&mut self, error: ProtocolError) {
        if self.doomed {
            return;
        }
        log::warning!(
            "client {}: protocol error on object {} (code {}): {}",
            self.id,
            error.object,
            error.code,
            error.message
        );
        self.send(
            1,
            wl_display::Event::Error {
                object_id: fulcrum_wire::ObjectId(error.object),
                code: error.code,
                message: error.message,
            },
        );
        self.doomed = true;
    }

    /// Makes an object with an ID the client chose.
    pub fn create(
        &mut self,
        id: NewId,
        interface: Cow<'static, str>,
        info: Option<&'static InterfaceInfo>,
        version: u32,
        kind: ObjectKind,
    ) -> Result<(), ProtocolError> {
        if id.0 >= SERVER_ID_START || self.objects.contains_key(&id.0) {
            return Err(ProtocolError {
                object: 1,
                code: wl_display::error::INVALID_OBJECT,
                message: format!("invalid new id {}", id.0),
            });
        }
        if self.objects.len() >= MAX_OBJECTS {
            return Err(ProtocolError {
                object: 1,
                code: wl_display::error::NO_MEMORY,
                message: format!("more than {MAX_OBJECTS} objects"),
            });
        }
        self.objects.insert(
            id.0,
            Object {
                interface,
                info,
                version,
                kind,
            },
        );
        Ok(())
    }

    /// Destroys an object and tells the client its ID is free.
    pub fn destroy(&mut self, id: u32) {
        if self.objects.remove(&id).is_some() && id < SERVER_ID_START {
            self.send(1, wl_display::Event::DeleteId { id });
        }
    }

    /// The IDs of this client's registries.
    pub fn registries(&self) -> Vec<u32> {
        self.objects
            .iter()
            .filter(|(_, object)| matches!(object.kind, ObjectKind::Registry))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Tells each of the client's registries about a global.
    pub fn announce(&mut self, name: u32, interface: &str, version: u32) {
        for registry in self.registries() {
            self.send(
                registry,
                wl_registry::Event::Global {
                    name,
                    interface: interface.to_string(),
                    version,
                },
            );
        }
    }

    pub fn fail(&mut self, error: &ConnectionError) {
        if !self.doomed {
            log::debug!("client {}: {error}", self.id);
            self.doomed = true;
        }
    }
}
