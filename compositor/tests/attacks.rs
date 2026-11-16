//! Attacks the compositor must refuse. Each test is one from a decision
//! record, and names it.

mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use common::{Compositor, another_uid, connect, own_uid, runtime_dir};
use crowbar_compositor::{State, policy};
use crowbar_probe::{Client, CompartmentListener, Guess};
use smithay::{
    reexports::{
        wayland_protocols::{
            ext::{
                data_control::v1::server::ext_data_control_manager_v1::ExtDataControlManagerV1,
                foreign_toplevel_list::v1::server::ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
                image_capture_source::v1::server::{
                    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
                    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
                },
                image_copy_capture::v1::server::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
                session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1,
            },
            wp::{
                drm_lease::v1::server::wp_drm_lease_device_v1::WpDrmLeaseDeviceV1,
                input_method::zv1::server::{
                    zwp_input_method_v1::ZwpInputMethodV1, zwp_input_panel_v1::ZwpInputPanelV1,
                },
                keyboard_shortcuts_inhibit::zv1::server::zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
                pointer_constraints::zv1::server::zwp_pointer_constraints_v1::ZwpPointerConstraintsV1,
                primary_selection::zv1::server::zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
                relative_pointer::zv1::server::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
                single_pixel_buffer::v1::server::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1,
                tablet::zv2::server::zwp_tablet_manager_v2::ZwpTabletManagerV2,
                text_input::{
                    zv1::server::zwp_text_input_manager_v1::ZwpTextInputManagerV1,
                    zv3::server::zwp_text_input_manager_v3::ZwpTextInputManagerV3,
                },
            },
            xdg::{
                activation::v1::server::xdg_activation_v1::XdgActivationV1,
                xdg_output::zv1::server::zxdg_output_manager_v1::ZxdgOutputManagerV1,
            },
            xwayland::shell::v1::server::xwayland_shell_v1::XwaylandShellV1,
        },
        wayland_protocols_misc::{
            zwp_input_method_v2::server::zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
            zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        },
        wayland_protocols_wlr::{
            data_control::v1::server::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
            export_dmabuf::v1::server::zwlr_export_dmabuf_manager_v1::ZwlrExportDmabufManagerV1,
            foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
            screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
            virtual_pointer::v1::server::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        },
        wayland_server::{Client as ServerClient, DataInit, DisplayHandle, New, Resource},
    },
    wayland::{Dispatch2, GlobalDispatch2},
};

/// A global that the test adds to the compositor. It refuses nobody itself,
/// so the table is all that stands between it and a client. It counts the
/// clients that bound it.
struct Planted(Arc<AtomicUsize>);

impl<I: Resource + 'static> GlobalDispatch2<I, State> for Planted {
    fn bind(
        &self,
        _state: &mut State,
        _handle: &DisplayHandle,
        _client: &ServerClient,
        resource: New<I>,
        data_init: &mut DataInit<'_, State>,
    ) {
        self.0.fetch_add(1, Ordering::SeqCst);
        data_init.init(resource, Bound);
    }

    fn can_view(&self, _client: &ServerClient) -> bool {
        true
    }
}

/// What a planted global gives a client that bound it: an object that does
/// nothing.
struct Bound;

impl<I: Resource> Dispatch2<I, State> for Bound {
    fn request(
        &self,
        _state: &mut State,
        _client: &ServerClient,
        _resource: &I,
        _request: I::Request,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, State>,
    ) {
    }
}

