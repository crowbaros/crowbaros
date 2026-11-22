//! The primary selection: `zwp_primary_selection_device_manager_v1`, what a
//! person selects and pastes with the middle button.
//!
//! Every client is offered it, under the clipboard's rules (see
//! `data_device.rs`). It works between the shell and every app, as on any
//! desktop: there is one primary selection, and the client that holds the
//! keyboard is offered it, as it gains the keyboard and when the selection
//! changes.

use std::{borrow::Cow, collections::HashMap};

use fulcrum_wire::{
    NewId, ObjectId,
    protocol::primary_selection_unstable_v1::{
        zwp_primary_selection_device_manager_v1, zwp_primary_selection_device_v1, zwp_primary_selection_offer_v1,
        zwp_primary_selection_source_v1,
    },
};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    data_device::{CLIENT_MIME_BYTES, CLIENT_OFFERS, SOURCE_MIME_TYPES},
};

/// The `zwp_primary_selection_device_manager_v1` version Fulcrum offers.
pub const PRIMARY_SELECTION_VERSION: u32 = 1;

pub(crate) type PrimarySourceId = u64;
pub(crate) type PrimaryOfferId = u64;

#[derive(Debug)]
pub(crate) struct PrimarySource {
    client: ClientId,
    object: u32,
    mime_types: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct PrimarySelections {
    sources: HashMap<PrimarySourceId, PrimarySource>,
    /// The primary selection.
    selection: Option<PrimarySourceId>,
    /// Each offer, and the source it offers.
    offers: HashMap<PrimaryOfferId, (ClientId, PrimarySourceId)>,
    next: u64,
}

impl State {
    pub(crate) fn primary_manager_request(
        &mut self,
        client_id: ClientId,
        version: u32,
        request: zwp_primary_selection_device_manager_v1::Request,
    ) -> Result<(), ProtocolError> {
        match request {
            zwp_primary_selection_device_manager_v1::Request::CreateSource { id } => {
                self.primary.next += 1;
                let source = self.primary.next;
                self.create_primary_object(
                    client_id,
                    id,
                    zwp_primary_selection_source_v1::NAME,
                    version,
                    ObjectKind::PrimarySource(source),
                )?;
                self.primary.sources.insert(
                    source,
                    PrimarySource {
                        client: client_id,
                        object: id.0,
                        mime_types: Vec::new(),
                    },
                );
            }
            zwp_primary_selection_device_manager_v1::Request::GetDevice { id, .. } => {
                self.create_primary_object(
                    client_id,
                    id,
                    zwp_primary_selection_device_v1::NAME,
                    version,
                    ObjectKind::PrimaryDevice,
                )?;
                if self.keyboard_client() == Some(client_id) {
                    self.offer_primary_on(client_id, id.0);
                }
            }
            zwp_primary_selection_device_manager_v1::Request::Destroy => {}
        }
        Ok(())
    }

