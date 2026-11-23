//! Attacks the compositor must refuse. Each test is one from a decision
//! record, and names it.

use crate::common;

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

/// An app asks for each global it is refused: layer shell, session lock,
/// and security contexts. None is offered, and none is bound by guessing
/// its number. The shell is offered all but security contexts.
///
/// The compositor creates session lock and security contexts not at all,
/// so the test adds one of each. Then the policy is what refuses them.
#[test]
fn an_app_gets_no_global_it_is_refused_by_asking_or_guessing() {
    let compositor = Compositor::start();
    let before = compositor.shell().last_global();
    let refused = plant(&compositor, policy::APPS_REFUSED);
    let last = last_number(&compositor);
    let planted = u32::try_from(refused.len()).unwrap();
    assert!(
        last > before + planted,
        "the planted globals have numbers past {last}, where the guesses stop"
    );

    let app = || compositor.app();
    let offered = app().interfaces();
    for interface in &refused {
        assert!(
            !offered.iter().any(|name| name == interface),
            "an app was offered {interface}"
        );
        assert_eq!(
            guess_up_to(&app, interface, last),
            [0; 0],
            "an app bound {interface} by guessing its number"
        );
    }
    assert_eq!(planted_binds(&compositor), 0, "an app bound a global it is refused");

    let shell = compositor.shell().interfaces();
    for interface in &refused {
        let offered = shell.iter().any(|name| name == interface);
        assert_eq!(
            offered,
            !policy::SHELL_REFUSED.contains(interface),
            "the shell and {interface}"
        );
    }
}

/// A global the compositor gains later is offered to every kind of client,
/// and each binds it by the same guess an app makes above. So the test
/// above passes because the policy refuses, not because a planted global
/// cannot be bound.
#[test]
fn a_global_the_compositor_gains_later_is_bound_by_every_client() {
    let compositor = Compositor::start();
    let befores: Vec<(&str, Client)> = vec![("the shell", compositor.shell()), ("an app", compositor.app())];
    let later = plant(&compositor, &["a_global_the_compositor_gains_later"])[0];
    let last = last_number(&compositor);

    let clients = EveryClient::of(&compositor);
    for ((who, connect), (_, before)) in clients.each().into_iter().zip(&befores) {
        assert!(
            connect().interfaces().iter().any(|name| name == later),
            "{who} was not offered {later}"
        );
        // A client that connected first was not told about the new global,
        // so for that client its number is one to guess.
        let bound = before
            .unadvertised(last)
            .into_iter()
            .filter(|&name| connect().guess(name, later, 1).unwrap() == Guess::Bound)
            .count();
        assert_eq!(bound, 1, "{who} did not bind {later} at its one number");
    }
}

/// A program that is neither the shell nor the person looks for any Wayland
/// socket. It finds none it can use.
///
/// The compositor makes four sockets: the shell's, the apps', the
/// shell's control channel, and the Harness's. Each refuses a peer by its user ID. Here neither
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

    // The compositor made exactly its four sockets.
    let mut made: Vec<String> = std::fs::read_dir(&compositor.dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    made.sort();
    assert_eq!(made, ["agent", "apps", "control", "shell"]);

    // None is a way in.
    let control = compositor.dir.join("control");
    let agent = compositor.dir.join("agent");
    for socket in [
        &compositor.parts.shell.socket,
        &compositor.parts.apps.socket,
        &control,
        &agent,
    ] {
        assert!(
            connect(socket).is_err(),
            "{} accepted a peer with the wrong user ID",
            socket.display()
        );
    }
}

/// An app tries to bind the real layer shell, which only the shell is
/// offered: by asking, and by guessing its number.
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
