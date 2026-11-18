//! xdg-shell windows and popups: the configure handshake, where windows open,
//! what an output shows, and the errors the protocol gives.

mod common;

use common::Compositor;
use crowbar_compositor::{NEW_WINDOW_OFFSET, identity::Identity};
use crowbar_probe::{Client, Toplevel};
use fulcrum_wire::{
    ObjectId,
    protocol::{
        wayland::{wl_output, wl_surface},
        xdg_shell::{xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base},
    },
};

const RED: u32 = 0xffff_0000;
const BLUE: u32 = 0xff00_00ff;
const GREEN: u32 = 0xff00_ff00;

fn next() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn client(compositor: &Compositor) -> (Client, crowbar_probe::CompartmentListener) {
    compositor.compartment(&format!("w{}", next()))
}

#[test]
fn a_window_opens_inside_the_usable_area_and_the_next_one_steps_down() {
    let compositor = Compositor::start();
    let (mut client, _listener) = client(&compositor);
    client.open_toplevel("one", "test.one", (100, 60), RED).unwrap();
    client.open_toplevel("two", "test.two", (100, 60), BLUE).unwrap();
    compositor.wait_until_still();

    let screen = compositor.screen(0);
    let (x, y) = NEW_WINDOW_OFFSET;
    assert_eq!(screen.pixel(x, y), Some(RED));
    assert_eq!(
        screen.pixel(x + 32, y + 32),
        Some(BLUE),
        "the second window was not on top, 32 down"
    );
    assert_eq!(screen.pixel(x - 1, y), Some(crowbar_compositor::BACKGROUND));
    assert_eq!(
        compositor.with(|state| state.window_rects()),
        [
            crowbar_compositor::geometry::Rect::new(x, y, 100, 60),
            crowbar_compositor::geometry::Rect::new(x + 32, y + 32, 100, 60)
        ]
    );
}

