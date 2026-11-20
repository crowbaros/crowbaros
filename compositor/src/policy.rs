//! What each client is offered.
//!
//! This is the one place that decides it, from CrowbarOS's ADR 0032: the
//! shell and the person's apps are the two kinds of client. The registry
//! advertises a global to a client, and lets the client bind it, only if
//! [`offers`] says so.
//!
//! Each kind of client is offered every global the compositor has, less a
//! short list, as on any desktop. A global the compositor gains later is
//! offered to both unless it is added to a list here.
//!
//! A list can name a global the compositor does not create yet.

use crate::identity::Identity;

/// What an app is refused: layer shell, so that only the shell draws the
/// bar, the dock, the launcher, the wallpaper, and the lock screen; session
/// lock, so that no app can lock the desktop or draw its lock screen; and
/// security contexts, with which a client could make another way in.
pub const APPS_REFUSED: &[&str] = &[
    "zwlr_layer_shell_v1",
    "ext_session_lock_manager_v1",
    "wp_security_context_manager_v1",
];

/// What the shell is refused: security contexts.
pub const SHELL_REFUSED: &[&str] = &["wp_security_context_manager_v1"];

/// Whether a client with this identity is offered the global with this
/// interface name.
pub fn offers(identity: &Identity, interface: &str) -> bool {
    let refused = match identity {
        Identity::Shell => SHELL_REFUSED,
        Identity::App => APPS_REFUSED,
    };
    !refused.contains(&interface)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERYONE: [Identity; 2] = [Identity::Shell, Identity::App];

    #[test]
    fn an_app_is_refused_layer_shell_session_lock_and_security_contexts() {
        for interface in [
            "zwlr_layer_shell_v1",
            "ext_session_lock_manager_v1",
            "wp_security_context_manager_v1",
        ] {
            assert!(!offers(&Identity::App, interface), "{interface}");
        }
    }

    #[test]
    fn layer_shell_is_for_the_shell_alone() {
        assert!(offers(&Identity::Shell, "zwlr_layer_shell_v1"));
        assert!(!offers(&Identity::App, "zwlr_layer_shell_v1"));
    }

    #[test]
    fn nobody_is_offered_security_contexts() {
        for identity in EVERYONE {
            assert!(!offers(&identity, "wp_security_context_manager_v1"), "{identity:?}");
        }
    }

    #[test]
    fn the_shell_is_offered_everything_an_app_is_offered() {
        for interface in [
            "wl_compositor",
            "xdg_wm_base",
            "zwp_linux_dmabuf_v1",
            "wl_data_device_manager",
        ] {
            assert!(offers(&Identity::App, interface), "{interface}");
            assert!(offers(&Identity::Shell, interface), "{interface}");
        }
        for interface in SHELL_REFUSED {
            assert!(APPS_REFUSED.contains(interface), "{interface}");
        }
    }

    #[test]
    fn a_global_the_compositor_gains_later_is_offered_to_everyone() {
        for interface in [
            "zxdg_output_manager_v1",
            "xdg_activation_v1",
            "a_global_the_compositor_gains_later",
        ] {
            for identity in EVERYONE {
                assert!(offers(&identity, interface), "{interface} to {identity:?}");
            }
        }
    }
}
