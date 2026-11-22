//! GPUI's prepared source, and each app's lockfile, keep what
//! `patches/gpui-0.2.2-builds-without-warnings.patch` did (see
//! `patches/README.md`). Without it, each build of GPUI prints three
//! warnings and a fourth about `proc-macro-error2`, and two of the four name
//! code a later Rust will refuse to build.

use std::{
    fs,
    path::{Path, PathBuf},
};

fn shell() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Rust chose `f32` for these two literals by a fallback it is phasing out
/// (rust-lang/rust#154024).
#[test]
fn the_grid_columns_float_literals_say_they_are_f32() {
    let taffy = read(&shell().join("gpui-source/src/taffy.rs"));
    assert!(taffy.contains("minmax(length(0.0_f32), fr(1.0_f32))"));
    assert!(!taffy.contains("minmax(length(0.0), fr(1.0))"));
}

/// Only the X11, macOS, and Windows backends call `selected_text_range`, so
/// a build for Wayland alone warned that it is never used.
#[test]
fn the_method_wayland_does_not_call_is_marked_unused_without_x11() {
    let platform = read(&shell().join("gpui-source/src/platform.rs"));
    let (before, _) = platform
        .split_once("    fn selected_text_range(&mut self, ignore_disabled_input: bool)")
        .expect("PlatformInputHandler::selected_text_range is in src/platform.rs");
    assert!(
        before
            .trim_end()
            .ends_with("not(feature = \"x11\")\n        ),\n        allow(dead_code)\n    )]"),
        "selected_text_range has no allow(dead_code) for a build without X11"
    );
}

/// `proc-macro-error2` 2.0.1, the newest there is, re-exports a private
/// crate (rust-lang/rust#127909). Only the macro of `stacksafe` before 1.0.2
/// used it.
#[test]
fn neither_app_builds_proc_macro_error2() {
    for app in ["Cargo.lock", "../studio-settings/Cargo.lock"] {
        let lock = read(&shell().join(app));
        assert!(
            !lock.contains("name = \"proc-macro-error2\""),
            "{app} names proc-macro-error2"
        );
        assert!(
            lock.contains("name = \"stacksafe-macro\""),
            "{app} names no stacksafe-macro: is this still GPUI's lockfile?"
        );
    }
}
