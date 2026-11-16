//! Who a client is.
//!
//! An identity is fixed when the compositor accepts the connection, from
//! which listener the client reached and, for a trusted part, the user ID the
//! kernel reports. Nothing a client says about itself changes it (ADR 0016).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use smithay::{
    reexports::wayland_server::{
        Client,
        backend::{ClientData, ClientId, DisconnectReason},
    },
    wayland::compositor::CompositorClientState,
};

/// Who a client is.
#[derive(Clone, Debug)]
pub enum Identity {
    /// The shell. It draws the trusted UI other than frames.
    Shell,
    /// The Warden. It registers each compartment's listener and draws nothing.
    Warden,
    /// A client that came through the listener the Warden registered for one
    /// compartment.
    Compartment(Arc<Compartment>),
}

/// One compartment, as the Warden named it when it registered the
/// compartment's listener. Every client of that listener shares it.
#[derive(Debug)]
pub struct Compartment {
    principal: String,
    id: String,
    gpu: AtomicBool,
}

impl Compartment {
    pub(crate) fn new(principal: String, id: String) -> Self {
        Compartment {
            principal,
            id,
            gpu: AtomicBool::new(false),
        }
    }

    /// The principal ID: stable, and what authority binds to.
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// The compartment ID: new at each start and never used twice.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether this compartment's stated needs include the GPU.
    pub fn needs_gpu(&self) -> bool {
        self.gpu.load(Ordering::Relaxed)
    }

    /// Records that this compartment's stated needs include the GPU. It
    /// decides what a client is offered when it connects, so it must be said
    /// before the compartment's program starts.
    ///
    /// The Warden says who a compartment is and nothing more (ADR 0016), so
    /// this does not come from the Warden. No part of Base says it yet: the
    /// tests call this in its place.
    pub fn grant_gpu(&self) {
        self.gpu.store(true, Ordering::Relaxed);
    }
}

/// What the compositor keeps for each connected client.
pub struct ClientState {
    pub(crate) compositor: CompositorClientState,
    identity: Identity,
}

impl ClientState {
    pub(crate) fn new(identity: Identity) -> Self {
        ClientState {
            compositor: CompositorClientState::default(),
            identity,
        }
    }
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// The identity of a connected client. A client the compositor did not accept
/// through [`crate::connections`] has none, and is offered nothing.
pub fn of(client: &Client) -> Option<&Identity> {
    client.get_data::<ClientState>().map(|state| &state.identity)
}

/// The same, from the data the compositor attached to a client.
pub(crate) fn of_data(data: &dyn ClientData) -> Option<&Identity> {
    data.downcast_ref::<ClientState>().map(|state| &state.identity)
}
