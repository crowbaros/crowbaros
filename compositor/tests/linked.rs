//! The guard on what is linked into the compositor's binary (ADR 0020,
//! ADR 0028 in CrowbarOS). It reads the binary itself, not the list of
//! crates, so code that is built but not linked does not count, and code
//! that is linked cannot hide.

use std::sync::OnceLock;

use object::{Object, ObjectSymbol, read::elf::ElfFile64};

fn binary() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        let path = env!("CARGO_BIN_EXE_crowbar-compositor");
        std::fs::read(path).unwrap_or_else(|error| panic!("{path}: {error}"))
    })
}

/// The shared libraries the binary names, as the dynamic linker sees them.
fn needed() -> Vec<String> {
    use object::read::elf::Dyn;
    let file = ElfFile64::<object::Endianness>::parse(binary()).expect("the compositor is a 64-bit ELF binary");
    let (endian, data, sections) = (file.endian(), file.data(), file.elf_section_table());
    let Ok(Some((dynamic, strings_index))) = sections.dynamic(endian, data) else {
        return Vec::new();
    };
    let strings = sections
        .strings(endian, data, strings_index)
        .expect("the dynamic section has its strings");
    dynamic
        .iter()
        .filter(|entry| entry.d_tag(endian) == object::elf::DT_NEEDED)
        .filter_map(|entry| entry.string(endian, strings).ok())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

fn symbols() -> Vec<String> {
    let file = object::File::parse(binary()).unwrap();
    file.symbols()
        .filter_map(|symbol| symbol.name().ok().map(str::to_string))
        .collect()
}

/// The C runtime, and the libraries ADR 0028 allows. Mesa's EGL, GLES, and
/// GBM, libinput, and libxkbcommon join as the code that needs them lands.
const ALLOWED: &[&str] = &[
    "libc.so.6",
    "libm.so.6",
    "libgcc_s.so.1",
    "ld-linux-x86-64.so.2",
    "ld-linux-aarch64.so.1",
    "libpthread.so.0",
    "libdl.so.2",
    "librt.so.1",
    "libEGL.so.1",
    "libGLESv2.so.2",
    "libgbm.so.1",
    "libinput.so.10",
    "libxkbcommon.so.0",
];

#[test]
fn the_binary_links_only_the_libraries_adr_0028_allows() {
    let needed = needed();
    assert!(
        needed.iter().any(|name| name.starts_with("libc.so")),
        "the guard did not read the binary: {needed:?}"
    );
    let foreign: Vec<&String> = needed.iter().filter(|name| !ALLOWED.contains(&name.as_str())).collect();
    assert!(foreign.is_empty(), "the compositor links {foreign:?}");
}

#[test]
fn the_binary_holds_no_smithay_or_wayland_rs_code() {
    let symbols = symbols();
    assert!(
        symbols.iter().any(|symbol| symbol.contains("crowbar_compositor")),
        "the guard did not read the binary's symbols"
    );
    for forbidden in [
        "smithay",
        "wayland_server",
        "wayland_backend",
        "calloop",
        "libseat",
        "x11rb",
        "reis",
    ] {
        let found: Vec<&String> = symbols.iter().filter(|symbol| symbol.contains(forbidden)).collect();
        assert!(found.is_empty(), "the binary holds {forbidden}: {}", found[0]);
    }
}

/// The compositor looks for no socket of its own (ADR 0032): it is given
/// the path of each, and Base links the apps' socket where apps look for it.
/// A compositor that chose its own would look for where to put it. The nested backend, which Base never
/// builds, looks for its host's socket the same way.
#[cfg(not(feature = "nested"))]
#[test]
fn the_binary_never_looks_for_where_a_public_socket_would_go() {
    let bytes = binary();
    for name in [
        &b"XDG_RUNTIME_DIR"[..],
        b"WAYLAND_DISPLAY",
        b"WAYLAND_SOCKET",
        b"wayland-0",
    ] {
        assert!(
            !bytes.windows(name.len()).any(|window| window == name),
            "the binary names {}",
            String::from_utf8_lossy(name)
        );
    }
}
