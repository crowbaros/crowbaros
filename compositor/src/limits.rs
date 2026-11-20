//! What one client may make the compositor hold.
//!
//! Every connection and every shared-memory pool costs the compositor a
//! descriptor, and the process has a fixed number. Without limits the
//! person's apps could take them all, and the compositor would accept
//! nothing more, not even the shell (CrowbarOS issue 174). So:
//!
//! - **A reserve stays free for the shell.** An app is accepted, and an
//!   app's pool is made, only while more than [`RESERVE`] descriptors are
//!   left. The shell is accepted down to the last one.
//! - **Each client has a budget** of [`CLIENT_POOLS`] pools,
//!   [`CLIENT_POOL_BYTES`] bytes of them mapped, and [`CLIENT_PIXEL_BYTES`]
//!   bytes of pixels: the compositor copies each buffer a client commits,
//!   and holds the copy until the next. A connection holds at most
//!   `fulcrum_wire::socket::MAX_FDS_IN` descriptors that no message has taken yet.
//!
//! An app's connection past the reserve is refused: it is accepted and
//! closed at once, so it waits in no queue. A pool or a commit over a budget
//! is a protocol error that ends its client. Nothing over a budget harms
//! another client.
//!
//! A pool costs address space, not memory, until its client writes to it, so
//! the budget of mapped bytes is large. Memory the compositor itself holds
//! for a client is the copies of its buffers: one buffer can be committed
//! many times from the same bytes, so those are budgeted on their own.
//!
//! The compositor raises its own descriptor limit to the hard limit when it
//! starts, so the budgets are a small part of what it has.

use crate::{State, client::ClientId, identity::Identity, shm::Source};

/// Descriptors kept free for the shell and the compositor's own work.
pub const RESERVE: usize = 64;
/// Shared-memory pools one client may hold.
pub const CLIENT_POOLS: usize = 128;
/// Bytes of shared-memory pools one client may hold mapped: address space,
/// room for `foot`'s sparse pools of 512 MiB for a window each.
pub const CLIENT_POOL_BYTES: usize = 8 * 1024 * 1024 * 1024;
/// Bytes of pixels the compositor may hold for one client's surfaces.
pub const CLIENT_PIXEL_BYTES: usize = 512 * 1024 * 1024;

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
    Pools,
    PoolBytes,
    PixelBytes,
}

impl std::fmt::Display for OverBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OverBudget::Reserve => write!(f, "fewer than {RESERVE} descriptors are left"),
            OverBudget::Pools => write!(f, "the client holds {CLIENT_POOLS} pools"),
            OverBudget::PoolBytes => write!(f, "the client holds {CLIENT_POOL_BYTES} bytes of shared memory"),
            OverBudget::PixelBytes => write!(f, "the client's surfaces hold {CLIENT_PIXEL_BYTES} bytes of pixels"),
        }
    }
}

impl State {
    /// The descriptors the compositor holds now: its own, its listeners, its
    /// connections, its pools, and descriptors waiting in connections.
    pub fn descriptors_held(&self) -> usize {
        // The event loop's epoll and its waker.
        let own = 2;
        let listeners = self.listeners.len();
        let connections: usize = self.clients.values().map(|client| 1 + client.conn.queued_fds()).sum();
        let dmabufs = self
            .buffers
            .values()
            .filter(|buffer| matches!(buffer.source, Source::Dmabuf { .. }))
            .count()
            + self
                .dmabufs
                .params
                .keys()
                .map(|params| self.params_descriptors(*params))
                .sum::<usize>();
        own + listeners + connections + self.pools.len() + dmabufs
    }

    fn descriptors_left(&self) -> usize {
        self.descriptor_limit.saturating_sub(self.descriptors_held())
    }

    /// Whether one more client with this identity may be accepted.
    pub(crate) fn may_connect(&self, identity: Identity) -> Result<(), OverBudget> {
        if identity == Identity::App && self.descriptors_left() <= RESERVE {
            return Err(OverBudget::Reserve);
        }
        Ok(())
    }

    /// Whether a client may hold one more pool of `bytes`.
    pub(crate) fn may_make_pool(&self, client_id: ClientId, bytes: usize) -> Result<(), OverBudget> {
        let Some(client) = self.clients.get(&client_id) else {
            return Ok(());
        };
        if client.identity == Identity::App && self.descriptors_left() <= RESERVE {
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
        Ok(())
    }

    /// Whether a client's surfaces may hold `bytes` more of pixels, where a
    /// commit replaces `replaced` bytes.
    pub(crate) fn may_hold_pixels(&self, client_id: ClientId, replaced: usize, bytes: usize) -> Result<(), OverBudget> {
        let held = self.clients.get(&client_id).map_or(0, |client| client.pixel_bytes);
        if held.saturating_sub(replaced).saturating_add(bytes) > self.pixel_budget {
            return Err(OverBudget::PixelBytes);
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
}
