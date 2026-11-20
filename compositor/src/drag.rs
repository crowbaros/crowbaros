//! The pointer owns one drag until its implicit grab ends. An offer belongs
//! to one visit to a target, and a successful drop lets that offer outlive
//! the grab while the destination finishes reading.

use fulcrum_wire::{
    Fixed, ObjectId,
    protocol::wayland::{wl_data_device, wl_data_device_manager::dnd_action, wl_data_offer, wl_data_source},
};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    data_device::{EVERY_ACTION, Offer, OfferId, SourceId},
    surface::{Role, SurfaceId},
};

pub(crate) struct DragStart {
    pub source: Option<ObjectId>,
    pub origin: ObjectId,
    pub icon: Option<ObjectId>,
    pub serial: u32,
}

#[derive(Debug)]
pub(crate) struct Drag {
    client: ClientId,
    origin: SurfaceId,
    source: Option<SourceId>,
    icon: Option<SurfaceId>,
    target: Option<Target>,
}

#[derive(Clone, Copy, Debug)]
struct Target {
    surface: SurfaceId,
    client: ClientId,
    device: u32,
    offer: Option<OfferId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Hover,
    Left,
    Dropped,
    Ask,
    Finished,
}

#[derive(Debug)]
pub(crate) struct DragOffer {
    phase: Phase,
    accepted: bool,
    action: u32,
}

impl DragOffer {
    pub(crate) fn can_receive(&self) -> bool {
        matches!(self.phase, Phase::Hover | Phase::Dropped | Phase::Ask)
    }

    pub(crate) fn finished(&self) -> bool {
        self.phase == Phase::Finished
    }
}

impl State {
    pub(crate) fn start_drag(
        &mut self,
        client: ClientId,
        device: u32,
        request: DragStart,
    ) -> Result<(), ProtocolError> {
        let origin = self
            .surface_of(client, request.origin)
            .expect("the origin is a surface");
        let icon = request
            .icon
            .map(|icon| self.surface_of(client, icon).expect("the icon is a surface"));
        if let Some(icon) = icon
            && !matches!(self.surfaces[&icon].role, Role::None | Role::DragIcon)
        {
            return Err(ProtocolError {
                object: device,
                code: wl_data_device::error::ROLE,
                message: "the drag icon already has another role".into(),
            });
        }
        let source = request
            .source
            .map(|id| self.use_source(client, device, id))
            .transpose()?;
        let starts = self.clients.contains_key(&client)
            && !self.locked()
            && self.data.drag.is_none()
            && self.grab.is_none()
            && self.has_drag_press(origin, request.serial);
        if !starts {
            if let Some(source) = source {
                self.send_source(source, 3, wl_data_source::Event::Cancelled);
            }
            return Ok(());
        };
        if let Some(icon) = icon {
            self.assign_drag_icon(icon);
        }
        self.leave_pointer_focus();
        self.data.drag = Some(Drag {
            client,
            origin,
            source,
            icon,
            target: None,
        });
        self.refresh_pointer_focus();
        self.queue_redraw_all();
        Ok(())
    }

    pub(crate) fn drag_icon(&self) -> Option<SurfaceId> {
        self.data.drag.as_ref()?.icon
    }

    pub(crate) fn drag_client(&self) -> Option<ClientId> {
        self.data.drag.as_ref().map(|drag| drag.client)
    }

    pub(crate) fn drag_motion(&mut self, time: u32) {
        let Some(drag) = self.data.drag.as_ref() else {
            return;
        };
        let (x, y) = self.pointer;
        let hit = self.surface_under(x, y).filter(|(surface, _)| {
            let client = self.surfaces[surface].client;
            drag.source.is_some() || client == drag.client
        });
        let old = drag.target;
        let source = drag.source;
        if let Some(old) = old {
            if let Some((surface, (ox, oy))) = hit
                && surface == old.surface
            {
                self.send_drag_device(
                    old,
                    wl_data_device::Event::Motion {
                        time,
                        x: Fixed::from_f64(x - ox),
                        y: Fixed::from_f64(y - oy),
                    },
                );
                return;
            }
            self.leave_drag_target(old, true);
        }
        let target = hit.and_then(|(surface, (ox, oy))| self.enter_drag_target(surface, (x - ox, y - oy), source));
        if let Some(drag) = self.data.drag.as_mut() {
            drag.target = target;
        }
    }

