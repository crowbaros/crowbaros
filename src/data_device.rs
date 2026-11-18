//! The data device: `wl_data_device_manager`, its sources, and each seat's
//! data device.
//!
//! ADR 0020 offers it to every compartment and the shell. Moving data between
//! clients is slice 4 (CrowbarOS issue 166, Milestone 6): it must follow the
//! person's act, and the broker records it. Until then the protocol works and
//! nothing moves. A client can make sources and set its selection, which no
//! other client is offered, and a drag is refused at once by cancelling its
//! source. Programs that will not start without the data device, such as
//! `foot`, start.

use std::{borrow::Cow, collections::HashMap};

use fulcrum_wire::{
    NewId, ObjectId,
    protocol::wayland::{wl_data_device, wl_data_device_manager, wl_data_source},
};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    surface::Role,
};

/// The `wl_data_device_manager` version Fulcrum offers.
pub const DATA_DEVICE_MANAGER_VERSION: u32 = 3;
/// MIME types one source may offer.
pub const SOURCE_MIME_TYPES: usize = 64;
/// Bytes of MIME types one client's sources may hold in all.
pub const CLIENT_MIME_BYTES: usize = 64 * 1024;

const EVERY_ACTION: u32 = wl_data_device_manager::dnd_action::COPY
    | wl_data_device_manager::dnd_action::MOVE
    | wl_data_device_manager::dnd_action::ASK;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct SourceId(pub u64);

#[derive(Debug)]
pub(crate) struct DataSource {
    pub client: ClientId,
    pub object: u32,
    pub mime_types: Vec<String>,
    pub actions: Option<u32>,
    /// It was given to a selection or a drag, and may not be given again.
    pub used: bool,
}

#[derive(Debug, Default)]
pub(crate) struct DataDevices {
    pub sources: HashMap<SourceId, DataSource>,
    next_source: u64,
    /// The source each client last set as its selection.
    pub selections: HashMap<ClientId, SourceId>,
}

impl State {
    pub(crate) fn data_device_manager_request(
        &mut self,
        client_id: ClientId,
        version: u32,
        request: wl_data_device_manager::Request,
    ) -> Result<(), ProtocolError> {
        match request {
            wl_data_device_manager::Request::CreateDataSource { id } => {
                self.data.next_source += 1;
                let source = SourceId(self.data.next_source);
                self.create_data_object(
                    client_id,
                    id,
                    wl_data_source::NAME,
                    version,
                    ObjectKind::DataSource(source),
                )?;
                self.data.sources.insert(
                    source,
                    DataSource {
                        client: client_id,
                        object: id.0,
                        mime_types: Vec::new(),
                        actions: None,
                        used: false,
                    },
                );
            }
            wl_data_device_manager::Request::GetDataDevice { id, .. } => {
                self.create_data_object(client_id, id, wl_data_device::NAME, version, ObjectKind::DataDevice)?;
            }
            // A destructor, from a version Fulcrum does not offer.
            wl_data_device_manager::Request::Release => {}
        }
        Ok(())
    }

    fn create_data_object(
        &mut self,
        client_id: ClientId,
        id: NewId,
        interface: &'static str,
        version: u32,
        kind: ObjectKind,
    ) -> Result<(), ProtocolError> {
        let info = fulcrum_wire::protocol::interface(interface);
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        client.create(id, Cow::Borrowed(interface), info, version, kind)
    }