/// Adds one global of each listed interface, and returns each one's name.
macro_rules! plant {
    ($compositor:expr, $binds:expr, [$($interface:ty),* $(,)?]) => {{
        let binds: Arc<AtomicUsize> = $binds.clone();
        $compositor.with(move |state| {
            vec![$({
                state
                    .display_handle
                    .create_global::<State, $interface, Planted>(1, Planted(binds.clone()));
                <$interface as Resource>::interface().name
            }),*]
        })
    }};
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
        let plain = compositor.register("test.principal", "plain");
        let with_gpu = compositor.register("test.principal", "with-gpu");
        compositor.with(|state| state.compartment("with-gpu").unwrap().grant_gpu());
        EveryClient {
            compositor,
            plain,
            with_gpu,
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

/// Asks for an interface at every global number the client was not told
/// about. Returns the numbers at which the compositor made the object.
fn guess_everywhere(connect: &dyn Fn() -> Client, interface: &str) -> Vec<u32> {
    let seen = connect();
    let last = seen.state.globals.iter().map(|global| global.name).max().unwrap_or(0) + 8;
    seen.unadvertised(last)
        .into_iter()
        .filter(|&name| connect().guess(name, interface, 1).unwrap() == Guess::Bound)
        .collect()
}

/// ADR 0020, attack 1: a client asks for every global in the "Nobody" row,
/// and for one the table has no row for. None is offered.
///
/// The compositor creates none of these globals, so the test adds one of
/// each. Then the table is what refuses them.
#[test]
fn adr_0020_attack_1_no_client_gets_a_global_from_the_nobody_row_or_one_with_no_row() {
    let compositor = Compositor::start();
    let binds = Arc::new(AtomicUsize::new(0));
    let nobody = plant!(
        compositor,
        binds,
        [
            XdgActivationV1,
            ZwpPrimarySelectionDeviceManagerV1,
            ZwlrDataControlManagerV1,
            ExtDataControlManagerV1,
            ExtImageCopyCaptureManagerV1,
            ExtOutputImageCaptureSourceManagerV1,
            ExtForeignToplevelImageCaptureSourceManagerV1,
            ZwlrScreencopyManagerV1,
            ZwlrExportDmabufManagerV1,
            ZwpVirtualKeyboardManagerV1,
            ZwlrVirtualPointerManagerV1,
            ZwpInputMethodManagerV2,
            ZwpInputMethodV1,
            ZwpInputPanelV1,
            ZwpTextInputManagerV3,
            ZwpTextInputManagerV1,
            ExtSessionLockManagerV1,
            ExtForeignToplevelListV1,
            ZwlrForeignToplevelManagerV1,
            ZwpKeyboardShortcutsInhibitManagerV1,
            ZwpPointerConstraintsV1,
            ZwpRelativePointerManagerV1,
            ZwpTabletManagerV2,
            WpDrmLeaseDeviceV1,
            XwaylandShellV1,
        ]
    );
    // The test plants exactly the row, so a global added to the row without
    // a planted twin fails here.
    assert_eq!(nobody, policy::NOBODY);
    // The spike offered the first of these. The table has no row for either.
    let no_row = plant!(compositor, binds, [ZxdgOutputManagerV1, WpSinglePixelBufferManagerV1]);

    let clients = EveryClient::of(&compositor);
    for (who, connect) in clients.each() {
        let offered = connect().interfaces();
        for interface in nobody.iter().chain(&no_row) {
            assert!(
                !offered.iter().any(|name| name == interface),
                "{who} was offered {interface}"
            );
            assert_eq!(
                guess_everywhere(&*connect, interface),
                [0; 0],
                "{who} bound {interface} by guessing its number"
            );
        }
    }
    assert_eq!(
        binds.load(Ordering::SeqCst),
        0,
        "a client bound a global nobody is offered"
    );
}

/// The test above passes only if a planted global can be bound at all. This
/// one has a row, and the client the row names binds it by the same guess.
#[test]
fn a_planted_global_with_a_row_is_bound_by_the_client_the_row_names() {
    use smithay::reexports::wayland_protocols::wp::cursor_shape::v1::server::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
    let compositor = Compositor::start();
    let binds = Arc::new(AtomicUsize::new(0));
    let (before, _listener) = compositor.compartment("before");
    let cursor_shape = plant!(compositor, binds, [WpCursorShapeManagerV1])[0];
    let listener = compositor.register("test.principal", "after");
    let warden = || compositor.warden();
    // A client that connected first was not told about the new global, so
    // for that client its number is one to guess.
    let last = before.state.globals.iter().map(|global| global.name).max().unwrap() + 8;
    let numbers = before.unadvertised(last);

    let bound: Vec<u32> = numbers
        .iter()
        .copied()
        .filter(|&name| connect(&listener.path).unwrap().guess(name, cursor_shape, 1).unwrap() == Guess::Bound)
        .collect();

    assert_eq!(bound.len(), 1, "the compartment did not bind the global its row offers");
    assert_eq!(binds.load(Ordering::SeqCst), 1);
    assert_eq!(
        guess_everywhere(&warden, cursor_shape),
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
/// security-context manager, layer shell, and each input and capture global.
///
/// The input and capture globals are in the "Nobody" row, and the test of
/// ADR 0020's attack 1 covers them. These are the globals that exist and
/// that another client is offered.
#[test]
fn adr_0016_attack_12_a_client_on_a_listener_cannot_bind_what_the_trusted_parts_are_offered() {
    let compositor = Compositor::start();
    let listener = compositor.register("test.principal", "c1");
    let client = || connect(&listener.path).unwrap();
    let number = |seen: &Client, interface: &str| {
        seen.state
            .globals
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
