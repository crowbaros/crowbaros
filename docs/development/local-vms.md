# Void development environments

Void Linux is the active Base and compositor target. The
[Fedora VM workflow](../archive/fedora-workflow/README.md) is archived. Its
scripts, RPM recipes, and session hooks are no longer active entry points.

## What is available in this checkout

| Environment | Current capability | State directory |
| --- | --- | --- |
| [Base lab](../../lab/void/README.md) | M1 generation/apply/manual-rollback loop | `.lab/void/` |
| Graphical lab | The compositor on a TTY, checked with a stand-in for the Warden; software rendering | `.lab/void-graphical/` |
| Isolated desktop smoke | Fresh guest per run; launch/focus/workspaces, Save/Undo, restart/reboot, identity checks | `.lab/void-desktop-checks/<run>/` |
| Existing desktop preview | A previously prepared VM may already be running | Preserve its disk and recorded configuration |

The [desktop acceptance record](void-desktop-smoke-2026-11-15.md) covers the
integrated Alpha services and saved appearance. They follow ADR 0016's temporary
three-user model; production Warden and compartment isolation remain separate.

From the repository root, inspect the available commands or an already-running
guest without rebuilding its disk:

```sh
rtk python3 lab/void/void_lab.py --help
rtk python3 lab/void/compositor_guest.py --help
rtk python3 lab/void/compositor_guest.py ssh cat /etc/os-release
```

To validate the desktop without replacing an existing VM:

```sh
rtk python3 lab/void/compositor_guest.py desktop-check
# Optional: reuse only the provisioned build root and Cargo target cache.
rtk python3 lab/void/compositor_guest.py desktop-check --build-cache .lab/void-graphical
```

Each run creates its own root, disk, SSH key/port, and Unix VNC sockets, retains
logs/screenshots/results, and powers off its guest. The cache option never
selects the development disk. Do not build concurrently against the same cache.
See the [lab guide](../../lab/void/README.md#full-desktop-smoke-check) for prerequisites
and diagnostics. Credentials and disks stay out of published evidence.

For a **disposable checkout with no existing VM state**, the older full
checks build or replace their configured images:

```sh
rtk python3 lab/void/void_lab.py milestone
rtk python3 lab/void/compositor_guest.py check
```

Do not use `image`, `check`, `desktop`, or `milestone` to reopen a preserved development VM:
they build or replace lab state. Keep its `.lab/` files and its viewer/connection
settings. The Base and graphical guests use different SSH ports; each lab still
supports only one run at a time at its configured paths.

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
cargo build --release --locked --manifest-path lab/compositor-spike/Cargo.toml
cargo build --release --locked --manifest-path apps/shell/Cargo.toml
cargo test --release --locked --manifest-path apps/shell/Cargo.toml
cargo build --release --locked --manifest-path apps/studio-settings/Cargo.toml
cargo test --release --locked --manifest-path apps/studio-settings/Cargo.toml
```

The preparer verifies the pinned GPUI archive and applies the existing layer-shell
backport. It is portable build tooling and stays in `apps/shell/packaging/`.
Cargo downloads require access to the pinned dependency sources on a cold build.
Tests are headless; they do not establish correct GUI behavior or session recovery.

The Alpha desktop runs the [compositor spike](../../lab/compositor-spike/README.md).
The [compositor](../../compositor/README.md) does not run the shell yet: it has
no public socket, and the shell's protocol is
[#71](https://github.com/crowbaros/crowbaros/issues/71).

For a same-user rendering experiment inside an existing **development** Wayland
session, start the spike nested (paths assume the default Cargo target
directories). This does not exercise the separate Alpha identities or launcher:

```sh
CROWBAR_SESSION=alpha-no-animations lab/compositor-spike/target/release/crowbar-compositor \
  --shell "$PWD/apps/shell/target/release/crowbaros-shell" \
  --spawn "$PWD/apps/studio-settings/target/release/crowbaros-studio-settings"
```

For a dedicated Void TTY, use the operator-led procedure and prerequisites in
the [NVIDIA record](../research/compositor-nvidia-2026-11-15.md). It includes
seatd, runtime-directory ownership, driver setup, and suspend hooks. Do not take
over a TTY/GPU used by another active session.

## Alpha limits and remaining session work

| Existing source | Remaining task |
| --- | --- |
| Shell Hyprland/systemd fallback, Settings Hyprland adapter, and spike control protocol | [#71](https://github.com/crowbaros/crowbaros/issues/71): production IPC, adapter removal, and corresponding regressions |
| Public ordinary Wayland socket and shared Alpha app UID | The compositor built with [#49](https://github.com/crowbaros/crowbaros/issues/49) has no public socket. The Alpha desktop keeps the spike's until it runs that compositor |

The Alpha services do not use those legacy adapters. Apps launch through the
root stand-in without inherited trusted descriptors or seat membership. Settings
Save works without Hyprland, and the shell follows saved preferences. The
compositor has no animations; the motion preference is stored for compatible
sessions without claiming an effect here. Apps still share one UID and can reach
one another, so this is not the production isolation boundary. Archived Fedora
service files are not a supported alternative session.