    fn enter_drag_target(
        &mut self,
        surface: SurfaceId,
        (x, y): (f64, f64),
        source: Option<SourceId>,
    ) -> Option<Target> {
        let surface_state = self.surfaces.get(&surface)?;
        let (client, object) = (surface_state.client, surface_state.object);
        let (&device, data_device) = self
            .clients
            .get(&client)?
            .objects
            .iter()
            .filter(|(_, object)| matches!(object.kind, ObjectKind::DataDevice))
            .min_by_key(|(id, _)| *id)?;
        let version = data_device.version;
        let (offer, offer_object) = if let Some(source) = source {
            let actions = self.source_actions(source)?;
            let initial = if version < 3 { actions & dnd_action::COPY } else { 0 };
            let (offer, object) = self.make_offer(
                client,
                device,
                source,
                Some(DragOffer {
                    phase: Phase::Hover,
                    accepted: false,
                    action: initial,
                }),
            )?;
            if version >= 3 {
                self.clients.get_mut(&client)?.send(
                    object,
                    wl_data_offer::Event::SourceActions {
                        source_actions: actions,
                    },
                );
            } else {
                self.send_source(source, 3, wl_data_source::Event::Action { dnd_action: initial });
            }
            (Some(offer), Some(ObjectId(object)))
        } else {
            (None, None)
        };
        let target = Target {
            surface,
            client,
            device,
            offer,
        };
        let serial = self.next_serial();
        self.send_drag_device(
            target,
            wl_data_device::Event::Enter {
                serial,
                surface: ObjectId(object),
                x: Fixed::from_f64(x),
                y: Fixed::from_f64(y),
                id: offer_object,
            },
        );
        Some(target)
    }

    fn send_drag_device(&mut self, target: Target, event: wl_data_device::Event) {
        if let Some(client) = self.clients.get_mut(&target.client) {
            client.send(target.device, event);
        }
    }

    fn source_actions(&self, source: SourceId) -> Option<u32> {
        self.data
            .sources
            .get(&source)
            .map(|data| data.actions.unwrap_or(dnd_action::COPY))
    }

    fn leave_drag_target(&mut self, target: Target, notify: bool) {
        if notify {
            self.send_drag_device(target, wl_data_device::Event::Leave);
        }
        if let Some(offer) = target.offer.and_then(|id| self.data.offers.get_mut(&id))
            && let Some(drag) = offer.drag.as_mut()
        {
            drag.phase = Phase::Left;
            let (source, selected) = (offer.source, drag.action);
            self.send_source(source, 1, wl_data_source::Event::Target { mime_type: None });
            if selected != 0 {
                self.send_source(source, 3, wl_data_source::Event::Action { dnd_action: 0 });
            }
        }
    }

    pub(crate) fn drop_drag(&mut self) {
        let Some(drag) = self.data.drag.take() else {
            return;
        };
        let drop = drag.target.filter(|target| {
            if drag.source.is_none() {
                return true;
            }
            let Some(offer) = target.offer.and_then(|id| self.data.offers.get_mut(&id)) else {
                return false;
            };
            let Some(offer_drag) = offer.drag.as_mut() else {
                return false;
            };
            if offer_drag.action == 0 || (offer.version >= 3 && !offer_drag.accepted) {
                return false;
            }
            offer_drag.phase = if offer_drag.action == dnd_action::ASK {
                Phase::Ask
            } else {
                Phase::Dropped
            };
            true
        });
        if let Some(target) = drop {
            self.send_drag_device(target, wl_data_device::Event::Drop);
            if let Some(source) = drag.source {
                self.send_source(source, 3, wl_data_source::Event::DndDropPerformed);
            }
        } else {
            self.cancel_taken_drag(drag);
        }
        self.reset_cursor();
        self.queue_redraw_all();
    }

