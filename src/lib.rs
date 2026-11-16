//! The CrowbarOS Wayland compositor, on Smithay. It is part of Base and part
//! of what CrowbarOS trusts.
//!
//! - [`identity`] says who a client is. The compositor decides it when the
//!   connection is made and never takes it from the client (ADR 0016).
//! - [`policy`] is the one table that says which client is offered which
//!   Wayland global (ADR 0020).
//! - [`connections`] is every way a client can connect. There is no public
//!   socket.
//! - [`backend`] is where frames go. The compositor draws only when something
//!   changed.

pub mod backend;
pub mod connections;
mod handlers;
pub mod identity;
mod input;
pub mod policy;
mod redraw;
mod state;
mod windows;

pub use state::State;
