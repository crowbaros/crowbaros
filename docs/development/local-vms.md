# Void development environments

Void Linux is the active Base and compositor target. The
[Fedora VM workflow](../archive/fedora-workflow/README.md) is archived. Its
scripts, RPM recipes, and session hooks are no longer active entry points.

## What is available in this checkout

| Environment | Current capability | State directory |
| --- | --- | --- |
| [Base](../../base/README.md) | The one Void build path and disk image, with the desktop on it; `base/void_base.py boot --window` shows it | `.lab/base/` |
| [Compositor check](../../lab/void/README.md#the-compositor-check) | A copy of Base's image per run, booted under UEFI; Fulcrum on a TTY with protocol, socket, input, idle, and hotplug checks | `.lab/void-compositor-checks/<run>/` |
| [Desktop check](../../lab/void/README.md#the-desktop-check) | A copy of Base's image per run, booted under UEFI; all Base boot checks plus launch/focus/workspaces, Save/Undo, restart/reboot, and identity checks | `.lab/void-desktop-checks/<run>/` |

The [desktop acceptance record](void-desktop-smoke-2026-11-15.md) covers the
integrated desktop services and saved appearance as they were on that date.
Today three accounts run the desktop, and every app runs as the person
([ADR 0032](../adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)).

From the repository root, inspect Base's commands or an already-running Base
guest without rebuilding its disk:

```sh
base/void_base.py --help
base/void_base.py ssh cat /etc/os-release
```

To run the desktop check (including all Base boot checks) or the compositor
check:

```sh
lab/void/desktop_check.py
lab/void/compositor_check.py
```

Both build Base's image with `base/void_base.py build --lab-account`, which
keeps its Void roots and Cargo target in `.lab/base/` as a cache. Each run
then boots its own grown copy of the image under UEFI, with its own firmware
variables, SSH port, and Unix VNC and QMP sockets, retains logs and
screenshots, and powers off its guest. Neither touches Base's own disk
(`.lab/base/disk.raw`) or previous runs. Do not run checks or builds
concurrently: they share Base's build directory, `.lab/base/`, and both checks
overwrite `.lab/base/base.raw` with the lab account, so run
`base/void_base.py build` again before writing `.lab/base/base.raw` to a real
disk. See the [lab guide](../../lab/void/README.md) for prerequisites and
diagnostics. Credentials and disks stay out of published evidence.

## Build the interfaces on Void

Each app has its own Cargo project and lockfile. Build in a dedicated Void glibc
development machine or build root, not the person's running desktop. The tested
development packages include `gcc`, `rust`, `cargo`, `pkg-config`, `patch`,
`libxkbcommon-devel`, `libxkbcommon-x11`, `wayland-devel`, `fontconfig-devel`, and
`freetype-devel`. The full build package list is in
[`base/config.json`](../../base/config.json). Runtime rendering needs a
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

The desktop runs the [compositor](../../compositor/README.md), Fulcrum.

For a same-user rendering experiment inside an existing **development** Wayland
session, run Fulcrum's nested backend and start an app at its socket for apps.
This does not exercise the desktop's separate accounts or its launcher:

```sh
# From the repository root:
cargo run --manifest-path compositor/Cargo.toml --release --features nested -- --backend nested \
  --shell-socket /tmp/fulcrum/shell --shell-uid "$(id -u)" \
  --apps-socket /tmp/fulcrum/apps --apps-uid "$(id -u)"
# Then:
WAYLAND_DISPLAY=/tmp/fulcrum/apps CROWBAR_SESSION=crowbaros \
  "$PWD/apps/studio-settings/target/release/crowbaros-studio-settings"
```

For a dedicated Void TTY, use the operator-led procedure and prerequisites in
the [NVIDIA record](../research/compositor-nvidia-2026-11-15.md). It includes
seatd, runtime-directory ownership, driver setup, and suspend hooks. Do not take
over a TTY/GPU used by another active session.

## What the desktop does not use

The shell's Hyprland and systemd fallbacks and Settings' Hyprland adapter are
from before Void. The desktop's services do not use them. The launcher starts
apps as the person, at the compositor's ordinary socket for apps. Settings Save
works without Hyprland, and the shell follows saved preferences. The compositor
has no animations; the motion preference is stored for other desktops
without claiming an effect here. Archived Fedora service files are not a
supported alternative session.
