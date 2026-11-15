# CrowbarOS GPUI probe

> **Status.** This app was built for the earlier Fedora plan. Its interface work
> carries over to the [vision](../../docs/VISION.md); its RPM packaging and Hyprland
> wiring do not. It is built and run only in the [Fedora lab](../../lab/fedora/README.md),
> which is kept for interface work. See [what carries over](../README.md).

A small native Rust application for validating the Fedora → RPM → Hyprland/Wayland
path. It uses **GPUI 0.2.2**, with its `wayland` feature enabled and default/X11
features disabled. The renderer is GPUI's Vulkan/Blade backend. There is no browser
or webview and no application network operation.

The probe includes an interaction counter, three keyboard-focusable buttons,
Crowbar/neutral Studio palettes, a 40-row scroll region, bundled Manrope and
IBM Plex Mono fonts, and a resizable native window. It is a development tool,
not the production shell or settings app.

The [November 14 validation record](../../docs/development/gpui-validation-2026-11-14.md)
contains the tested RPM, actual screenshots, passed checks, fixes and remaining limits.
The later [hardware Vulkan validation](../../docs/development/gpu-validation-2026-11-14.md)
repeats the probe checks through the host Intel GPU.

## Build and install through the VM lab

From the repository root, after [setting up the lab](../../docs/development/local-vms.md):

```sh
rtk python3 lab/fedora/lab.py stop test
rtk python3 lab/fedora/lab.py start build
rtk python3 lab/fedora/lab.py wait build
rtk python3 lab/fedora/lab.py view build &
rtk python3 lab/fedora/lab.py gpui-build build
rtk python3 lab/fedora/lab.py stop build
rtk python3 lab/fedora/lab.py start test
rtk python3 lab/fedora/lab.py wait test
rtk python3 lab/fedora/lab.py view test &
rtk python3 lab/fedora/lab.py gpui-deploy test
rtk python3 lab/fedora/lab.py gpui-run test
```

The build script vendors the locked dependencies, retains their license files,
creates an SRPM, and builds in a clean Fedora 45 Mock environment. The actual
Cargo build uses `--frozen` and offline mode. It produces a signed RPM and signed
repository metadata using the lab's development-only key. The test guest verifies
both signatures through DNF. Fedora repositories supply runtime dependencies;
the test guest does not need Rust or Cargo.

The host receives `.local/vms/gpui-repo.tar.gz`. The builder retains the generated
spec, source archive, SRPM, RPM, and logs under `~/artifacts/gpui/`.
The package installs `/usr/bin/crowbaros-gpui-probe`, a desktop launcher/icon,
font and dependency licenses, dependency metadata, and a build manifest.

## Controls

| Input | Action |
| --- | --- |
| Click Increment / Ctrl+I | Increment the counter |
| Tab / Shift+Tab | Move visible keyboard focus between buttons |
| Enter / Space | Activate the focused button |
| Use Studio palette / Ctrl+T | Toggle neutral Studio colors |
| Reset count / Ctrl+R | Reset the counter |
| Wheel over the list | Scroll the text samples |
| Ctrl+Q / close the window | Quit the application |

Launch `crowbaros-gpui-probe` from a terminal **inside the test desktop**, or use
its desktop entry when a launcher is available. `--version` is safe without a
display. Logs beginning `GPUI_PROBE` describe real input callbacks for integration
verification. The app does not continuously request animation frames while idle.

`gpui-run` discovers the active test Wayland socket and starts the app as the
ordinary `desktop` user in a transient user service. It does not run the GUI as
root. Its logs are available with:

```sh
rtk python3 lab/fedora/lab.py ssh test sudo journalctl -b _SYSTEMD_USER_UNIT=crowbaros-gpui-probe.service --no-pager
```

After [installing the Intel VirtIO lab driver](../../docs/development/gpu-vulkan.md),
use `gpui-run test intel-virtio` to force hardware Vulkan, or `gpui-run test software`
to force lavapipe. Quit the existing probe before changing drivers. With no last
argument, the command uses `gpui_vulkan_driver` from the lab configuration.
The ordinary desktop launcher still uses system driver discovery.

## Dependency decisions

- `gpui = "=0.2.2"` is the published registry release used by this probe. Current
  upstream `main` has a different, split platform API. These results apply to the
  recorded release and lockfile, not every GPUI revision.
- `libc = "=0.2.189"` holds the Rust FFI crate at a version compatible with
  GPUI → `gpui_http_client` → `zed-async-tar` → `xattr 0.2.3`. The latter references
  Linux's obsolete `ENOATTR` alias, removed by libc 0.2.190. This does not downgrade
  Fedora's glibc. Revisit the constraint when upgrading GPUI or its tar dependency.
- `Cargo.lock` records all dependency versions/checksums. Build manifests record
  the compiler, Cargo, lockfile/source digests, and font digests.
- Manrope and IBM Plex Mono are embedded from the approved brand kit with OFL
  notices. The app uses [static Manrope weights](fonts/README.md) because the
  variable source rendered as ExtraLight in this GPUI Linux stack. GPUI and
  other third-party license files are retained in the RPM.
- Focused `on_click` controls already receive Enter/Space activation from GPUI.
  Registering another activation binding makes the action fire twice. The probe
  relies on GPUI's built-in activation and tests for exactly one state change.
- This disposable integration fixture uses MIT; it does not set the license
  policy for CrowbarOS's future shell or product applications.

## Repeat the input check

Keep the test viewer open, then run from the repository root:

```sh
rtk python3 lab/fedora/check_gpui.py
rtk python3 lab/fedora/lab.py gpui-run test
```

The check restarts only the probe, drives real guest keyboard/pointer events,
checks single activation, focus, theme changes and quit, and captures rendering
at 820×700 and 640×600. It expects the lab's single 1280×800, scale-1 display and
desktop user/session. Evidence is saved under `.local/gpui/validation/`; inspect
the screenshots as well as the command's result. Avoid simultaneous manual input
during this short check. The second command leaves the app open again.

For hardware validation, pass `--vulkan-driver intel-virtio --output .local/gpu/gpui-validation`
to `check_gpui.py`. This also verifies the private Intel library is loaded and
lavapipe is absent. The selected driver must already be installed.

## Scope of validation

This probe can establish application launch, visible rendering, elementary input,
focus, scrolling, theme redraw, resizing, and package deployment. It does not
implement a production text field, IME integration, screen-reader semantics,
layer-shell panels, secure locking, portals, or multi-monitor behavior. The
software and Intel VirtIO passes do not establish GPU performance, idle power, or color
accuracy. Physical hardware validation remains a separate milestone.

API reference: [GPUI 0.2.2 documentation](https://docs.rs/gpui/0.2.2/gpui/).
Use this version's examples when reproducing the probe; upstream `main` has
already changed its application/platform initialization API.
