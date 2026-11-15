# CrowbarOS desktop shell

> **Status.** This app was built for the earlier Fedora plan. Its interface work
> carries over to the [vision](../../docs/VISION.md); its RPM packaging and Hyprland
> wiring do not. It is built and run only in the [Fedora lab](../../lab/fedora/README.md),
> which is kept for interface work. See [what carries over](../README.md).

The first Rust/GPUI desktop slice: a live workspace bar, a persistent dock,
installed-application search and the approved Signal Feather wallpaper.
It runs as an ordinary user on Hyprland. The shell uses real Wayland layer
surfaces: the bar reserves 44 px, the dock reserves 96 px, and the launcher
captures keyboard input only while open.

## Build and run in the local lab

```bash
rtk python3 lab/fedora/lab.py start build
rtk python3 lab/fedora/lab.py view build
rtk python3 lab/fedora/lab.py wait build
rtk python3 lab/fedora/lab.py shell-build build
rtk python3 lab/fedora/lab.py start test
rtk python3 lab/fedora/lab.py view test
rtk python3 lab/fedora/lab.py wait test
rtk python3 lab/fedora/lab.py shell-deploy test
rtk python3 lab/fedora/lab.py shell-run test intel-virtio
```

Keep the relevant VM viewer visible during work. The experimental accelerated
SPICE profile can stall Vulkan startup while its viewer is hidden on another
workspace; bringing the viewer forward resumes it. These commands build in Fedora,
produce a source RPM and binary RPM with clean Mock/offline compilation, sign
the binary and repository with the lab development key, and install through DNF.
The host remains Omarchy. The test guest needs no compiler. See
[the local VM workflow](../../docs/development/local-vms.md) for prerequisites.

The recipe pins GPUI 0.2.2 and applies a small upstream layer-shell backport.
[Patch provenance and adaptations](patches/README.md) explain the maintenance
boundary. Studio Settings continues using stock GPUI. `Cargo.lock`, the patch,
vendor tree and dependency licenses travel in the source RPM. Generated GPUI
sources are reproducible via `python3 packaging/prepare-gpui.py`.

## Using the desktop

- Click **Applications**, its dock icon, or press **Alt+Space** to search.
- Type a name; **Up/Down** or **Tab/Shift+Tab** chooses a result, **Enter** opens it.
- **Escape**, the close button, or clicking the backdrop dismisses the launcher.
- Dock buttons open or focus Terminal and Studio Settings when installed.
- Workspace numbers switch workspaces; the bar shows the active window and local time.
- **Alt+Return** opens a terminal, **Alt+Q** closes a window, **Alt+V** toggles floating.
- Hold **Alt** and drag with the left mouse button to move a window, or right to resize; window borders also resize.

Application discovery honors desktop-entry visibility, XDG precedence, locale,
and executable availability. GIO handles desktop entry execution inside a separate systemd user scope, so
restarting the shell does not close applications. Search text
is never executed as a command. Files are rediscovered each time the launcher opens.
The launcher focuses an existing matching window when possible.

The shell follows **saved** Studio Settings appearance preferences (Dark, Light,
Warm Paper and Studio neutral). It does not change those preferences or theme
third-party applications. The shell has no entrance or continuous animations.

## Session and recovery

The RPM installs a systemd user service and an optional Hyprland Lua hook.
`lab.py shell-deploy test` connects the hook to the lab-owned compositor config.
`--start-session` imports the current Wayland/Hyprland environment and starts the
service; `--launcher` and `--quit` send fixed commands over a private user socket.
The service restarts on failure, with a bounded restart limit.

`lab.py shell-run test` accepts `system`, `intel-virtio` and `software`, like the
GPUI probe. The selected ICD is scoped to the shell service and its child apps
in `/etc/crowbaros-lab/shell-vulkan.env`; it is not globally registered.

Stop the service from the administrator SSH session if needed:

```bash
rtk python3 lab/fedora/lab.py ssh test sudo -u desktop env XDG_RUNTIME_DIR=/run/user/1001 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1001/bus systemctl --user stop crowbaros-shell
```

The `before-shell` offline checkpoint preserves the previously working lab.
Restore with the existing `lab.py stop test` and `lab.py restore test before-shell`
workflow. The helper retains the outgoing disk before restoring.

## Repeat the integration checks

Save a checkpoint, close test Terminal/Settings windows, use the recorded Dark,
Studio-off, reduced-motion baseline, keep the viewer visible, and pause manual
interaction while running:

```bash
rtk python3 lab/fedora/validate_shell.py
```

The check drives native input, exercises application launch and focus, follows a
saved theme, checks long/Unicode input and a recoverable launch error, changes
output resolution live, and restarts the shell while an application stays open.
It temporarily creates a named desktop-entry fixture and restores saved appearance.
Evidence is written under `.local/shell/validation/`. `wl-copy` and `grim` are
validation utilities already available in this lab. Cold-boot verification is
recorded separately in the validation report.

## First-slice boundaries

One output is supported by this integration. There is no multi-monitor selection,
auto-hide dock, notification center, lock screen, power menu, network/audio controls,
or editable pins yet. The dock initially pins installed Terminal and Settings;
it is not a full running-application task switcher. Session state currently uses
bounded Hyprland IPC polling; event-driven updates and idle-power profiling remain
follow-up work. Screen reader/IME integration, fractional scaling, international
layout coverage and production session lifecycle still require qualification.

Brand assets are copied from the approved CrowbarOS kit, with origins recorded
in `assets/README.md`. Manrope is bundled under OFL. Native source is GPL-3.0-or-later;
the adapted GPUI text-input example and layer-shell backport retain Apache-2.0 terms.
