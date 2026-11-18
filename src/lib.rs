//! The CrowbarOS Wayland compositor, written from the ground up.
//!
//! - [`event_loop`] waits on `epoll` on one thread.
//! - [`identity`] says who a client is. The compositor decides it when the
//!   connection is made and never takes it from the client (ADR 0016).
//! - [`policy`] is the one table that says which client is offered which
//!   global (ADR 0020).
//! - [`connections`] is every way a client can connect. There is no public
//!   socket.
//!
//! The wire protocol is `fulcrum-wire`. Nothing here holds `unsafe` code.

#![forbid(unsafe_code)]

pub mod backend;
mod client;
pub mod connections;
pub mod event_loop;
pub mod geometry;
mod globals;
pub mod identity;
pub mod image;
mod layer;
pub mod limits;
pub mod log;
mod output;
pub mod policy;
mod redraw;
mod shm;
mod state;
mod surface;
mod windows;
mod xdg;

pub use output::{OUTPUT_VERSION, Output};
pub use shm::MAX_POOL_SIZE;
pub use state::State;
pub use surface::MAX_SUBSURFACE_DEPTH;
pub use windows::{BACKGROUND, NEW_WINDOW_OFFSET};
pub use xdg::ToplevelRequests;
