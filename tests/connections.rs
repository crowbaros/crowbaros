//! Who a client is. The compositor decides it from how the connection was
//! made, and from the user ID the kernel reports (ADR 0016).

mod common;

use common::{Compositor, another_uid, connect, own_uid};

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
fn the_wardens_socket_takes_the_wardens_user_id_and_refuses_any_other() {
    let ours = Compositor::start_trusting(own_uid(), own_uid());
    let theirs = Compositor::start_trusting(own_uid(), another_uid());

    assert!(ours.try_warden().is_ok());
    assert!(
        theirs.try_warden().is_err(),
        "a peer without the Warden's user ID was accepted as the Warden"
    );
}

/// The two sockets are not interchangeable: which one a peer reached decides
/// which trusted part it is.
#[test]
fn the_socket_a_trusted_part_reached_says_which_part_it_is() {
    let compositor = Compositor::start();

    let shell = compositor.shell();
    let warden = compositor.warden();

    assert!(shell.interfaces().contains(&"zwlr_layer_shell_v1".to_string()));
    assert!(
        !shell
            .interfaces()
            .contains(&"wp_security_context_manager_v1".to_string())
    );
    assert_eq!(warden.interfaces(), ["wp_security_context_manager_v1"]);
}

/// The client of a compartment's listener belongs to that compartment, as
/// the Warden named it.
#[test]
fn a_client_belongs_to_the_compartment_whose_listener_it_came_through() {
    let compositor = Compositor::start();
    let mail = compositor.register("org.example.Mail", "compartment-1");
    let notes = compositor.register("org.example.Notes", "compartment-2");
    let _a = connect(&mail.path).unwrap();
    let _b = connect(&notes.path).unwrap();

    let named = compositor.with(|state| {
        ["compartment-1", "compartment-2"].map(|id| {
            let compartment = state.compartment(id).unwrap();
            (compartment.principal().to_string(), compartment.had_client())
        })
    });
    assert_eq!(
        named,
        [
            ("org.example.Mail".to_string(), true),
            ("org.example.Notes".to_string(), true)
        ]
    );
}

#[test]
fn a_listener_registered_without_a_principal_id_or_a_compartment_id_accepts_nothing() {
    let compositor = Compositor::start();
    let mut warden = compositor.warden();

    for (name, principal, compartment) in [
        ("no-principal", None, Some("compartment-7")),
        ("no-compartment", Some("org.example.Mail"), None),
        ("empty-principal", Some(""), Some("compartment-8")),
        ("empty-compartment", Some("org.example.Mail"), Some("")),
    ] {
        let listener = warden
            .register_compartment(&compositor.listener_path(name), principal, compartment)
            .unwrap();
        assert!(
            connect(&listener.path).is_err(),
            "{name}: a client with no identity was accepted"
        );
    }
}

#[test]
fn a_second_listener_for_a_live_compartment_accepts_nothing() {
    let compositor = Compositor::start();
    let _first = compositor.register("org.example.Mail", "compartment-7");

    let second = compositor
        .warden()
        .register_compartment(
            &compositor.listener_path("again"),
            Some("org.example.Notes"),
            Some("compartment-7"),
        )
        .unwrap();

    assert!(connect(&second.path).is_err());
}

/// The Warden stops a compartment and lets go of its listener.
#[test]
fn the_listener_of_a_stopped_compartment_accepts_nothing() {
    let compositor = Compositor::start();
    let mut listener = compositor.register("org.example.Mail", "compartment-7");
    let path = listener.path.clone();
    assert!(connect(&path).is_ok());

    listener.close();

    // The compositor learns of it on its next turn.
    let refused = (0..200).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(5));
        connect(&path).is_err()
    });
    assert!(
        refused,
        "the listener still accepted clients after the Warden let go of it"
    );
}

/// The Warden's connection can end, as when it restarts, and the listeners
/// it registered go on serving their compartments.
#[test]
fn a_listener_outlives_the_wardens_connection() {
    let compositor = Compositor::start();
    let listener = {
        let mut warden = compositor.warden();
        warden
            .register_compartment(&compositor.listener_path("c1"), Some("p"), Some("c1"))
            .unwrap()
    };
    assert!(connect(&listener.path).is_ok());
}

/// A GPU need recorded after the compartment's first client connected is
/// refused: what a client is offered is fixed when it connects.
#[test]
fn the_gpu_need_cannot_widen_what_a_connected_client_was_offered() {
    let compositor = Compositor::start();
    let (client, _listener) = compositor.compartment("c1");
    let refused = compositor.with(|state| state.compartment("c1").unwrap().record_gpu_need().is_err());
    assert!(refused);
    assert!(!client.interfaces().contains(&"zwp_linux_dmabuf_v1".to_string()));
}