    fn create_primary_object(
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

    pub(crate) fn primary_source_request(
        &mut self,
        client_id: ClientId,
        source: PrimarySourceId,
        request: zwp_primary_selection_source_v1::Request,
    ) -> Result<(), ProtocolError> {
        match request {
            zwp_primary_selection_source_v1::Request::Offer { mime_type } => {
                let held = self.mime_bytes_held(client_id) + self.primary_mime_bytes_held(client_id);
                let Some(data) = self.primary.sources.get_mut(&source) else {
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
            zwp_primary_selection_source_v1::Request::Destroy => self.forget_primary_source(source),
        }
        Ok(())
    }

    pub(crate) fn primary_device_request(
        &mut self,
        client_id: ClientId,
        request: zwp_primary_selection_device_v1::Request,
    ) -> Result<(), ProtocolError> {
        let zwp_primary_selection_device_v1::Request::SetSelection { source, .. } = request else {
            return Ok(());
        };
        let new = source.and_then(|source| self.primary_source_of(client_id, source));
        if !self.clients.contains_key(&client_id) {
            if let Some(new) = new {
                self.cancel_primary_source(new);
            }
            return Ok(());
        }
        let old = std::mem::replace(&mut self.primary.selection, new);
        if let Some(old) = old.filter(|old| Some(*old) != new) {
            self.cancel_primary_source(old);
        }
        if let Some(client_id) = self.keyboard_client() {
            self.offer_primary_selection(client_id);
        }
        Ok(())
    }

    fn primary_source_of(&self, client_id: ClientId, object: ObjectId) -> Option<PrimarySourceId> {
        match self.clients.get(&client_id)?.objects.get(&object.0)?.kind {
            ObjectKind::PrimarySource(source) => Some(source),
            _ => None,
        }
    }

    pub(crate) fn primary_offer_request(
        &mut self,
        offer: PrimaryOfferId,
        request: zwp_primary_selection_offer_v1::Request,
    ) -> Result<(), ProtocolError> {
        match request {
            zwp_primary_selection_offer_v1::Request::Receive { mime_type, fd } => {
                let Some((receiver, source)) = self.primary.offers.get(&offer).copied() else {
                    return Ok(());
                };
                let Some(data) = self.primary.sources.get(&source) else {
                    return Ok(());
                };
                if !data.mime_types.contains(&mime_type) {
                    return Ok(());
                }
                let (client_id, object) = (data.client, data.object);
                // As for the selection: the descriptor is passed on to the
                // source's client only while that leaves the shell's reserve
                // whole (see `data_device.rs`).
                self.may_hold_descriptor(receiver).map_err(|over| ProtocolError {
                    object: 1,
                    code: fulcrum_wire::protocol::wayland::wl_display::error::NO_MEMORY,
                    message: format!("no paste: {over}"),
                })?;
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.send(object, zwp_primary_selection_source_v1::Event::Send { mime_type, fd });
                }
            }
            zwp_primary_selection_offer_v1::Request::Destroy => self.forget_primary_offer(offer),
        }
        Ok(())
    }

    /// Offers a client the primary selection, or none, on each of its
    /// primary selection devices.
    pub(crate) fn offer_primary_selection(&mut self, client_id: ClientId) {
        let devices: Vec<u32> = self.clients.get(&client_id).map_or(Vec::new(), |client| {
            client
                .objects
                .iter()
                .filter(|(_, object)| matches!(object.kind, ObjectKind::PrimaryDevice))
                .map(|(id, _)| *id)
                .collect()
        });
        for device in devices {
            self.offer_primary_on(client_id, device);
        }
    }

    fn offer_primary_on(&mut self, client_id: ClientId, device: u32) {
        let source = self
            .primary
            .selection
            .filter(|source| self.primary.sources.contains_key(source));
        let held = self
            .primary
            .offers
            .values()
            .filter(|(client, _)| *client == client_id)
            .count();
        let offer = match source {
            Some(source) if held < CLIENT_OFFERS => self.make_primary_offer(client_id, device, source),
            _ => None,
        };
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.send(
                device,
                zwp_primary_selection_device_v1::Event::Selection {
                    id: offer.map(ObjectId),
                },
            );
        }
    }

    fn make_primary_offer(&mut self, client_id: ClientId, device: u32, source: PrimarySourceId) -> Option<u32> {
        let mime_types = self.primary.sources.get(&source)?.mime_types.clone();
        let client = self.clients.get_mut(&client_id)?;
        self.primary.next += 1;
        let offer = self.primary.next;
        let id = client
            .create_server_object(
                Cow::Borrowed(zwp_primary_selection_offer_v1::NAME),
                Some(&zwp_primary_selection_offer_v1::INFO),
                1,
                ObjectKind::PrimaryOffer(offer),
            )
            .ok()?;
        client.send(
            device,
            zwp_primary_selection_device_v1::Event::DataOffer { offer: NewId(id) },
        );
        for mime_type in mime_types {
            client.send(id, zwp_primary_selection_offer_v1::Event::Offer { mime_type });
        }
        self.primary.offers.insert(offer, (client_id, source));
        Some(id)
    }

    fn cancel_primary_source(&mut self, source: PrimarySourceId) {
        let Some(data) = self.primary.sources.get(&source) else {
            return;
        };
        if let Some(client) = self.clients.get_mut(&data.client) {
            client.send(data.object, zwp_primary_selection_source_v1::Event::Cancelled);
        }
    }

    /// Lets go of a source, when it is destroyed or its client is gone. If
    /// it was the primary selection there is none, and the client with the
    /// keyboard hears so.
    pub(crate) fn forget_primary_source(&mut self, source: PrimarySourceId) {
        if self.primary.sources.remove(&source).is_none() {
            return;
        }
        if self.primary.selection == Some(source) {
            self.primary.selection = None;
            if let Some(client_id) = self.keyboard_client() {
                self.offer_primary_selection(client_id);
            }
        }
    }

    pub(crate) fn forget_primary_offer(&mut self, offer: PrimaryOfferId) {
        self.primary.offers.remove(&offer);
    }

    /// The bytes of MIME types a client's primary selection sources hold,
    /// counted with its data sources' against one budget.
    pub(crate) fn primary_mime_bytes_held(&self, client_id: ClientId) -> usize {
        self.primary
            .sources
            .values()
            .filter(|source| source.client == client_id)
            .flat_map(|source| source.mime_types.iter().map(String::len))
            .sum()
    }
}
