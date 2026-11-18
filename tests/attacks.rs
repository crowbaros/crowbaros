//! Attacks the compositor must refuse. Each test is one from a decision
//! record, and names it.

mod common;

use common::{Compositor, another_uid, connect, own_uid, runtime_dir};
use crowbar_compositor::policy;
use crowbar_probe::{Client, CompartmentListener, Guess};

/// Adds one inert global of each listed interface, and returns the names.
fn plant(compositor: &Compositor, interfaces: &[&'static str]) -> Vec<&'static str> {
    let list = interfaces.to_vec();
    compositor.with(move |state| {
        for interface in &list {
            state.plant_global(interface);
        }
    });
    interfaces.to_vec()
}

fn planted_binds(compositor: &Compositor) -> usize {
    compositor.with(|state| state.planted_binds())
}

/// A way to connect one more client of some kind.
type Connect<'a> = Box<dyn Fn() -> Client + 'a>;

/// Every kind of client there is.
struct EveryClient<'a> {
    compositor: &'a Compositor,
    plain: CompartmentListener,
    with_gpu: CompartmentListener,
}

impl<'a> EveryClient<'a> {
    fn of(compositor: &'a Compositor) -> Self {
        EveryClient {
            compositor,
            plain: compositor.compartment("plain").1,
            with_gpu: compositor.compartment_with_gpu("with-gpu").1,
        }
    }

    fn each(&self) -> Vec<(&'static str, Connect<'_>)> {
        vec![
            ("the shell", Box::new(|| self.compositor.shell())),
            ("the Warden", Box::new(|| self.compositor.warden())),
            ("a compartment", Box::new(|| connect(&self.plain.path).unwrap())),
            (
                "a compartment with the GPU",
                Box::new(|| connect(&self.with_gpu.path).unwrap()),
            ),
        ]
    }
}

/// Asks for an interface at every global number up to `last` that the client
/// was not told about. Returns the numbers at which the compositor made the
/// object.
fn guess_up_to(connect: &dyn Fn() -> Client, interface: &str, last: u32) -> Vec<u32> {
    crowbar_probe::guess_up_to(&|| Ok(connect()), interface, last).unwrap()
}

/// Adds one more global, which the shell is offered, and returns its number.
/// No test here removes a global, so every global made before it has a lower
/// number, and a guess at each number up to this one reaches them all.
fn last_number(compositor: &Compositor) -> u32 {
    plant(compositor, &["zwlr_layer_shell_v1"]);
    compositor.shell().last_global()
}

/// ADR 0020, attack 1: a client asks for every global in the "Nobody" row,
/// and for one the table has no row for. None is offered.
///
/// The compositor creates none of these globals, so the test adds one of
/// each. Then the table is what refuses them.
#[test]
fn adr_0020_attack_1_no_client_gets_a_global_from_the_nobody_row_or_one_with_no_row() {
    let compositor = Compositor::start();
    let before = compositor.shell().last_global();
    let nobody = plant(&compositor, policy::NOBODY);
    // The spike offered the first of these. The table has no row for either.
    let no_row = plant(
        &compositor,
        &["zxdg_output_manager_v1", "wp_single_pixel_buffer_manager_v1"],
    );
    let last = last_number(&compositor);
    // The guesses below reach the number of every planted global.
    let planted = u32::try_from(nobody.len() + no_row.len()).unwrap();
    assert!(
        last > before + planted,
        "the planted globals have numbers past {last}, where the guesses stop"
    );

    let clients = EveryClient::of(&compositor);
    for (who, connect) in clients.each() {
        let offered = connect().interfaces();
        for interface in nobody.iter().chain(&no_row) {
            assert!(
                !offered.iter().any(|name| name == interface),
                "{who} was offered {interface}"
            );
            assert_eq!(
                guess_up_to(&*connect, interface, last),
                [0; 0],
                "{who} bound {interface} by guessing its number"
            );
        }
    }
    assert_eq!(
        planted_binds(&compositor),
        0,
        "a client bound a global nobody is offered"
    );
}

/// The test above passes only if a planted global can be bound at all. This
/// one has a row, and the client the row names binds it by the same guess.
#[test]
fn a_planted_global_with_a_row_is_bound_by_the_client_the_row_names() {
    let compositor = Compositor::start();
    let (before, _listener) = compositor.compartment("before");
    let cursor_shape = plant(&compositor, &["wp_cursor_shape_manager_v1"])[0];
    let listener = compositor.register("test.principal", "after");
    let warden = || compositor.warden();
    // A client that connected first was not told about the new global, so
    // for that client its number is one to guess.
    let last = last_number(&compositor);
    let numbers = before.unadvertised(last);

    let bound: Vec<u32> = numbers
        .iter()
        .copied()
        .filter(|&name| connect(&listener.path).unwrap().guess(name, cursor_shape, 1).unwrap() == Guess::Bound)
        .collect();

    assert_eq!(bound.len(), 1, "the compartment did not bind the global its row offers");
    assert_eq!(planted_binds(&compositor), 1);
    assert_eq!(
        guess_up_to(&warden, cursor_shape, last),
        [0; 0],
        "the Warden bound a compartment's global"
    );
}

/// ADR 0016, attack 11: a compartment with no listener looks for any Wayland
/// socket. It finds none.
///
/// The compositor makes two sockets, and each refuses a peer by its user ID.
/// Here neither trusted part has this process's user ID, so this process is
/// in the place of that compartment, and it is given both paths on purpose.
#[test]
fn adr_0016_attack_11_a_compartment_with_no_listener_finds_no_wayland_socket() {
    let compositor = Compositor::start_trusting(another_uid(), another_uid());

    // Nothing where a compositor with a public socket would put it.
    let public: Vec<_> = std::fs::read_dir(runtime_dir()).unwrap().collect();
    assert!(public.is_empty(), "something was made in XDG_RUNTIME_DIR: {public:?}");

    // The compositor made exactly its two sockets.
    let mut made: Vec<String> = std::fs::read_dir(&compositor.dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    made.sort();
    assert_eq!(made, ["shell", "warden"]);

    // Neither one is a way in.
    for socket in [&compositor.parts.shell.socket, &compositor.parts.warden.socket] {
        assert!(
            connect(socket).is_err(),
            "{} accepted a peer with the wrong user ID",
            socket.display()
        );
    }
}

/// ADR 0016, attack 12: a client on a listener tries to bind the
/// security-context manager, layer shell, and linux-dmabuf: the globals that
/// exist and that another client is offered. The input and capture globals
/// are in the "Nobody" row, and the test of ADR 0020's attack 1 covers them.
#[test]
fn adr_0016_attack_12_a_client_on_a_listener_cannot_bind_what_the_trusted_parts_are_offered() {
    let compositor = Compositor::start();
    let listener = compositor.register("test.principal", "c1");
    let client = || connect(&listener.path).unwrap();
    let number = |seen: &Client, interface: &str| {
        seen.globals
            .iter()
            .find(|global| global.interface == interface)
            .unwrap_or_else(|| panic!("{interface} is not offered where it should be"))
            .name
    };
    let security_context = number(&compositor.warden(), "wp_security_context_manager_v1");
    let layer_shell = number(&compositor.shell(), "zwlr_layer_shell_v1");
    let dmabuf = number(&compositor.shell(), "zwp_linux_dmabuf_v1");

    for (interface, name) in [
        ("wp_security_context_manager_v1", security_context),
        ("zwlr_layer_shell_v1", layer_shell),
        ("zwp_linux_dmabuf_v1", dmabuf),
    ] {
        assert!(
            !client().interfaces().iter().any(|offered| offered == interface),
            "a compartment was offered {interface}"
        );
        // With its real number, which the client cannot learn but could guess.
        assert_eq!(
            client().guess(name, interface, 1).unwrap(),
            Guess::Refused,
            "a compartment bound {interface}"
        );
    }
}

/// No compartment can register a security context (ADR 0016): a program
/// that sandboxes its own children gets no weaker connection for them. The
/// shell cannot either. The Warden alone is offered the manager.
#[test]
fn only_the_warden_can_register_a_listener() {
    let compositor = Compositor::start();
    let (mut client, _listener) = compositor.compartment("c1");
    let mut shell = compositor.shell();

    for (who, client) in [("a compartment", &mut client), ("the shell", &mut shell)] {
        let nested = client.register_compartment(
            &compositor.listener_path("nested"),
            Some("test.principal"),
            Some("nested"),
        );
        assert!(nested.is_err(), "{who} registered a listener");
    }
    assert!(compositor.with(|state| state.compartment("nested").is_none()));
}

/// A trusted part's socket is no use to a peer with another user ID, even
/// one that was accepted somewhere else: a client of a compartment's
/// listener that is handed the shell's path is refused there.
#[test]
fn a_peer_that_is_given_a_trusted_socket_is_refused_on_its_user_id() {
    // This process is the Warden, so that it can make a compartment, and it
    // is not the shell.
    let compositor = Compositor::start_trusting(another_uid(), own_uid());
    let (client, _listener) = compositor.compartment("c1");
    assert!(!client.interfaces().is_empty());

    assert!(connect(&compositor.parts.shell.socket).is_err());
}