/// A client shadow outside its window geometry is drawn, and the geometry,
/// not the shadow, is what is placed.
#[test]
fn the_window_geometry_is_what_is_placed() {
    let compositor = Compositor::start();
    let (mut client, _listener) = client(&compositor);
    let wm_base = client.bind(xdg_wm_base::NAME, 6).unwrap();
    let surface = client.create_surface().unwrap();
    let xdg = client.new_id(xdg_surface::NAME);
    client
        .request(
            wm_base,
            xdg_wm_base::Request::GetXdgSurface {
                id: xdg,
                surface: ObjectId(surface),
            },
        )
        .unwrap();
    let toplevel = client.new_id(xdg_toplevel::NAME);
    client
        .request(xdg.0, xdg_surface::Request::GetToplevel { id: toplevel })
        .unwrap();
    client
        .request(
            xdg.0,
            xdg_surface::Request::SetWindowGeometry {
                x: 10,
                y: 10,
                width: 80,
                height: 40,
            },
        )
        .unwrap();
    client.request(surface, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();
    let buffer = client.shm_buffer((100, 60), GREEN).unwrap();
    client.show(surface, Some(&buffer)).unwrap();
    client.roundtrip().unwrap();

    let (x, y) = NEW_WINDOW_OFFSET;
    assert_eq!(
        compositor.with(|state| state.window_rects()),
        [crowbar_compositor::geometry::Rect::new(x, y, 80, 40)]
    );
    let screen = compositor.screen(0);
    // The shadow, outside the geometry, is drawn too.
    assert_eq!(screen.pixel(x - 10, y - 10), Some(GREEN));
    assert_eq!(screen.pixel(x - 11, y - 10), Some(crowbar_compositor::BACKGROUND));
}

#[test]
fn a_window_is_told_which_output_it_is_on() {
    let compositor = Compositor::start();
    let (mut client, _listener) = client(&compositor);
    let output = client.bind(wl_output::NAME, 4).unwrap();
    let window = client.open_toplevel("enter", "test.enter", (50, 50), RED).unwrap();
    client.roundtrip().unwrap();
    let enter = client.take_event(window.surface, "enter").expect("no wl_surface.enter");
    assert!(matches!(enter.args.as_slice(), [fulcrum_wire::Arg::Object(Some(ObjectId(id)))] if *id == output));
}

#[test]
fn a_null_buffer_unmaps_and_the_client_starts_again_from_its_first_commit() {
    let compositor = Compositor::start();
    let (mut client, _listener) = client(&compositor);
    let window = client.open_toplevel("gone", "test.gone", (50, 50), RED).unwrap();
    client.show(window.surface, None).unwrap();
    client.roundtrip().unwrap();
    assert!(compositor.with(|state| state.window_rects()).is_empty());

    // A buffer straight away is a buffer before a configure.
    let buffer = client.shm_buffer((50, 50), RED).unwrap();
    client.show(window.surface, Some(&buffer)).unwrap();
    assert!(client.roundtrip().is_err());
    assert_eq!(
        client.error.as_ref().unwrap().code,
        xdg_surface::error::UNCONFIGURED_BUFFER
    );
}

/// The compartment a window belongs to is the one whose listener its client
/// came through, and nothing the client calls itself changes that.
#[test]
fn a_window_belongs_to_its_clients_compartment_whatever_it_calls_itself() {
    let compositor = Compositor::start();
    let mail = compositor.register("org.example.Mail", "compartment-1");
    let mut client = common::connect(&mail.path).unwrap();
    client
        .open_toplevel("Notes", "org.example.Notes", (50, 50), RED)
        .unwrap();

    let owners = compositor.with(|state| {
        state
            .window_owners()
            .into_iter()
            .map(|owner| match owner {
                Identity::Compartment(compartment) => compartment.principal().to_string(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(owners, ["org.example.Mail"]);
    let asked = compositor.with(|state| state.window_requests());
    assert_eq!(
        asked[0].app_id, "org.example.Notes",
        "the window's own name is kept, as a name"
    );
}

/// Expects the compositor to refuse what `send` does with `code`.
fn refused(compositor: &Compositor, code: u32, send: impl FnOnce(&mut Client)) {
    let (mut client, _listener) = client(compositor);
    send(&mut client);
    assert!(client.roundtrip().is_err(), "the compositor accepted it");
    let error = client.error.clone().expect("a protocol error");
    assert_eq!(error.code, code, "{error:?}");
    compositor.shell().roundtrip().unwrap();
}

/// An `xdg_surface` and its toplevel, not yet committed.
fn bare_toplevel(client: &mut Client) -> Toplevel {
    let wm_base = client.bind(xdg_wm_base::NAME, 6).unwrap();
    let surface = client.create_surface().unwrap();
    let xdg = client.new_id(xdg_surface::NAME);
    client
        .request(
            wm_base,
            xdg_wm_base::Request::GetXdgSurface {
                id: xdg,
                surface: ObjectId(surface),
            },
        )
        .unwrap();
    let toplevel = client.new_id(xdg_toplevel::NAME);
    client
        .request(xdg.0, xdg_surface::Request::GetToplevel { id: toplevel })
        .unwrap();
    Toplevel {
        surface,
        xdg_surface: xdg.0,
        toplevel: toplevel.0,
    }
}

#[test]
fn the_xdg_shell_errors() {
    let compositor = Compositor::start();
    // A buffer before the first configure.
    refused(&compositor, xdg_surface::error::UNCONFIGURED_BUFFER, |client| {
        let window = bare_toplevel(client);
        let buffer = client.shm_buffer((10, 10), RED).unwrap();
        client.show(window.surface, Some(&buffer)).unwrap();
    });
    // A commit before the xdg_surface has a role.
    refused(&compositor, xdg_surface::error::NOT_CONSTRUCTED, |client| {
        let wm_base = client.bind(xdg_wm_base::NAME, 6).unwrap();
        let surface = client.create_surface().unwrap();
        let xdg = client.new_id(xdg_surface::NAME);
        client
            .request(
                wm_base,
                xdg_wm_base::Request::GetXdgSurface {
                    id: xdg,
                    surface: ObjectId(surface),
                },
            )
            .unwrap();
        client.request(surface, wl_surface::Request::Commit).unwrap();
    });
    // An acknowledgement of a serial never sent.
    refused(&compositor, xdg_surface::error::INVALID_SERIAL, |client| {
        let window = bare_toplevel(client);
        client
            .request(
                window.xdg_surface,
                xdg_surface::Request::AckConfigure { serial: 999_999 },
            )
            .unwrap();
    });
    // The xdg_surface destroyed before its toplevel.
    refused(&compositor, xdg_surface::error::DEFUNCT_ROLE_OBJECT, |client| {
        let window = bare_toplevel(client);
        client
            .request(window.xdg_surface, xdg_surface::Request::Destroy)
            .unwrap();
    });
    // The wm_base destroyed before its surfaces.
    refused(&compositor, xdg_wm_base::error::DEFUNCT_SURFACES, |client| {
        bare_toplevel(client);
        let wm_base = client.bind(xdg_wm_base::NAME, 6).unwrap();
        let surface = client.create_surface().unwrap();
        let xdg = client.new_id(xdg_surface::NAME);
        client
            .request(
                wm_base,
                xdg_wm_base::Request::GetXdgSurface {
                    id: xdg,
                    surface: ObjectId(surface),
                },
            )
            .unwrap();
        client.request(wm_base, xdg_wm_base::Request::Destroy).unwrap();
    });
    // A surface with a role already.
    refused(&compositor, xdg_wm_base::error::ROLE, |client| {
        let parent = client.create_surface().unwrap();
        let child = client.create_surface().unwrap();
        client.subsurface(child, parent).unwrap();
        let wm_base = client.bind(xdg_wm_base::NAME, 6).unwrap();
        let xdg = client.new_id(xdg_surface::NAME);
        client
            .request(
                wm_base,
                xdg_wm_base::Request::GetXdgSurface {
                    id: xdg,
                    surface: ObjectId(child),
                },
            )
            .unwrap();
    });
    // A toplevel as its own ancestor.
    refused(&compositor, xdg_toplevel::error::INVALID_PARENT, |client| {
        let a = bare_toplevel(client);
        let b = bare_toplevel(client);
        client
            .request(
                a.toplevel,
                xdg_toplevel::Request::SetParent {
                    parent: Some(ObjectId(b.toplevel)),
                },
            )
            .unwrap();
        client
            .request(
                b.toplevel,
                xdg_toplevel::Request::SetParent {
                    parent: Some(ObjectId(a.toplevel)),
                },
            )
            .unwrap();
    });
}

/// A popup with its positioner's size and anchor rectangle, from its
/// parent.
fn popup(client: &mut Client, parent: &Toplevel, anchor: (i32, i32, i32, i32), size: (i32, i32)) -> (u32, u32, u32) {
    let wm_base = client.bind(xdg_wm_base::NAME, 6).unwrap();
    let positioner = client.new_id(xdg_positioner::NAME);
    client
        .request(wm_base, xdg_wm_base::Request::CreatePositioner { id: positioner })
        .unwrap();
    client
        .request(
            positioner.0,
            xdg_positioner::Request::SetSize {
                width: size.0,
                height: size.1,
            },
        )
        .unwrap();
    client
        .request(
            positioner.0,
            xdg_positioner::Request::SetAnchorRect {
                x: anchor.0,
                y: anchor.1,
                width: anchor.2,
                height: anchor.3,
            },
        )
        .unwrap();
    client
        .request(
            positioner.0,
            xdg_positioner::Request::SetAnchor {
                anchor: xdg_positioner::anchor::BOTTOM_LEFT,
            },
        )
        .unwrap();
    client
        .request(
            positioner.0,
            xdg_positioner::Request::SetGravity {
                gravity: xdg_positioner::gravity::BOTTOM_RIGHT,
            },
        )
        .unwrap();
    let surface = client.create_surface().unwrap();
    let xdg = client.new_id(xdg_surface::NAME);
    client
        .request(
            wm_base,
            xdg_wm_base::Request::GetXdgSurface {
                id: xdg,
                surface: ObjectId(surface),
            },
        )
        .unwrap();
    let popup = client.new_id(xdg_popup::NAME);
    client
        .request(
            xdg.0,
            xdg_surface::Request::GetPopup {
                id: popup,
                parent: Some(ObjectId(parent.xdg_surface)),
                positioner: ObjectId(positioner.0),
            },
        )
        .unwrap();
    client.request(surface, wl_surface::Request::Commit).unwrap();
    client.roundtrip().unwrap();
    (surface, xdg.0, popup.0)
}

#[test]
fn a_popup_shows_where_its_positioner_puts_it_and_goes_with_its_parent() {
    let compositor = Compositor::start();
    let (mut client, _listener) = client(&compositor);
    let window = client.open_toplevel("menu", "test.menu", (200, 100), RED).unwrap();
    // Anchored at the bottom-left corner of a 20x10 rectangle at (30,40), and
    // laid out down and to the right from it.
    let (surface, _, popup_object) = popup(&mut client, &window, (30, 40, 20, 10), (60, 30));
    let configure = client
        .take_event(popup_object, "configure")
        .expect("the popup was configured");
    assert!(matches!(
        configure.args.as_slice(),
        [
            fulcrum_wire::Arg::Int(30),
            fulcrum_wire::Arg::Int(50),
            fulcrum_wire::Arg::Int(60),
            fulcrum_wire::Arg::Int(30)
        ]
    ));
    let buffer = client.shm_buffer((60, 30), BLUE).unwrap();
    client.show(surface, Some(&buffer)).unwrap();
    client.roundtrip().unwrap();

    let (x, y) = NEW_WINDOW_OFFSET;
    let screen = compositor.screen(0);
    assert_eq!(screen.pixel(x + 30, y + 50), Some(BLUE));
    assert_eq!(screen.pixel(x + 29, y + 50), Some(RED));

    // The window goes: its popup is told it is done.
    client.request(window.toplevel, xdg_toplevel::Request::Destroy).unwrap();
    client
        .request(window.xdg_surface, xdg_surface::Request::Destroy)
        .unwrap();
    client.request(window.surface, wl_surface::Request::Destroy).unwrap();
    client.roundtrip().unwrap();
    assert!(client.take_event(popup_object, "popup_done").is_some());
}

#[test]
fn a_popup_below_another_cannot_be_destroyed_first() {
    let compositor = Compositor::start();
    refused(&compositor, xdg_wm_base::error::NOT_THE_TOPMOST_POPUP, |client| {
        let window = client.open_toplevel("menus", "test.menus", (200, 100), RED).unwrap();
        let (_, first_xdg, first) = popup(client, &window, (0, 0, 10, 10), (20, 20));
        let parent = Toplevel {
            surface: 0,
            xdg_surface: first_xdg,
            toplevel: 0,
        };
        popup(client, &parent, (0, 0, 10, 10), (20, 20));
        client.request(first, xdg_popup::Request::Destroy).unwrap();
    });
}
