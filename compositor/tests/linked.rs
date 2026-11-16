//! The guard on what is linked (ADR 0020).
//!
//! The compositor's binary must hold no libei, XWayland, or X11-backend
//! code. The list of crates does not show this: Smithay depends on the `reis`
//! crate without a feature gate, so the crate is built even though none of
//! its code is linked. These tests read the symbols of the binary itself.

use std::sync::OnceLock;

use object::{Object, ObjectSymbol};

/// Every symbol in the compositor's binary, demangled.
fn symbols() -> &'static [String] {
    static SYMBOLS: OnceLock<Vec<String>> = OnceLock::new();
    SYMBOLS.get_or_init(|| {
        let path = env!("CARGO_BIN_EXE_crowbar-compositor");
        let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{path}: {error}"));
        let file = object::File::parse(&*bytes).unwrap_or_else(|error| panic!("{path}: {error}"));
        file.symbols()
            .filter_map(|symbol| symbol.name().ok())
            .map(|name| format!("{:#}", rustc_demangle::demangle(name)))
            .collect()
    })
}

/// Whether a symbol names something from this crate or module: its own
/// code, or other code made for one of its types.
fn is_from(symbol: &str, module: &str) -> bool {
    symbol.match_indices(module).any(|(at, _)| {
        let starts_a_path = !symbol[..at]
            .chars()
            .next_back()
            .is_some_and(|before| before.is_alphanumeric() || before == '_' || before == ':');
        starts_a_path && symbol[at + module.len()..].starts_with("::")
    })
}

fn linked_from(module: &str) -> Vec<&'static String> {
    symbols().iter().filter(|symbol| is_from(symbol, module)).collect()
}

#[track_caller]
fn assert_not_linked(module: &str) {
    let found = linked_from(module);
    assert!(
        found.is_empty(),
        "the binary holds {} symbols from {module}, such as {}",
        found.len(),
        found[0]
    );
}

/// If the binary were stripped, or the reading of it broke, every test below
/// would pass for the wrong reason.
#[test]
fn the_guard_sees_what_the_binary_does_hold() {
    for module in ["crowbar_compositor", "smithay::backend::drm", "smithay::backend::winit"] {
        assert!(!linked_from(module).is_empty(), "no symbol from {module} was found");
    }
}

/// libei is how a program asks a compositor to type and point for it.
#[test]
fn the_binary_holds_no_libei_code() {
    assert_not_linked("reis");
    assert_not_linked("smithay::backend::libei");
}

#[test]
fn the_binary_holds_no_xwayland_code() {
    assert_not_linked("smithay::xwayland");
    assert_not_linked("smithay::wayland::xwayland_shell");
    assert_not_linked("smithay::wayland::xwayland_keyboard_grab");
}

#[test]
fn the_binary_holds_no_x11_backend_code() {
    assert_not_linked("smithay::backend::x11");
}

/// The compositor has no public socket, and no flag or build option that
/// opens one (ADR 0016). Smithay's and wayland-server's code for a socket
/// that any process can find is not in the binary at all.
#[test]
fn the_binary_holds_no_code_that_opens_a_public_socket() {
    assert_not_linked("smithay::wayland::socket");
    assert_not_linked("wayland_server::socket");
}

#[test]
fn a_symbol_is_from_a_module_only_when_the_module_starts_a_path_in_it() {
    assert!(is_from("reis::ei::Context::new", "reis"));
    assert!(is_from("<alloc::vec::Vec<reis::event::Event>>::push", "reis"));
    assert!(is_from(
        "<smithay::backend::x11::X11Backend>::new",
        "smithay::backend::x11"
    ));
    // Other code that was only compiled inside the crate, and names nothing
    // of the crate's.
    assert!(!is_from(
        "<hashbrown::map::HashMap<u32, ()>>::contains_key::<u32>",
        "reis"
    ));
    assert!(!is_from("foreign::reis::thing", "reis"));
    assert!(!is_from("unreis::thing", "reis"));
    assert!(!is_from(
        "smithay::backend::x11rb_helper::thing",
        "smithay::backend::x11"
    ));
}
