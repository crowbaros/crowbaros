//! Which client may see which Wayland global.
//!
//! This is the one table that decides it. Every global the compositor creates
//! passes through [`client_may_see`] (see the `GlobalDispatch` impl in
//! `handlers/mod.rs`), so a global that is in no list here is visible to no
//! client at all, the shell included.
//!
//! This is not the glossary's Principal. A kind says how a client reached the
//! compositor; which principal owns a surface is a later question.
//!
//! A client's [`ClientKind`] is fixed by how its connection was made, never by
//! anything the client says about itself:
//!
//! - **Shell**: the compositor made the socket pair itself and handed one end
//!   to the shell process it started.
//! - **Ordinary**: the client connected to the public socket in
//!   `XDG_RUNTIME_DIR`.
//! - **SecurityContext**: the client connected to a listener that was registered
//!   through security-context.

use smithay::reexports::wayland_server::Client;

use crate::state::ClientState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientKind {
    Shell,
    Ordinary,
    SecurityContext,
}

/// What a security-context client gets: enough to draw windows and receive its own
/// input.
const CONFINED: &[&str] = &[
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
/// start a security-context client.
const ORDINARY_EXTRA: &[&str] = &["wl_data_device_manager", "wp_security_context_manager_v1"];

/// Added for the shell alone.
const SHELL_EXTRA: &[&str] = &["zwlr_layer_shell_v1"];

pub fn may_see(kind: ClientKind, interface: &str) -> bool {
    let lists: &[&[&str]] = match kind {
        ClientKind::SecurityContext => &[CONFINED],
        ClientKind::Ordinary => &[CONFINED, ORDINARY_EXTRA],
        ClientKind::Shell => &[CONFINED, ORDINARY_EXTRA, SHELL_EXTRA],
    };
    lists.iter().any(|list| list.contains(&interface))
}

/// The kind of a connected client. A client the compositor did not
/// insert itself has none.
pub fn kind_of(client: &Client) -> Option<ClientKind> {
    client.get_data::<ClientState>().map(|state| state.kind)
}

pub fn client_may_see(client: &Client, interface: &str) -> bool {
    kind_of(client).is_some_and(|kind| may_see(kind, interface))
}
