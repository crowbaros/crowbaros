//! Attacks the compositor must refuse. Each test is one from a decision
//! record, and names it.

mod common;

use common::{Compositor, another_uid, connect, own_uid, runtime_dir};
use crowbar_compositor::policy;
use crowbar_probe::{Client, Guess};

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
}

impl<'a> EveryClient<'a> {
    fn of(compositor: &'a Compositor) -> Self {
        EveryClient { compositor }
    }

    fn each(&self) -> Vec<(&'static str, Connect<'_>)> {
        vec![
            ("the shell", Box::new(|| self.compositor.shell())),
            ("an app", Box::new(|| self.compositor.app())),
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
    let before = compositor.app();
    let cursor_shape = plant(&compositor, &["wp_cursor_shape_manager_v1"])[0];
    // A client that connected first was not told about the new global, so
    // for that client its number is one to guess.
    let last = last_number(&compositor);
    let numbers = before.unadvertised(last);

    let bound: Vec<u32> = numbers
        .iter()
        .copied()
        .filter(|&name| compositor.app().guess(name, cursor_shape, 1).unwrap() == Guess::Bound)
        .collect();

    assert_eq!(bound.len(), 1, "the app did not bind the global its row offers");
    assert_eq!(planted_binds(&compositor), 1);
}

/// A program that is neither the shell nor the person looks for any Wayland
/// socket. It finds none it can use.
///
/// The compositor makes three sockets: the shell's, the apps', and the
/// shell's control channel. Each refuses a peer by its user ID. Here neither
/// the shell nor the person has this process's user ID, and this process is
/// given every path on purpose.
#[test]
fn a_program_that_is_neither_the_shell_nor_the_person_finds_no_wayland_socket() {
    let compositor = Compositor::start_trusting(another_uid(), another_uid());

    // Nothing where a compositor with a public socket would put it: Base
    // links the apps' socket into the person's runtime directory, not the
    // compositor.
    let public: Vec<_> = std::fs::read_dir(runtime_dir()).unwrap().collect();
    assert!(public.is_empty(), "something was made in XDG_RUNTIME_DIR: {public:?}");

    // The compositor made exactly its three sockets.
    let mut made: Vec<String> = std::fs::read_dir(&compositor.dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    made.sort();
    assert_eq!(made, ["apps", "control", "shell"]);

    // None is a way in.
    let control = compositor.dir.join("control");
    for socket in [&compositor.parts.shell.socket, &compositor.parts.apps.socket, &control] {
        assert!(
            connect(socket).is_err(),
            "{} accepted a peer with the wrong user ID",
            socket.display()
        );
    }
}

/// An app tries to bind layer shell, which only the shell is offered: by
/// asking, and by guessing its number. The input and capture globals are in
/// the "Nobody" row, and the test above covers them.
#[test]
fn an_app_cannot_bind_layer_shell() {
    let compositor = Compositor::start();
    let layer_shell = compositor
        .shell()
        .globals
        .iter()
        .find(|global| global.interface == "zwlr_layer_shell_v1")
        .expect("the shell is offered layer shell")
        .name;

    assert!(
        !compositor
            .app()
            .interfaces()
            .iter()
            .any(|offered| offered == "zwlr_layer_shell_v1"),
        "an app was offered layer shell"
    );
    // With its real number, which the app cannot learn but could guess.
    assert_eq!(
        compositor.app().guess(layer_shell, "zwlr_layer_shell_v1", 1).unwrap(),
        Guess::Refused,
        "an app bound layer shell"
    );
}

/// The shell's socket and its control channel are no use to the person's
/// apps: an app that is handed their paths is refused on its user ID.
#[test]
fn an_app_that_is_given_the_shells_sockets_is_refused_on_its_user_id() {
    // This process is the person, and not the shell.
    let compositor = Compositor::start_trusting(another_uid(), own_uid());
    let client = compositor.app();
    assert!(!client.interfaces().is_empty());

    assert!(connect(&compositor.parts.shell.socket).is_err());
    let control = std::os::unix::net::UnixStream::connect(compositor.dir.join("control")).unwrap();
    assert!(
        closed_at_once(control),
        "the control channel kept a peer without the shell's user ID"
    );
}

/// Whether the other end closes a stream without a word.
fn closed_at_once(mut stream: std::os::unix::net::UnixStream) -> bool {
    use std::io::Read;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut byte = [0u8; 1];
    matches!(stream.read(&mut byte), Ok(0))
}
