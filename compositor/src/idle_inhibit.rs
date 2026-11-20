//! `zwp_idle_inhibit_manager_v1`: a client asks that the display not go
//! idle while one of its surfaces is shown, as a video player does.
//!
//! An inhibitor counts only while its surface is shown: drawn on an output
//! now, on the workspace the person is on. One whose surface is hidden,
//! unmapped, or gone counts again once the surface is shown again. Idle
//! itself, with blanking and locking, is Milestone 8's, and asks
//! [`State::idle_inhibited`] then. The table offers it to every client
//! (ADR 0032).

use std::{borrow::Cow, collections::HashMap};

use fulcrum_wire::protocol::idle_inhibit_unstable_v1::{zwp_idle_inhibit_manager_v1, zwp_idle_inhibitor_v1};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    scene::Element,
    surface::SurfaceId,
};

/// The `zwp_idle_inhibit_manager_v1` version Fulcrum offers.
pub const IDLE_INHIBIT_VERSION: u32 = 1;

pub(crate) type InhibitorId = u64;

/// Each inhibitor, and the surface it is for.
#[derive(Default)]
pub(crate) struct IdleInhibitors {
    surfaces: HashMap<InhibitorId, SurfaceId>,
}

impl State {
    pub(crate) fn idle_inhibit_manager_request(
        &mut self,
        client_id: ClientId,
        request: zwp_idle_inhibit_manager_v1::Request,
    ) -> Result<(), ProtocolError> {
        let zwp_idle_inhibit_manager_v1::Request::CreateInhibitor { id, surface } = request else {
            return Ok(());
        };
        let surface_id = self
            .surface_of(client_id, surface)
            .expect("the dispatch checked the argument's interface");
        let inhibitor = self.next_resource();
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        client.create(
            id,
            Cow::Borrowed(zwp_idle_inhibitor_v1::NAME),
            Some(&zwp_idle_inhibitor_v1::INFO),
            1,
            ObjectKind::IdleInhibitor(inhibitor),
        )?;
        self.idle_inhibitors.surfaces.insert(inhibitor, surface_id);
        Ok(())
    }

    /// An inhibitor is destroyed, or its client is gone.
    pub(crate) fn forget_inhibitor(&mut self, inhibitor: InhibitorId) {
        self.idle_inhibitors.surfaces.remove(&inhibitor);
    }

    /// The outputs an inhibitor keeps from going idle: those its surface is
    /// drawn on now.
    pub fn idle_inhibited_outputs(&self) -> Vec<u64> {
        let inhibiting: Vec<SurfaceId> = self.idle_inhibitors.surfaces.values().copied().collect();
        if inhibiting.is_empty() {
            return Vec::new();
        }
        self.outputs
            .iter()
            .filter(|output| {
                self.scene(output.number).is_some_and(|scene| {
                    scene.elements.iter().any(
                        |element| matches!(element, Element::Surface { surface, .. } if inhibiting.contains(surface)),
                    )
                })
            })
            .map(|output| output.number)
            .collect()
    }

    /// Whether an inhibitor keeps any output from going idle.
    pub fn idle_inhibited(&self) -> bool {
        !self.idle_inhibited_outputs().is_empty()
    }

    /// How many inhibitors the compositor holds, shown or not.
    #[doc(hidden)]
    pub fn idle_inhibitor_count(&self) -> usize {
        self.idle_inhibitors.surfaces.len()
    }
}
