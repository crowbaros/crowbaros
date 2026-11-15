//! Which client may see which Wayland global.
//!
//! This is the one table that decides it. Every global the compositor creates
//! passes through [`client_may_see`] (see the `GlobalDispatch` impl in
//! `handlers/mod.rs`), so a global that is in no list here is visible to no
//! client at all, the shell included.
//!
//! A client's [`Principal`] is fixed by how its connection was made, never by
//! anything the client says about itself:
//!
//! - **Shell**: the compositor made the socket pair itself and handed one end
//!   to the shell process it started.
//! - **Ordinary**: the client connected to the public socket in
//!   `XDG_RUNTIME_DIR`.
//! - **Sandboxed**: the client connected to a listener that was registered
//!   through security-context.

use smithay::reexports::wayland_server::Client;

use crate::state::ClientState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Principal {
    Shell,
    Ordinary,
    Sandboxed,
}

/// What a sandboxed client gets: enough to draw windows and receive its own
/// input.
const SANDBOXED: &[&str] = &[
    "wl_compositor",
    "wl_subcompositor",
    "wl_shm",
    "wl_seat",
    "wl_output",
    "zxdg_output_manager_v1",
    "xdg_wm_base",
    "zwp_linux_dmabuf_v1",
    "wp_viewporter",
    "wp_fractional_scale_manager_v1",
];

/// Added for a client on the public socket: the clipboard, and the right to
/// start a sandboxed client.
const ORDINARY_EXTRA: &[&str] = &["wl_data_device_manager", "wp_security_context_manager_v1"];

/// Added for the shell alone.
const SHELL_EXTRA: &[&str] = &["zwlr_layer_shell_v1"];

pub fn may_see(principal: Principal, interface: &str) -> bool {
    let lists: &[&[&str]] = match principal {
        Principal::Sandboxed => &[SANDBOXED],
        Principal::Ordinary => &[SANDBOXED, ORDINARY_EXTRA],
        Principal::Shell => &[SANDBOXED, ORDINARY_EXTRA, SHELL_EXTRA],
    };
    lists.iter().any(|list| list.contains(&interface))
}

/// The principal of a connected client. A client the compositor did not
/// insert itself has none.
pub fn principal_of(client: &Client) -> Option<Principal> {
    client.get_data::<ClientState>().map(|state| state.principal)
}

pub fn client_may_see(client: &Client, interface: &str) -> bool {
    principal_of(client).is_some_and(|principal| may_see(principal, interface))
}
