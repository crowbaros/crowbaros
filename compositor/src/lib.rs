//! Spike for issue 3: the CrowbarOS compositor on Smithay.
//!
//! Started from Smithay's `smallvil` example (MIT, see `LICENSE-smithay.txt`).
//! Throwaway code: it exists to test ADR 0015, not to ship.
#![allow(irrefutable_let_patterns)]

pub mod control;
pub mod grabs;
pub mod handlers;
pub mod input;
pub mod policy;
pub mod probe;
pub mod state;
pub mod tty;
pub mod windows;
pub mod winit;

pub use state::State;

pub mod session;
