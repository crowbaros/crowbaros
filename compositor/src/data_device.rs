//! The data device: `wl_data_device_manager`, its sources, each seat's data
//! device, and the offers it makes.
//!
//! Every client is offered it (ADR 0032). Copy, paste, and drag and drop
//! work between the shell and every app, as on any desktop: there is one
//! selection, offered to the client that holds the keyboard, as it gains it
//! and when the selection changes. A drag can drop on any client. The
//! person's act moves the data.

use std::{borrow::Cow, collections::HashMap};

use fulcrum_wire::{
    NewId, ObjectId,
    protocol::wayland::{wl_data_device, wl_data_device_manager, wl_data_source},
};

use fulcrum_wire::protocol::wayland::wl_data_offer;

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    drag::{Drag, DragOffer, DragStart},
};

/// Offers one client may hold at once. Past them it is offered nothing more
/// until it destroys some: a client that never does holds only its own.
pub const CLIENT_OFFERS: usize = 256;

/// The `wl_data_device_manager` version Fulcrum offers.
pub const DATA_DEVICE_MANAGER_VERSION: u32 = 3;
/// MIME types one source may offer.
pub const SOURCE_MIME_TYPES: usize = 64;
/// Bytes of MIME types one client's sources may hold in all.
pub const CLIENT_MIME_BYTES: usize = 64 * 1024;

pub(crate) const EVERY_ACTION: u32 = wl_data_device_manager::dnd_action::COPY
    | wl_data_device_manager::dnd_action::MOVE
    | wl_data_device_manager::dnd_action::ASK;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct SourceId(pub u64);

