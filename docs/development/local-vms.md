# Void development environments

Void Linux is the active Base and compositor target. The
[Fedora VM workflow](../archive/fedora-workflow/README.md) is archived. Its
scripts, RPM recipes, and session hooks are no longer active entry points.

## What is available in this checkout

| Environment | Current capability | State directory |
| --- | --- | --- |
| [Graphical lab](../../lab/void/README.md) | The compositor on a TTY, with the shell and an app at their own sockets; software rendering | `.lab/void-graphical/` |
| [Base](../../base/README.md) | The desktop on Base's image; `base/void_base.py boot --window` shows it | `.lab/base/` |
| Desktop check | A copy of Base's image per run, booted under UEFI; launch/focus/workspaces, Save/Undo, restart/reboot, identity checks | `.lab/void-desktop-checks/<run>/` |

The [desktop acceptance record](void-desktop-smoke-2026-11-15.md) covers the
integrated desktop services and saved appearance as they were on that date.
Today three accounts run the desktop, and every app runs as the person
([ADR 0032](../adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)).

From the repository root, inspect the available commands or an already-running
guest without rebuilding its disk:

```sh
rtk python3 lab/void/compositor_guest.py --help
rtk python3 lab/void/compositor_guest.py ssh cat /etc/os-release
```

To validate the desktop without replacing an existing VM:

```sh
rtk python3 lab/void/compositor_guest.py desktop-check
```

It builds Base's image with `base/void_base.py build`, which keeps its Void
roots and Cargo target in `.lab/base/` as a cache. Each run then boots its own
copy of the image under UEFI, with its own firmware variables, SSH port, and
Unix VNC sockets, retains logs/screenshots/results, and powers off its guest.
It never selects Base's disk or the lab's development disk. Do not build
concurrently: runs share Base's build directory.
See the [lab guide](../../lab/void/README.md#the-desktop-check) for prerequisites
and diagnostics. Credentials and disks stay out of published evidence.

For a **disposable checkout with no existing VM state**, the compositor check
builds or replaces its configured image:

```sh
rtk python3 lab/void/compositor_guest.py check
```

Do not use `image` or `check` to reopen a preserved development VM:
they build or replace lab state. Keep its `.lab/` files and its viewer/connection
settings. The lab supports only one run at a time at its configured paths.

## Build the interfaces on Void

Each app has its own Cargo project and lockfile. Build in a dedicated Void glibc
development machine or build root, not the person's running desktop. The tested
development packages include `gcc`, `rust`, `cargo`, `pkg-config`, `patch`,
`libxkbcommon-devel`, `libxkbcommon-x11`, `wayland-devel`, `fontconfig-devel`, and
`freetype-devel`. The compositor additionally needs the development packages in
[`lab/void/config.json`](../../lab/void/config.json). Runtime rendering needs a
working Vulkan loader/driver; software Vulkan is distinct from hardware GPU
qualification.

From a repository checkout inside that Void environment (RTK is optional and
not a build dependency):

```sh
python3 apps/shell/packaging/prepare-gpui.py
cargo build --release --locked --manifest-path apps/shell/Cargo.toml
cargo test --release --locked --manifest-path apps/shell/Cargo.toml
cargo build --release --locked --manifest-path apps/studio-settings/Cargo.toml
cargo test --release --locked --manifest-path apps/studio-settings/Cargo.toml
```

The preparer verifies the pinned GPUI archive and applies the existing layer-shell
backport. It is portable build tooling and stays in `apps/shell/packaging/`.
Cargo downloads require access to the pinned dependency sources on a cold build.
Tests are headless; they do not establish correct GUI behavior or session recovery.

The Alpha desktop runs the [compositor](../../compositor/README.md), Fulcrum.

For a same-user rendering experiment inside an existing **development** Wayland
session, run Fulcrum's nested backend and start an app at its socket for apps.
This does not exercise the desktop's separate accounts or its launcher:

```sh
# From the repository root:
cargo run --manifest-path compositor/Cargo.toml --release --features nested -- --backend nested \
  --shell-socket /tmp/fulcrum/shell --shell-uid "$(id -u)" \
  --apps-socket /tmp/fulcrum/apps --apps-uid "$(id -u)"
# Then:
WAYLAND_DISPLAY=/tmp/fulcrum/apps CROWBAR_SESSION=alpha-no-animations \
  "$PWD/apps/studio-settings/target/release/crowbaros-studio-settings"
```

For a dedicated Void TTY, use the operator-led procedure and prerequisites in
the [NVIDIA record](../research/compositor-nvidia-2026-11-15.md). It includes
seatd, runtime-directory ownership, driver setup, and suspend hooks. Do not take
over a TTY/GPU used by another active session.

## Alpha limits and remaining session work

| Existing source | Remaining task |
| --- | --- |
| Shell Hyprland/systemd fallback, Settings Hyprland adapter, and spike control protocol | [#71](https://github.com/crowbaros/crowbaros/issues/71): production IPC, adapter removal, and corresponding regressions |

The desktop's services do not use those legacy adapters. The launcher starts
apps as the person, at the compositor's ordinary socket for apps. Settings Save
works without Hyprland, and the shell follows saved preferences. The compositor
has no animations; the motion preference is stored for compatible sessions
without claiming an effect here. Archived Fedora service files are not a
supported alternative session.
