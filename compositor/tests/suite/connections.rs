//! Who a client is. The compositor decides it from which socket the peer
//! reached and from the user ID the kernel reports (ADR 0032).

use crate::common;

use std::os::unix::fs::PermissionsExt;

use common::{Compositor, another_uid, own_uid};
use crowbar_compositor::identity::Identity;

#[test]
fn the_shells_socket_takes_the_shells_user_id_and_refuses_any_other() {
    let ours = Compositor::start_trusting(own_uid(), own_uid());
    let theirs = Compositor::start_trusting(another_uid(), own_uid());

    assert!(ours.try_shell().is_ok());
    assert!(
        theirs.try_shell().is_err(),
        "a peer without the shell's user ID was accepted as the shell"
    );
}

#[test]
fn the_apps_socket_takes_the_persons_user_id_and_refuses_any_other() {
    let ours = Compositor::start_trusting(own_uid(), own_uid());
    let theirs = Compositor::start_trusting(own_uid(), another_uid());

    assert!(ours.try_app().is_ok());
    assert!(
        theirs.try_app().is_err(),
        "a peer without the person's user ID was accepted as an app"
    );
}

/// The owner and the group of each socket's directory may connect; the
/// check that counts is the peer's user ID.
#[test]
fn both_sockets_are_for_their_owner_and_group_alone() {
    let compositor = Compositor::start();
    for socket in [&compositor.parts.shell.socket, &compositor.parts.apps.socket] {
        let mode = std::fs::metadata(socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o660, "{}", socket.display());
    }
}

/// The two sockets are not interchangeable: which one a peer reached decides
/// whether it is the shell or an app, even when the shell and the person
/// have one user ID.
#[test]
fn the_socket_a_client_reached_says_which_kind_of_client_it_is() {
    let compositor = Compositor::start();

    let shell = compositor.shell();
    let app = compositor.app();

    assert!(shell.interfaces().contains(&"zwlr_layer_shell_v1".to_string()));
    assert!(!app.interfaces().contains(&"zwlr_layer_shell_v1".to_string()));
    let mut kinds = compositor.with(|state| state.client_identities());
    kinds.sort_by_key(|identity| *identity == Identity::App);
    assert_eq!(kinds, [Identity::Shell, Identity::App]);
}

/// Every app shares the one socket, and each connection is a client of its
/// own.
#[test]
fn many_apps_connect_at_the_one_socket() {
    let compositor = Compositor::start();
    let apps: Vec<_> = (0..4).map(|_| compositor.app()).collect();
    assert_eq!(apps.len(), 4);
    assert_eq!(compositor.client_count(), 4);
}
