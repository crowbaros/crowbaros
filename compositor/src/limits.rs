//! What one client may make the compositor hold.
//!
//! Every connection costs the compositor a descriptor, and so does every
//! descriptor a client sends and every one the compositor makes for a
//! client. The process has a fixed number. Without limits the person's apps
//! could take them all, and the compositor would accept nothing more, not
//! even the shell (CrowbarOS issue 174). So:
//!
//! - **A reserve stays free for the shell.** Nothing an app does leaves
//!   fewer than [`RESERVE`] descriptors. The shell is served down to the
//!   last one.
//! - **Each client has a budget** of [`CLIENT_POOLS`] pools,
//!   [`CLIENT_POOL_BYTES`] bytes of them mapped, and [`CLIENT_PIXEL_BYTES`]
//!   bytes of pixels: the compositor copies each buffer a client commits,
//!   and holds the copy until the next. A connection holds at most
//!   `fulcrum_wire::socket::MAX_FDS_IN` descriptors that no message has taken yet.
//!
//! The reserve is kept at every place an app can make the compositor hold
//! one more descriptor:
//!
//! - **A connection.** An app is accepted only while more than the reserve
//!   is left.
//! - **A descriptor the app sends**: a pool's file, a dma-buf's plane, the
//!   pipe a paste is read from, a timeline to import. It is in this process
//!   from the moment it arrives, whether a request then keeps it, passes it
//!   on, or drops it. So it is checked where it arrives, once the requests
//!   that came with it are read, in their order: an app that destroys a
//!   pool no buffer still uses, and makes another, has taken nothing. If what arrived leaves less
//!   than the reserve, and less than before it came, the app is refused
//!   and goes at once, with what its requests took, and what no request
//!   took is closed. A request moves a descriptor from
//!   one count to another and adds none. A request that passes a descriptor
//!   on to another client, as a paste does to the source's, checks the
//!   reserve itself before it does: refusing the sender afterwards would
//!   not take the descriptor back.
//! - **A descriptor the compositor makes for the app**: a keyboard's keymap
//!   and linux-dmabuf's format table, which wait in the app's connection
//!   until the app reads them, and the eventfd a commit waits on for its
//!   acquire point. Each is made only while more than the reserve is left.
//!   An app that asks for a keymap or a table past it is refused. A commit
//!   that cannot wait is applied at once, as where the kernel has no
//!   eventfd to give, and one in a held commit's place takes its eventfd's
//!   place.
//!
//! A timeline takes none: every timeline shares the compositor's one
//! descriptor of the DRM device. The compositor keeps no descriptor of a
//! dma-buf once the GPU has imported it.
//!
//! [`State::descriptors_held`] counts what clients make the compositor
//! hold. What the compositor opens for itself, as its display and input
//! devices and the shell's control channel, and what one request duplicates
//! and closes again before it returns, are not counted: they come out of
//! the reserve too.
//!
//! An app's connection past the reserve is refused: it is accepted and
//! closed at once, so it waits in no queue. Anything else past the reserve
//! or over a budget, but a commit's wait, is a protocol error that ends its
//! client. Nothing over a budget harms another client. The reserve is not a
//! budget: apps share what is above it, so once one app has taken that
//! room, the app refused is the one that asks next.
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
    /// The descriptors the compositor holds now: its event loop's, its
    /// listeners, its connections, the descriptors waiting in connections to
    /// be read as requests or to be sent, its pools, the dma-bufs it reads,
    /// and the eventfds of held commits.
    pub fn descriptors_held(&self) -> usize {
        // The event loop's epoll and its waker.
        let own = 2;
        let listeners = self.listeners.len();
        let connections: usize = self
            .clients
            .values()
            .map(|client| 1 + client.conn.queued_fds() + client.conn.waiting_fds())
            .sum();
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
        let held_commits = self.explicit_sync.held.len();
        own + listeners + connections + self.pools.len() + dmabufs + held_commits
    }

    pub(crate) fn descriptors_left(&self) -> usize {
        self.descriptor_limit.saturating_sub(self.descriptors_held())
    }

    /// Whether one more client with this identity may be accepted.
    pub(crate) fn may_connect(&self, identity: Identity) -> Result<(), OverBudget> {
        if identity == Identity::App && self.descriptors_left() <= RESERVE {
            return Err(OverBudget::Reserve);
        }
        Ok(())
    }

    fn is_app(&self, client_id: ClientId) -> bool {
        self.clients
            .get(&client_id)
            .is_some_and(|client| client.identity == Identity::App)
    }

    /// Whether the compositor may make one more descriptor for a client, or
    /// keep one that is in a request's hands and so in no count.
    pub(crate) fn may_hold_descriptor(&self, client_id: ClientId) -> Result<(), OverBudget> {
        if self.is_app(client_id) && self.descriptors_left() <= RESERVE {
            return Err(OverBudget::Reserve);
        }
        Ok(())
    }

    /// Whether what a client has just sent may stay, where `before` were
    /// left before it arrived. The count already has what is kept. An app
    /// whose requests let go of as much as it sent has taken nothing, even
    /// while the shell is using its reserve.
    pub(crate) fn may_keep_descriptors(&self, client_id: ClientId, before: usize) -> Result<(), OverBudget> {
        let left = self.descriptors_left();
        if self.is_app(client_id) && left < RESERVE && left < before {
            return Err(OverBudget::Reserve);
        }
        Ok(())
    }

    /// Whether a client may hold one more pool of `bytes`. The pool's file
    /// is a descriptor the client sent, which is checked where it arrives.
    pub(crate) fn may_make_pool(&self, client_id: ClientId, bytes: usize) -> Result<(), OverBudget> {
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
