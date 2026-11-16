//! Who a client is. The compositor decides it from how the connection was
//! made, and from the user ID the kernel reports (ADR 0016).

mod common;

use common::{Compositor, another_uid, connect, own_uid};
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

fn owners(compositor: &Compositor) -> Vec<(String, String)> {
    compositor.with(|state| {
        state
            .window_owners()
            .into_iter()
            .map(|owner| match owner {
                Identity::Compartment(compartment) => (compartment.principal().to_owned(), compartment.id().to_owned()),
                other => (format!("{other:?}"), String::new()),
            })
            .collect()
    })
}

#[test]
fn a_window_belongs_to_the_compartment_whose_listener_its_client_came_through() {
    let compositor = Compositor::start();
    let mail = compositor.register("org.example.Mail", "compartment-7");
    let notes = compositor.register("org.example.Notes", "compartment-8");
    let mut first = connect(&mail.path).unwrap();
    let mut second = connect(&notes.path).unwrap();

    first.open_toplevel("Inbox", "mail", (300, 200), 0xffff_ffff).unwrap();
    second.open_toplevel("Notes", "notes", (300, 200), 0xffff_ffff).unwrap();

    assert_eq!(
        owners(&compositor),
        [
            ("org.example.Mail".to_owned(), "compartment-7".to_owned()),
            ("org.example.Notes".to_owned(), "compartment-8".to_owned()),
        ]
    );
}

/// A client names its own window. The name is not its identity.
#[test]
fn what_a_client_calls_itself_does_not_change_who_it_is() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("compartment-7");

    client
        .open_toplevel("CrowbarOS shell", "crowbaros-shell", (300, 200), 0xffff_ffff)
        .unwrap();

    assert_eq!(
        owners(&compositor),
        [("test.principal".to_owned(), "compartment-7".to_owned())]
    );
    assert!(!client.interfaces().contains(&"zwlr_layer_shell_v1".to_string()));
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
    let listener = compositor.register("org.example.Mail", "compartment-7");
    let path = listener.path.clone();
    assert!(connect(&path).is_ok());

    drop(listener);

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
