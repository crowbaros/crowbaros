//! The guard on what Fulcrum is built from (ADR 0028 in CrowbarOS).
//!
//! Fulcrum depends on no crate from the Smithay organization. This reads
//! `Cargo.lock`, which names every crate the build can use, and fails on any
//! of them.

use std::path::Path;

/// Crates published from the Smithay organization's repositories: Smithay
/// itself, wayland-rs, drm-rs, gbm.rs, input.rs, calloop, udev-rs,
/// libseat-rs, and the client toolkit.
const SMITHAY_CRATES: &[&str] = &[
    "smithay",
    "smithay-drm-extras",
    "smithay-client-toolkit",
    "smithay-clipboard",
    "wayland-backend",
    "wayland-client",
    "wayland-server",
    "wayland-protocols",
    "wayland-protocols-wlr",
    "wayland-protocols-misc",
    "wayland-protocols-plasma",
    "wayland-protocols-experimental",
    "wayland-scanner",
    "wayland-sys",
    "wayland-cursor",
    "wayland-egl",
    "drm",
    "drm-ffi",
    "drm-sys",
    "gbm",
    "gbm-sys",
    "input",
    "input-sys",
    "calloop",
    "calloop-wayland-source",
    "udev",
    "libseat",
    "libseat-sys",
];

fn locked_packages() -> Vec<String> {
    let lock = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock"))
        .expect("Cargo.lock is committed");
    lock.lines()
        .filter_map(|line| line.strip_prefix("name = \""))
        .filter_map(|rest| rest.strip_suffix('"'))
        .map(str::to_string)
        .collect()
}

#[test]
fn no_crate_from_the_smithay_organization_is_used() {
    let packages = locked_packages();
    assert!(
        packages.iter().any(|name| name == "fulcrum-wire"),
        "Cargo.lock was not read"
    );
    let found: Vec<&String> = packages
        .iter()
        .filter(|name| SMITHAY_CRATES.contains(&name.as_str()))
        .collect();
    assert!(found.is_empty(), "crates from the Smithay organization: {found:?}");
}
