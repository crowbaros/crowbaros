//! What one client, and one compartment, may make the compositor hold.
//!
//! Every connection and every shared-memory pool costs the compositor a
//! descriptor, and the process has a fixed number. Without limits one
//! compartment could take them all, and the compositor would accept nothing
//! more: not another compartment, not the shell, not the Warden (CrowbarOS
//! issue 174). So:
//!
//! - **A reserve stays free for the trusted parts.** A compartment's client
//!   is accepted, and a pool is made, only while more than [`RESERVE`]
//!   descriptors are left. The shell and the Warden are accepted down to the
//!   last one.
//! - **Each compartment has a budget** of [`COMPARTMENT_CONNECTIONS`]
//!   connections and [`COMPARTMENT_DESCRIPTORS`] descriptors in all:
//!   connections, pools, and descriptors a client sent that no message has
//!   taken yet.
//! - **Each client has a budget** of [`CLIENT_POOLS`] pools and
//!   [`CLIENT_POOL_BYTES`] bytes of shared memory.
//!
//! A connection over its compartment's budget is refused: it is accepted and
//! closed at once, so it waits in no queue. A pool over a budget is a
//! protocol error that ends its client. Nothing over a budget harms another
//! client.
//!
//! The compositor raises its own descriptor limit to the hard limit when it
//! starts, so the budgets are a small part of what it has.

use crate::{
    State,
    client::{ClientId, ObjectKind},
    identity::{Compartment, Identity},
};

/// Descriptors kept free for the shell, the Warden, and the compositor's own
/// work.
pub const RESERVE: usize = 64;
/// Connections one compartment may hold open at once.
pub const COMPARTMENT_CONNECTIONS: usize = 16;
/// Descriptors one compartment may make the compositor hold.
pub const COMPARTMENT_DESCRIPTORS: usize = 512;
/// Shared-memory pools one client may hold.
pub const CLIENT_POOLS: usize = 128;
/// Bytes of shared memory one client may hold mapped.
pub const CLIENT_POOL_BYTES: usize = 512 * 1024 * 1024;

/// The descriptors this process may hold, as its soft limit says.
pub fn descriptor_limit() -> usize {
    let limit = rustix::process::getrlimit(rustix::process::Resource::Nofile);
    limit.current.map_or(usize::MAX, |current| current as usize)
}

/// Raises this process's soft descriptor limit to its hard limit. The
/// compositor calls it once, before it listens.
pub fn raise_descriptor_limit() {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let limit = getrlimit(Resource::Nofile);
    if limit.current != limit.maximum {
        let _ = setrlimit(
            Resource::Nofile,
            Rlimit {
                current: limit.maximum,
                maximum: limit.maximum,
            },
        );
    }
}

/// Why something was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverBudget {
    Reserve,
    Connections,
    Descriptors,
    Pools,
    PoolBytes,
}

impl std::fmt::Display for OverBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OverBudget::Reserve => write!(f, "fewer than {RESERVE} descriptors are left"),
            OverBudget::Connections => write!(f, "its compartment has {COMPARTMENT_CONNECTIONS} connections"),
            OverBudget::Descriptors => {
                write!(f, "its compartment holds {COMPARTMENT_DESCRIPTORS} descriptors")
            }
            OverBudget::Pools => write!(f, "the client holds {CLIENT_POOLS} pools"),
            OverBudget::PoolBytes => write!(f, "the client holds {CLIENT_POOL_BYTES} bytes of shared memory"),
        }
    }
}

impl State {
    /// The descriptors the compositor holds now: its own, its listeners, its
    /// connections, its pools, and descriptors waiting in connections.
    pub fn descriptors_held(&self) -> usize {
        // The event loop's epoll and its waker.
        let own = 2;
        let listeners = self.trusted.len() + 2 * self.listeners.len();
        let connections: usize = self.clients.values().map(|client| 1 + client.conn.queued_fds()).sum();
        own + listeners + connections + self.pools.len()
    }

    fn descriptors_left(&self) -> usize {
        self.descriptor_limit.saturating_sub(self.descriptors_held())
    }

    fn compartment_clients(&self, compartment: &Compartment) -> impl Iterator<Item = ClientId> + '_ {
        let id = compartment.id().to_string();
        self.clients.values().filter_map(move |client| match &client.identity {
            Identity::Compartment(of) if of.id() == id => Some(client.id),
            _ => None,
        })
    }

    /// The descriptors one compartment makes the compositor hold.
    pub(crate) fn compartment_descriptors(&self, compartment: &Compartment) -> usize {
        self.compartment_clients(compartment)
            .map(|id| {
                let client = &self.clients[&id];
                1 + client.conn.queued_fds()
                    + client
                        .objects
                        .values()
                        .filter(|object| matches!(object.kind, ObjectKind::Pool(_)))
                        .count()
            })
            .sum()
    }

    /// Whether one more connection may be accepted for a compartment.
    pub(crate) fn may_connect(&self, compartment: &Compartment) -> Result<(), OverBudget> {
        if self.descriptors_left() <= RESERVE {
            return Err(OverBudget::Reserve);
        }
        if self.compartment_clients(compartment).count() >= COMPARTMENT_CONNECTIONS {
            return Err(OverBudget::Connections);
        }
        if self.compartment_descriptors(compartment) >= COMPARTMENT_DESCRIPTORS {
            return Err(OverBudget::Descriptors);
        }
        Ok(())
    }

    /// Whether a client may hold one more pool of `bytes`.
    pub(crate) fn may_make_pool(&self, client_id: ClientId, bytes: usize) -> Result<(), OverBudget> {
        let Some(client) = self.clients.get(&client_id) else {
            return Ok(());
        };
        let trusted = !matches!(client.identity, Identity::Compartment(_));
        if !trusted && self.descriptors_left() <= RESERVE {
            return Err(OverBudget::Reserve);
        }
        let pools: Vec<_> = self
            .pools
            .iter()
            .filter(|(_, pool)| pool.client == client_id)
            .map(|(_, pool)| pool.size())
            .collect();
        if pools.len() >= CLIENT_POOLS {
            return Err(OverBudget::Pools);
        }
        if pools.iter().sum::<usize>().saturating_add(bytes) > CLIENT_POOL_BYTES {
            return Err(OverBudget::PoolBytes);
        }
        if let Identity::Compartment(compartment) = &client.identity
            && self.compartment_descriptors(compartment) >= COMPARTMENT_DESCRIPTORS
        {
            return Err(OverBudget::Descriptors);
        }
        Ok(())
    }

    /// Whether a client's pools may grow by `bytes`.
    pub(crate) fn may_grow_pool(&self, client_id: ClientId, bytes: usize) -> Result<(), OverBudget> {
        let held: usize = self
            .pools
            .values()
            .filter(|pool| pool.client == client_id)
            .map(|pool| pool.size())
            .sum();
        if held.saturating_add(bytes) > CLIENT_POOL_BYTES {
            return Err(OverBudget::PoolBytes);
        }
        Ok(())
    }

    /// Whether a compartment's client holds more descriptors than its
    /// compartment's budget, as by sending descriptors no message takes.
    pub(crate) fn over_descriptor_budget(&self, client_id: ClientId) -> bool {
        match self.clients.get(&client_id).map(|client| &client.identity) {
            Some(Identity::Compartment(compartment)) => {
                self.compartment_descriptors(compartment) > COMPARTMENT_DESCRIPTORS
            }
            _ => false,
        }
    }
}
