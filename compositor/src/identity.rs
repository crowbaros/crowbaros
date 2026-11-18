//! Who a client is.
//!
//! An identity is fixed when the compositor accepts the connection. It comes
//! from which listener the client reached and, for a trusted part, the user ID
//! the kernel reports. Nothing a client says about itself changes it
//! (ADR 0016).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
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
    /// A client of the compartment's listener has connected. What a client
    /// is offered is fixed when it connects, so the GPU need cannot be
    /// recorded after this.
    had_client: AtomicBool,
}

impl Compartment {
    pub(crate) fn new(principal: String, id: String) -> Self {
        Compartment {
            principal,
            id,
            gpu: AtomicBool::new(false),
            had_client: AtomicBool::new(false),
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
    /// decides what a client is offered when it connects, so it is refused
    /// once a client has connected.
    ///
    /// The Warden says who a compartment is and nothing more (ADR 0016), and
    /// nothing a client says reaches this. The broker knows a principal's
    /// needs, and tells the compositor over its control channel from
    /// Milestone 4. Until then only the tests call this, in its place.
    pub fn record_gpu_need(&self) -> Result<(), String> {
        if self.had_client() {
            return Err("too late: a client of this compartment has connected".into());
        }
        self.gpu.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Whether a client of this compartment's listener has connected.
    pub fn had_client(&self) -> bool {
        self.had_client.load(Ordering::Relaxed)
    }

    pub(crate) fn client_connected(&self) {
        self.had_client.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gpu_need_cannot_be_recorded_after_a_client_connects() {
        let compartment = Compartment::new("p".into(), "c".into());
        compartment.client_connected();
        assert!(compartment.record_gpu_need().is_err());
        assert!(!compartment.needs_gpu());
    }
}
