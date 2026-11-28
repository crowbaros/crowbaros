//! Lever (`crowbar-lever`): the CrowbarOS service that drives the person's
//! harnesses behind a protocol-agnostic [`harness::Harness`] port and keeps
//! the record of every thread in the person's home.

pub mod acp;
pub mod channel;
pub mod discovery;
pub(crate) mod frames;
pub(crate) mod fulcrum;
pub(crate) mod handoff;
pub mod harness;
pub mod log;
pub mod mcp;
pub(crate) mod opencode;
pub(crate) mod project;
pub mod service;
pub mod store;
pub mod sys;
pub mod version;
pub mod watcher;
