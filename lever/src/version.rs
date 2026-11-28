//! Protocol and package version constants shared across Lever modules.

/// Lever's protocol version spoken on both the surface and person sockets.
pub const PROTOCOL_VERSION: u64 = 1;

/// Lever's package version reported in `status` replies and client handshakes.
pub const LEVER_VERSION: &str = env!("CARGO_PKG_VERSION");