    pub(crate) fn cancel_drag(&mut self) {
        if let Some(drag) = self.data.drag.take() {
            self.cancel_taken_drag(drag);
            self.reset_cursor();
            self.queue_redraw_all();
        }
    }

    fn cancel_taken_drag(&mut self, drag: Drag) {
        if let Some(target) = drag.target {
            self.leave_drag_target(target, true);
        }
        if let Some(source) = drag.source {
            self.send_source(source, 3, wl_data_source::Event::Cancelled);
        }
    }

    pub(crate) fn drag_source_destroyed(&mut self, source: SourceId) {
        if self.data.drag.as_ref().is_some_and(|drag| drag.source == Some(source)) {
            self.cancel_drag();
        }
    }

    pub(crate) fn drag_device_released(&mut self, client: ClientId, device: u32) {
        let Some(drag) = self.data.drag.as_mut() else {
            return;
        };
        if let Some(target) = drag
            .target
            .filter(|target| target.client == client && target.device == device)
        {
            drag.target = None;
            self.leave_drag_target(target, false);
        }
    }

    pub(crate) fn drag_surface_unmapped(&mut self, surface: SurfaceId) {
        if self
            .seat
            .drag_press
            .is_some_and(|(origin, _)| self.in_drag_subtree(origin, surface))
        {
            self.seat.drag_press = None;
        }
        let Some(drag) = self.data.drag.as_ref() else {
            return;
        };
        if self.in_drag_subtree(drag.origin, surface) {
            self.cancel_drag();
        } else if let Some(target) = drag.target
            && self.in_drag_subtree(target.surface, surface)
        {
            self.data.drag.as_mut().expect("a drag is active").target = None;
            self.leave_drag_target(target, true);
        }
    }

    fn in_drag_subtree(&self, mut member: SurfaceId, root: SurfaceId) -> bool {
        for _ in 0..=crate::surface::MAX_SUBSURFACE_DEPTH {
            if member == root {
                return true;
            }
            let Some(parent) = self.surfaces.get(&member).and_then(|surface| surface.parent) else {
                break;
            };
            member = parent;
        }
        false
    }

    pub(crate) fn drag_surface_destroyed(&mut self, surface: SurfaceId) {
        self.drag_surface_unmapped(surface);
        if let Some(drag) = self.data.drag.as_mut()
            && drag.icon == Some(surface)
        {
            drag.icon = None;
            self.queue_redraw_all();
        }
    }

    pub(crate) fn drag_accept(&mut self, object: u32, id: OfferId, mime: Option<String>) -> Result<(), ProtocolError> {
        let Some(offer) = self.data.offers.get_mut(&id) else {
            return Ok(());
        };
        let Some(drag) = offer.drag.as_mut() else {
            return Ok(());
        };
        if drag.phase == Phase::Finished {
            return Err(fail(
                object,
                wl_data_offer::error::INVALID_OFFER,
                "the drop has finished",
            ));
        }
        if !matches!(drag.phase, Phase::Hover | Phase::Ask) {
            return Ok(());
        }
        let source = offer.source;
        let Some(data) = self.data.sources.get(&source) else {
            return Ok(());
        };
        let mime_type = mime.filter(|mime| data.mime_types.contains(mime));
        drag.accepted = mime_type.is_some();
        self.send_source(source, 1, wl_data_source::Event::Target { mime_type });
        Ok(())
    }