    pub(crate) fn data_source_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        source: SourceId,
        request: wl_data_source::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |code, message: String| ProtocolError { object, code, message };
        match request {
            wl_data_source::Request::Offer { mime_type } => {
                let held: usize = self
                    .data
                    .sources
                    .values()
                    .filter(|source| source.client == client_id)
                    .flat_map(|source| source.mime_types.iter().map(String::len))
                    .sum();
                let Some(data) = self.data.sources.get_mut(&source) else {
                    return Ok(());
                };
                if data.mime_types.contains(&mime_type) {
                    return Ok(());
                }
                if data.mime_types.len() >= SOURCE_MIME_TYPES || held + mime_type.len() > CLIENT_MIME_BYTES {
                    return Err(ProtocolError {
                        object: 1,
                        code: fulcrum_wire::protocol::wayland::wl_display::error::NO_MEMORY,
                        message: format!(
                            "a source may offer {SOURCE_MIME_TYPES} MIME types, and a client's sources \
                             {CLIENT_MIME_BYTES} bytes of them"
                        ),
                    });
                }
                data.mime_types.push(mime_type);
            }
            wl_data_source::Request::Destroy => self.forget_source(source),
            wl_data_source::Request::SetActions { dnd_actions } => {
                let Some(data) = self.data.sources.get_mut(&source) else {
                    return Ok(());
                };
                if dnd_actions & !EVERY_ACTION != 0 {
                    return Err(fail(
                        wl_data_source::error::INVALID_ACTION_MASK,
                        format!("{dnd_actions:#x} is not a set of drag-and-drop actions"),
                    ));
                }
                if data.actions.is_some() {
                    return Err(fail(
                        wl_data_source::error::INVALID_ACTION_MASK,
                        "the actions of a source are set once".into(),
                    ));
                }
                if data.used {
                    return Err(fail(
                        wl_data_source::error::INVALID_SOURCE,
                        "the actions of a source are set before it is used".into(),
                    ));
                }
                data.actions = Some(dnd_actions);
            }
        }
        Ok(())
    }

    pub(crate) fn data_device_request(
        &mut self,
        client_id: ClientId,
        object: u32,
        request: wl_data_device::Request,
    ) -> Result<(), ProtocolError> {
        let fail = |code, message: String| ProtocolError { object, code, message };
        match request {
            wl_data_device::Request::SetSelection { source, .. } => {
                let new = match source {
                    Some(source) => Some(self.use_source(client_id, object, source)?),
                    None => None,
                };
                let old = match new {
                    Some(new) => self.data.selections.insert(client_id, new),
                    None => self.data.selections.remove(&client_id),
                };
                if let Some(old) = old.filter(|old| Some(*old) != new) {
                    self.cancel_source(old);
                }
            }
            wl_data_device::Request::StartDrag { source, icon, .. } => {
                if let Some(icon) = icon {
                    let surface = self
                        .surface_of(client_id, icon)
                        .expect("the dispatch checked the argument's interface");
                    if self.surfaces[&surface].role != Role::None {
                        return Err(fail(
                            wl_data_device::error::ROLE,
                            format!("wl_surface#{} already has a role", icon.0),
                        ));
                    }
                }
                // Drag and drop between clients comes with slice 4. Until
                // then a drag ends as soon as it starts.
                if let Some(source) = source {
                    let source = self.use_source(client_id, object, source)?;
                    self.cancel_source(source);
                }
            }
            wl_data_device::Request::Release => {}
        }
        Ok(())
    }

    /// The source an argument names, marked as used.
    fn use_source(&mut self, client_id: ClientId, device: u32, object: ObjectId) -> Result<SourceId, ProtocolError> {
        let source = match self
            .clients
            .get(&client_id)
            .and_then(|client| client.objects.get(&object.0))
        {
            Some(found) => match found.kind {
                ObjectKind::DataSource(source) => source,
                _ => unreachable!("the dispatch checked the argument's interface"),
            },
            None => unreachable!("the dispatch checked the argument exists"),
        };
        let data = self
            .data
            .sources
            .get_mut(&source)
            .expect("a source object has its source");
        if data.used {
            return Err(ProtocolError {
                object: device,
                code: wl_data_device::error::USED_SOURCE,
                message: format!("wl_data_source#{} was already used", object.0),
            });
        }
        data.used = true;
        Ok(source)
    }

    /// Tells a source's client it is no longer the selection, or that its
    /// drag is over.
    fn cancel_source(&mut self, source: SourceId) {
        let Some(data) = self.data.sources.get(&source) else {
            return;
        };
        if let Some(client) = self.clients.get_mut(&data.client) {
            client.send(data.object, wl_data_source::Event::Cancelled);
        }
    }

    /// Lets go of a source, when it is destroyed or its client is gone.
    pub(crate) fn forget_source(&mut self, source: SourceId) {
        if let Some(data) = self.data.sources.remove(&source)
            && self.data.selections.get(&data.client) == Some(&source)
        {
            self.data.selections.remove(&data.client);
        }
    }
}
