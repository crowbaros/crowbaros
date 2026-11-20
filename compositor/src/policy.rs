//! What each client is offered.
//!
//! This is the one place that decides it, from CrowbarOS's ADR 0032: the
//! shell and the person's apps are the two kinds of client. The registry
//! advertises a global to a client, and lets the client bind it, only if
//! [`offers`] says so. A global with no row here is offered to nobody,
//! including one the compositor gains later.
//!
//! A row can name a global the compositor does not create yet.

use crate::identity::Identity;

/// Every client: what a Wayland app expects. The core protocols, xdg-shell,
/// the data device and the primary selection (see `data_device.rs`),
/// viewporter, fractional scale, cursor shape, presentation time, and idle
/// inhibit.
const EVERY_CLIENT: &[&str] = &[
    "wl_compositor",
    "wl_subcompositor",
    "wl_shm",
    "wl_seat",
    "wl_output",
    "wl_data_device_manager",
    "zwp_primary_selection_device_manager_v1",
    "xdg_wm_base",
    "wp_viewporter",
    "wp_fractional_scale_manager_v1",
    "wp_cursor_shape_manager_v1",
    "wp_presentation",
    "zwp_idle_inhibit_manager_v1",
];

/// The GPU's buffers, for every client: linux-dmabuf and explicit sync.
const GPU: &[&str] = &["zwp_linux_dmabuf_v1", "wp_linux_drm_syncobj_manager_v1"];

/// The shell alone: layer shell, so that only the shell draws the bar, the
/// dock, the launcher, the wallpaper, and the lock screen.
const SHELL_ALONE: &[&str] = &["zwlr_layer_shell_v1"];

/// Nobody. A global is offered to nobody without being listed here, because
/// it has no row. The list is here so that the tests ask for each one by
/// name, and so that adding one of these to a row above fails a test.
pub const NOBODY: &[&str] = &[
    // Activation
    "xdg_activation_v1",
    // Data control
    "zwlr_data_control_manager_v1",
    "ext_data_control_manager_v1",
    // Every capture protocol
    "ext_image_copy_capture_manager_v1",
    "ext_output_image_capture_source_manager_v1",
    "ext_foreign_toplevel_image_capture_source_manager_v1",
    "zwlr_screencopy_manager_v1",
    "zwlr_export_dmabuf_manager_v1",
    // Virtual keyboard and virtual pointer
    "zwp_virtual_keyboard_manager_v1",
    "zwlr_virtual_pointer_manager_v1",
    // Input method and text input
    "zwp_input_method_manager_v2",
    "zwp_input_method_v1",
    "zwp_input_panel_v1",
    "zwp_text_input_manager_v3",
    "zwp_text_input_manager_v1",
    // Session lock
    "ext_session_lock_manager_v1",
    // Foreign toplevel lists
    "ext_foreign_toplevel_list_v1",
    "zwlr_foreign_toplevel_manager_v1",
    // Shortcut inhibit
    "zwp_keyboard_shortcuts_inhibit_manager_v1",
    // Pointer constraints and relative pointer
    "zwp_pointer_constraints_v1",
    "zwp_relative_pointer_manager_v1",
    // Tablets
    "zwp_tablet_manager_v2",
    // DRM leasing
    "wp_drm_lease_device_v1",
    // The XWayland shell
    "xwayland_shell_v1",
    // Security contexts: a client could make another way in
    "wp_security_context_manager_v1",
];

/// Whether a client with this identity is offered the global with this
/// interface name.
///
/// An app is offered what a Wayland app expects, the GPU's buffers included.
/// The shell is offered that, and layer shell.
pub fn offers(identity: &Identity, interface: &str) -> bool {
    let rows: &[&[&str]] = match identity {
        Identity::Shell => &[EVERY_CLIENT, GPU, SHELL_ALONE],
        Identity::App => &[EVERY_CLIENT, GPU],
    };
    rows.iter().any(|row| row.contains(&interface))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERYONE: [Identity; 2] = [Identity::Shell, Identity::App];

    /// What ADR 0032 offers every app: what a Wayland app expects, the GPU's
    /// buffers included.
    const ADR_EVERY_APP: [&str; 15] = [
        "wl_compositor",
        "wl_subcompositor",
        "wl_shm",
        "wl_seat",
        "wl_output",
        "wl_data_device_manager",
        "zwp_primary_selection_device_manager_v1",
        "xdg_wm_base",
        "wp_viewporter",
        "wp_fractional_scale_manager_v1",
        "wp_cursor_shape_manager_v1",
        "wp_presentation",
        "zwp_idle_inhibit_manager_v1",
        "zwp_linux_dmabuf_v1",
        "wp_linux_drm_syncobj_manager_v1",
    ];

    #[test]
    fn an_app_is_offered_what_a_wayland_app_expects_the_gpus_buffers_included() {
        for interface in ADR_EVERY_APP {
            assert!(offers(&Identity::App, interface), "{interface}");
        }
    }

    #[test]
    fn layer_shell_is_for_the_shell_alone() {
        assert!(offers(&Identity::Shell, "zwlr_layer_shell_v1"));
        assert!(!offers(&Identity::App, "zwlr_layer_shell_v1"));
    }

    #[test]
    fn the_shell_is_offered_what_an_app_is_offered() {
        for interface in ADR_EVERY_APP {
            assert!(offers(&Identity::Shell, interface), "{interface}");
        }
    }

    #[test]
    fn nothing_in_the_nobody_row_is_offered_to_anybody() {
        for interface in NOBODY {
            for identity in EVERYONE {
                assert!(!offers(&identity, interface), "{interface} to {identity:?}");
            }
        }
    }

    #[test]
    fn a_global_with_no_row_is_offered_to_nobody() {
        for interface in [
            "zxdg_output_manager_v1",
            "wp_single_pixel_buffer_manager_v1",
            "a_global_the_compositor_gains_later",
        ] {
            for identity in EVERYONE {
                assert!(!offers(&identity, interface), "{interface} to {identity:?}");
            }
        }
    }
}
