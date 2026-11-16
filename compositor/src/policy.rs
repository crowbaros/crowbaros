//! What each client is offered.
//!
//! This is the table "What each client is offered" of ADR 0020, and the one
//! place that decides it. Every global the compositor creates passes through
//! [`offered_to`] (see the `GlobalDispatch` impl in `handlers/mod.rs`), so a
//! global with no row here is offered to nobody. That includes one a later
//! Smithay adds.
//!
//! A row can name a global the compositor does not create yet. The README
//! says which ones exist.

use smithay::reexports::wayland_server::Client;

use crate::identity::{self, Identity};

/// Every compartment with a Wayland listener: the core protocols, xdg-shell,
/// the data device, viewporter, fractional scale, cursor shape, presentation
/// time, and idle inhibit.
const EVERY_COMPARTMENT: &[&str] = &[
    "wl_compositor",
    "wl_subcompositor",
    "wl_shm",
    "wl_seat",
    "wl_output",
    "wl_data_device_manager",
    "xdg_wm_base",
    "wp_viewporter",
    "wp_fractional_scale_manager_v1",
    "wp_cursor_shape_manager_v1",
    "wp_presentation",
    "zwp_idle_inhibit_manager_v1",
];

/// A compartment whose needs include the GPU: linux-dmabuf and explicit sync.
const GPU: &[&str] = &["zwp_linux_dmabuf_v1", "wp_linux_drm_syncobj_manager_v1"];

/// The shell alone: layer shell.
const SHELL_ALONE: &[&str] = &["zwlr_layer_shell_v1"];

/// The Warden alone: the security context manager.
const WARDEN_ALONE: &[&str] = &["wp_security_context_manager_v1"];

/// Nobody. A global is offered to nobody without being listed here, because
/// it has no row. The list is here so that the tests ask for each one by
/// name, and so that adding one of these to a row above fails a test.
pub const NOBODY: &[&str] = &[
    // Activation
    "xdg_activation_v1",
    // Primary selection
    "zwp_primary_selection_device_manager_v1",
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
];

/// Whether a client with this identity is offered the global with this
/// interface name.
///
/// The shell is not a compartment. It draws with the GPU, so it is offered
/// what a compartment with the GPU is offered, and layer shell. The Warden
/// draws nothing, and is offered the security context manager and nothing
/// else.
pub fn offers(identity: &Identity, interface: &str) -> bool {
    let rows: &[&[&str]] = match identity {
        Identity::Shell => &[EVERY_COMPARTMENT, GPU, SHELL_ALONE],
        Identity::Warden => &[WARDEN_ALONE],
        Identity::Compartment(compartment) if compartment.needs_gpu() => &[EVERY_COMPARTMENT, GPU],
        Identity::Compartment(_) => &[EVERY_COMPARTMENT],
    };
    rows.iter().any(|row| row.contains(&interface))
}

/// Whether a connected client is offered the global with this interface
/// name. A client with no identity is offered nothing.
pub fn offered_to(client: &Client, interface: &str) -> bool {
    identity::of(client).is_some_and(|identity| offers(identity, interface))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::identity::Compartment;

    fn compartment() -> Identity {
        Identity::Compartment(Arc::new(Compartment::new("principal".into(), "compartment".into())))
    }

    fn compartment_with_gpu() -> Identity {
        let compartment = Compartment::new("principal".into(), "compartment".into());
        compartment.record_gpu_need();
        Identity::Compartment(Arc::new(compartment))
    }

    fn everyone() -> [Identity; 4] {
        [Identity::Shell, Identity::Warden, compartment(), compartment_with_gpu()]
    }

    /// The names ADR 0020 gives every compartment with a Wayland listener.
    const ADR_EVERY_COMPARTMENT: [&str; 12] = [
        "wl_compositor",
        "wl_subcompositor",
        "wl_shm",
        "wl_seat",
        "wl_output",
        "wl_data_device_manager",
        "xdg_wm_base",
        "wp_viewporter",
        "wp_fractional_scale_manager_v1",
        "wp_cursor_shape_manager_v1",
        "wp_presentation",
        "zwp_idle_inhibit_manager_v1",
    ];

    #[test]
    fn every_compartment_is_offered_the_protocols_of_its_row() {
        for interface in ADR_EVERY_COMPARTMENT {
            assert!(offers(&compartment(), interface), "{interface}");
            assert!(offers(&compartment_with_gpu(), interface), "{interface}");
        }
    }

    #[test]
    fn dmabuf_and_explicit_sync_need_the_gpu() {
        for interface in ["zwp_linux_dmabuf_v1", "wp_linux_drm_syncobj_manager_v1"] {
            assert!(!offers(&compartment(), interface), "{interface}");
            assert!(offers(&compartment_with_gpu(), interface), "{interface}");
        }
    }

    #[test]
    fn layer_shell_is_for_the_shell_alone() {
        assert!(offers(&Identity::Shell, "zwlr_layer_shell_v1"));
        for other in [Identity::Warden, compartment(), compartment_with_gpu()] {
            assert!(!offers(&other, "zwlr_layer_shell_v1"), "{other:?}");
        }
    }

    #[test]
    fn the_shell_can_draw_windows_with_the_gpu() {
        for interface in ADR_EVERY_COMPARTMENT.into_iter().chain(["zwp_linux_dmabuf_v1"]) {
            assert!(offers(&Identity::Shell, interface), "{interface}");
        }
    }

    #[test]
    fn the_security_context_manager_is_for_the_warden_alone() {
        assert!(offers(&Identity::Warden, "wp_security_context_manager_v1"));
        for other in [Identity::Shell, compartment(), compartment_with_gpu()] {
            assert!(!offers(&other, "wp_security_context_manager_v1"), "{other:?}");
        }
    }

    #[test]
    fn the_warden_is_offered_nothing_else() {
        for interface in ADR_EVERY_COMPARTMENT.into_iter().chain([
            "zwp_linux_dmabuf_v1",
            "wp_linux_drm_syncobj_manager_v1",
            "zwlr_layer_shell_v1",
        ]) {
            assert!(!offers(&Identity::Warden, interface), "{interface}");
        }
    }

    #[test]
    fn nothing_in_the_nobody_row_is_offered_to_anybody() {
        for interface in NOBODY {
            for identity in everyone() {
                assert!(!offers(&identity, interface), "{interface} to {identity:?}");
            }
        }
    }

    #[test]
    fn a_global_with_no_row_is_offered_to_nobody() {
        // The spike offered the first. The table of ADR 0020 has no row for
        // either.
        for interface in [
            "zxdg_output_manager_v1",
            "wp_single_pixel_buffer_manager_v1",
            "a_global_from_a_later_smithay",
        ] {
            for identity in everyone() {
                assert!(!offers(&identity, interface), "{interface} to {identity:?}");
            }
        }
    }
}