#[derive(Debug)]
pub(crate) struct DataSource {
    pub client: ClientId,
    pub object: u32,
    pub version: u32,
    pub mime_types: Vec<String>,
    pub actions: Option<u32>,
    /// It was given to a selection or a drag, and may not be given again.
    pub used: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct OfferId(pub u64);

/// An offer belongs to a single destination.
#[derive(Debug)]
pub(crate) struct Offer {
    pub client: ClientId,
    pub version: u32,
    pub source: SourceId,
    pub drag: Option<DragOffer>,
}

#[derive(Debug, Default)]
pub(crate) struct DataDevices {
    pub sources: HashMap<SourceId, DataSource>,
    next_source: u64,
    /// The selection: the source a client set last.
    pub selection: Option<SourceId>,
    pub offers: HashMap<OfferId, Offer>,
    next_offer: u64,
    pub drag: Option<Drag>,
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
                        version,
                        mime_types: Vec::new(),
                        actions: None,
                        used: false,
                    },
                );
            }
            wl_data_device_manager::Request::GetDataDevice { id, .. } => {
                self.create_data_object(client_id, id, wl_data_device::NAME, version, ObjectKind::DataDevice)?;
                // A client that holds the keyboard learns the selection.
                if self.keyboard_client() == Some(client_id) {
                    self.offer_selection_on(client_id, id.0);
                }
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
                let held = self.mime_bytes_held(client_id) + self.primary_mime_bytes_held(client_id);
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
        match request {
            wl_data_device::Request::SetSelection { source, .. } => {
                let new = match source {
                    Some(source) => {
                        let source = self.use_source(client_id, object, source)?;
                        let data = &self.data.sources[&source];
                        if data.actions.is_some() {
                            return Err(ProtocolError {
                                object: data.object,
                                code: wl_data_source::error::INVALID_SOURCE,
                                message: "a source with drag actions cannot be a selection".into(),
                            });
                        }
                        Some(source)
                    }
                    None => None,
                };
                if !self.clients.contains_key(&client_id) {
                    if let Some(new) = new {
                        self.cancel_source(new);
                    }
                    return Ok(());
                }
                let old = std::mem::replace(&mut self.data.selection, new);
                if let Some(old) = old.filter(|old| Some(*old) != new) {
                    self.cancel_source(old);
                }
                self.capture_clipboard();
                self.offer_selection_to_keyboard();
            }
            wl_data_device::Request::StartDrag {
                source,
                origin,
                icon,
                serial,
            } => {
                self.start_drag(
                    client_id,
                    object,
                    DragStart {
                        source,
                        origin,
                        icon,
                        serial,
                    },
                )?;
            }
            wl_data_device::Request::Release => self.drag_device_released(client_id, object),
        }
        Ok(())
    }

    /// The source an argument names, marked as used.
    pub(crate) fn use_source(
        &mut self,
        client_id: ClientId,
        device: u32,
        object: ObjectId,
    ) -> Result<SourceId, ProtocolError> {
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
        self.send_source(source, 1, wl_data_source::Event::Cancelled);
    }

    pub(crate) fn send_source(&mut self, source: SourceId, since: u32, event: wl_data_source::Event) {
        let Some(data) = self.data.sources.get(&source) else {
            return;
        };
        if data.version >= since
            && let Some(client) = self.clients.get_mut(&data.client)
        {
            client.send(data.object, event);
        }
    }

    /// Lets go of a source, when it is destroyed or its client is gone. If
    /// it was the selection there is none, and the client with the keyboard
    /// hears so.
    pub(crate) fn forget_source(&mut self, source: SourceId) {
        if self.data.sources.remove(&source).is_none() {
            return;
        }
        self.drag_source_destroyed(source);
        if self.data.selection == Some(source) {
            self.data.selection = None;
            self.cancel_clipboard_capture();
            self.offer_selection_to_keyboard();
        }
    }

    /// The bytes of MIME types a client's data sources hold.
    pub(crate) fn mime_bytes_held(&self, client_id: ClientId) -> usize {
        self.data
            .sources
            .values()
            .filter(|source| source.client == client_id)
            .flat_map(|source| source.mime_types.iter().map(String::len))
            .sum()
    }

    /// The client whose surface has the keyboard.
    pub(crate) fn keyboard_client(&self) -> Option<ClientId> {
        self.seat
            .keyboard_focus
            .and_then(|focus| self.surfaces.get(&focus))
            .map(|surface| surface.client)
    }

    /// The selection changed: the client with the keyboard, if one has it,
    /// is offered the new one.
    fn offer_selection_to_keyboard(&mut self) {
        if let Some(client_id) = self.keyboard_client() {
            self.offer_selection(client_id);
        }
    }

    /// Offers a client the selection, or none, on each of its data
    /// devices: as it gains the keyboard, and when the selection changes
    /// while it holds it.
    pub(crate) fn offer_selection(&mut self, client_id: ClientId) {
        let devices: Vec<u32> = self.clients.get(&client_id).map_or(Vec::new(), |client| {
            client
                .objects
                .iter()
                .filter(|(_, object)| matches!(object.kind, ObjectKind::DataDevice))
                .map(|(id, _)| *id)
                .collect()
        });
        for device in devices {
            self.offer_selection_on(client_id, device);
        }
    }

    fn offer_selection_on(&mut self, client_id: ClientId, device: u32) {
        let source = self
            .data
            .selection
            .filter(|source| self.data.sources.contains_key(source));
        let offer = source.and_then(|source| self.make_offer(client_id, device, source, None));
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.send(
                device,
                wl_data_device::Event::Selection {
                    id: offer.map(|(_, object)| ObjectId(object)),
                },
            );
        }
    }

    /// A new offer of a source, introduced on a data device with the MIME
    /// types the source offers.
    pub(crate) fn make_offer(
        &mut self,
        client_id: ClientId,
        device: u32,
        source: SourceId,
        drag: Option<DragOffer>,
    ) -> Option<(OfferId, u32)> {
        let held = self
            .data
            .offers
            .values()
            .filter(|offer| offer.client == client_id)
            .count();
        if held >= CLIENT_OFFERS {
            return None;
        }
        let mime_types = self.data.sources.get(&source)?.mime_types.clone();
        let client = self.clients.get_mut(&client_id)?;
        let version = client.objects.get(&device)?.version;
        self.data.next_offer += 1;
        let offer = OfferId(self.data.next_offer);
        let id = client
            .create_server_object(
                Cow::Borrowed(wl_data_offer::NAME),
                Some(&wl_data_offer::INFO),
                version,
                ObjectKind::DataOffer(offer),
            )
            .ok()?;
        client.send(device, wl_data_device::Event::DataOffer { id: NewId(id) });
        for mime_type in mime_types {
            client.send(id, wl_data_offer::Event::Offer { mime_type });
        }
        self.data.offers.insert(
            offer,
            Offer {
                client: client_id,
                version,
                source,
                drag,
            },
        );
        Some((offer, id))
    }

    pub(crate) fn data_offer_request(
        &mut self,
        object: u32,
        offer: OfferId,
        request: wl_data_offer::Request,
    ) -> Result<(), ProtocolError> {
        match request {
            wl_data_offer::Request::Receive { mime_type, fd } => {
                // The source sends its data, if it is still there and offers
                // that type. Otherwise the descriptor closes, and the reader
                // reads nothing.
                let Some(offer) = self.data.offers.get(&offer) else {
                    return Ok(());
                };
                if offer.drag.as_ref().is_some_and(DragOffer::finished) {
                    return Err(ProtocolError {
                        object,
                        code: wl_data_offer::error::INVALID_OFFER,
                        message: "the drop has finished".into(),
                    });
                }
                if offer.drag.as_ref().is_some_and(|drag| !drag.can_receive()) {
                    return Ok(());
                }
                let Some(data) = self.data.sources.get(&offer.source) else {
                    return Ok(());
                };
                if !data.mime_types.contains(&mime_type) {
                    return Ok(());
                }
                let (client_id, source_object) = (data.client, data.object);
                // The descriptor goes on to the source's client, and waits
                // there until that client reads it, where refusing the app
                // that sent it cannot take it back. So it is passed on only
                // while that leaves the shell's reserve whole.
                self.may_hold_descriptor(offer.client).map_err(|over| ProtocolError {
                    object: 1,
                    code: fulcrum_wire::protocol::wayland::wl_display::error::NO_MEMORY,
                    message: format!("no paste: {over}"),
                })?;
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.send(source_object, wl_data_source::Event::Send { mime_type, fd });
                }
            }
            wl_data_offer::Request::Destroy => self.forget_offer(offer),
            wl_data_offer::Request::Accept { mime_type, .. } => self.drag_accept(object, offer, mime_type)?,
            wl_data_offer::Request::Finish => self.drag_finish(object, offer)?,
            wl_data_offer::Request::SetActions {
                dnd_actions,
                preferred_action,
            } => {
                self.drag_actions(object, offer, dnd_actions, preferred_action)?;
            }
        }
        Ok(())
    }

    /// Lets go of an offer, when it is destroyed or its client is gone.
    pub(crate) fn forget_offer(&mut self, offer: OfferId) {
        if let Some(removed) = self.data.offers.remove(&offer) {
            self.drag_offer_destroyed(removed);
        }
    }
}
