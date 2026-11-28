//! Lever (`crowbar-lever`): the CrowbarOS service that drives the person's
//! harnesses behind a protocol-agnostic [`harness::Harness`] port and keeps
//! the record of every thread in the person's home.

pub mod acp;
pub mod channel;
pub mod discovery;
pub mod frames;
pub mod fulcrum;
pub mod handoff;
pub mod harness;
pub mod log;
pub mod mcp;
pub mod opencode;
pub mod project;
pub mod service;
pub mod store;
pub mod sys;
pub mod version;
pub mod watcher;
