# CrowbarOS GPUI probe

A small native Rust/GPUI 0.2.2 rendering and input reference. It uses Wayland
with GPUI's Vulkan/Blade renderer, with default/X11 features disabled. It is
not the production shell or Settings. The Fedora RPM workflow is
[archived](../../docs/archive/fedora-workflow/README.md).

## Build and run

Use a Void development environment with the dependencies in the
[build/run guide](../../docs/development/local-vms.md). From this directory:

```sh
cargo build --release --locked
cargo test --release --locked
target/release/crowbaros-gpui-probe --version
```

Run the binary as an ordinary user inside a development Wayland session, or
pass its absolute path to the `--spawn` option of the
[compositor spike](../../lab/compositor-spike/README.md). Rendering needs
a working Vulkan driver. The probe is optional and is not installed by the
current graphical guest builder.

The [original GPUI validation](../../docs/development/gpui-validation-2026-11-14.md)
and [Intel GPU validation](../../docs/development/gpu-validation-2026-11-14.md)
remain the evidence for those exact environments. They do not establish full
Void desktop acceptance or a supported graphics matrix.

## Controls

| Input | Action |
| --- | --- |
| Increment / Ctrl+I | Increment the counter |
| Tab / Shift+Tab | Move button focus |
| Enter / Space | Activate the focused button |
| Studio palette / Ctrl+T | Toggle neutral colors |
| Reset / Ctrl+R | Reset the count |
| Wheel over the list | Scroll the 40-row sample |
| Ctrl+Q / close | Quit |

`GPUI_PROBE` log messages describe input callbacks. The app does not continuously
request animation frames while idle. `--version` needs no display.

## Dependencies and licensing

Cargo.lock pins the dependency graph. GPUI is exactly 0.2.2; results do not apply
to every upstream revision. `libc = 0.2.189` retains the ENOATTR alias required by
the pinned xattr dependency; it pins Rust bindings, not the system's C library.
Reassess that constraint when upgrading GPUI or its archive dependencies.

Owned code is MIT. GPUI retains Apache-2.0 terms; the checked-in Manrope and IBM
Plex Mono fonts retain OFL notices and [provenance](fonts/README.md). Portable
desktop entries/icons, source, licenses, and fonts remain after RPM retirement.