    pub(crate) fn drag_actions(
        &mut self,
        object: u32,
        id: OfferId,
        actions: u32,
        preferred: u32,
    ) -> Result<(), ProtocolError> {
        let Some(offer) = self.data.offers.get(&id) else {
            return Ok(());
        };
        let Some(drag) = offer.drag.as_ref() else {
            return Err(fail(
                object,
                wl_data_offer::error::INVALID_OFFER,
                "actions are for a drag's offer",
            ));
        };
        if actions & !EVERY_ACTION != 0 {
            return Err(fail(
                object,
                wl_data_offer::error::INVALID_ACTION_MASK,
                "unknown drag action bits",
            ));
        }
        if preferred.count_ones() > 1 || preferred & actions != preferred {
            return Err(fail(
                object,
                wl_data_offer::error::INVALID_ACTION,
                "preferred action must be one supported action",
            ));
        }
        let (phase, previous, source, client) = (drag.phase, drag.action, offer.source, offer.client);
        let Some(source_actions) = self.source_actions(source) else {
            return Ok(());
        };
        let shared = actions & source_actions;
        let action = match phase {
            Phase::Hover if preferred & shared != 0 => preferred,
            Phase::Hover => [dnd_action::COPY, dnd_action::MOVE, dnd_action::ASK]
                .into_iter()
                .find(|action| shared & action != 0)
                .unwrap_or(0),
            Phase::Ask if matches!(preferred, dnd_action::COPY | dnd_action::MOVE) && preferred & shared != 0 => {
                preferred
            }
            Phase::Ask => {
                return Err(fail(
                    object,
                    wl_data_offer::error::INVALID_ACTION,
                    "choose a shared copy or move action",
                ));
            }
            Phase::Finished => {
                return Err(fail(
                    object,
                    wl_data_offer::error::INVALID_OFFER,
                    "the drop has finished",
                ));
            }
            Phase::Left | Phase::Dropped => return Ok(()),
        };
        self.data
            .offers
            .get_mut(&id)
            .and_then(|offer| offer.drag.as_mut())
            .expect("the drag offer is live")
            .action = action;
        if phase == Phase::Hover && action != previous {
            if let Some(client) = self.clients.get_mut(&client) {
                client.send(object, wl_data_offer::Event::Action { dnd_action: action });
            }
            self.send_source(source, 3, wl_data_source::Event::Action { dnd_action: action });
        }
        Ok(())
    }

    pub(crate) fn drag_finish(&mut self, object: u32, id: OfferId) -> Result<(), ProtocolError> {
        let Some(offer) = self.data.offers.get_mut(&id) else {
            return Ok(());
        };
        let Some(drag) = offer.drag.as_mut().filter(|drag| {
            matches!(drag.phase, Phase::Dropped | Phase::Ask)
                && drag.accepted
                && matches!(drag.action, dnd_action::COPY | dnd_action::MOVE)
        }) else {
            return Err(fail(
                object,
                wl_data_offer::error::INVALID_FINISH,
                "finish needs an accepted drop and a final action",
            ));
        };
        let (resolved_ask, action, source) = (drag.phase == Phase::Ask, drag.action, offer.source);
        drag.phase = Phase::Finished;
        if resolved_ask {
            self.send_source(source, 3, wl_data_source::Event::Action { dnd_action: action });
        }
        self.send_source(source, 3, wl_data_source::Event::DndFinished);
        Ok(())
    }

    pub(crate) fn drag_offer_destroyed(&mut self, offer: Offer) {
        let Some(drag) = offer.drag else {
            return;
        };
        match drag.phase {
            Phase::Dropped if offer.version < 3 => {
                self.send_source(offer.source, 3, wl_data_source::Event::DndFinished)
            }
            Phase::Dropped | Phase::Ask => self.send_source(offer.source, 3, wl_data_source::Event::Cancelled),
            Phase::Hover if drag.accepted || drag.action != 0 => {
                self.send_source(offer.source, 1, wl_data_source::Event::Target { mime_type: None });
                self.send_source(offer.source, 3, wl_data_source::Event::Action { dnd_action: 0 });
            }
            _ => {}
        }
    }
}

fn fail(object: u32, code: u32, message: &str) -> ProtocolError {
    ProtocolError {
        object,
        code,
        message: message.into(),
    }
}
