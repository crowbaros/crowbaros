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
pub mod control;
mod cursor;
mod cursor_shape;
mod damage;
pub mod data_device;
pub mod dmabuf;
mod edid;
pub mod event_loop;
mod evidence;
mod explicit_sync;
mod fractional_scale;
pub mod geometry;
mod globals;
mod grabs;
pub mod identity;
mod idle_inhibit;
pub mod image;
mod json;
mod layer;
mod layout;
pub mod limits;
pub mod lock;
pub mod log;
pub mod modes;
mod output;
pub mod policy;
pub mod presentation;
mod primary_selection;
mod redraw;
pub mod render;
mod scene;
mod seat;
mod shm;
mod state;
mod surface;
mod viewporter;
mod windows;
pub mod workspaces;
mod xcursor;
mod xdg;

pub use output::{OUTPUT_VERSION, Output};
pub use scene::{FRAME_FOCUSED, FRAME_UNFOCUSED, FRAME_WIDTH};
pub use seat::{REPEAT, SEAT_VERSION};
pub use shm::MAX_POOL_SIZE;
pub use state::State;
pub use surface::MAX_SUBSURFACE_DEPTH;
pub use windows::{BACKGROUND, NEW_WINDOW_OFFSET};
#[doc(hidden)]
pub use xcursor::file_for_tests as xcursor_file_for_tests;
pub use xdg::ToplevelRequests;
