# CrowbarOS desktop shell

The Rust/GPUI desktop slice: workspace bar, dock, installed-application search,
and Signal Feather wallpaper. The interface and its assets carry forward to
Void; Fedora packaging and session hooks are
[archived](../../docs/archive/fedora-workflow/README.md).

## Build and run

Use a Void development environment with the dependencies in the
[build/run guide](../../docs/development/local-vms.md). From this directory:

```sh
python3 packaging/prepare-gpui.py
cargo build --release --locked
cargo test --release --locked
```

The sources are not rustfmt-clean, and CI runs `cargo test` only. Do not run
`cargo fmt` here: it reformats files a change does not touch.

GPUI 0.2.2 uses the existing verified layer-shell backport, a patch with
which a still window commits nothing, and one with which a destroyed window
draws nothing ([patches](patches/README.md)).

With no display, the shell waits for one before it makes its layers. When the
last display goes away, the compositor closes the shell's layers and GPUI
stops, so the shell exits cleanly; its service starts it again, and it waits.
`compositor/tests/shell_outputs.rs` runs the real shell through that on a GPU
(#271).
The [desktop check](../../lab/void/README.md#the-desktop-check) builds Base's
image and runs the shell as Base runs it, on a copy of that image. On Base the
shell runs as its own service account, `crowbar-shell`, and connects to the
compositor's shell socket and control channel, which take the shell's user ID
alone ([ADR 0032](../../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)).
The [development guide](../../docs/development/local-vms.md) also describes a
nested, same-user rendering experiment, which runs no launcher service and has
no separate accounts.

The [Void NVIDIA record](../../docs/research/compositor-nvidia-2026-11-15.md)
shows rendering, resize, and suspend/resume for the tested setup. The earlier
[shell validation](../../docs/development/shell-validation-2026-11-14.md) remains
historical behavior evidence, not a current Fedora workflow.

## How it behaves on the desktop

- The bar and dock reserve 24 and 48 pixels through Wayland layer surfaces.
  Only the shell is offered layer shell, so no app can draw the bar, the dock,
  the launcher, the wallpaper, or the lock screen.
- Applications search supports keyboard navigation; closing the launcher returns focus.
- Workspace and window focus requests go over the compositor's control
  channel, and the compositor tells the shell of each change to the windows
  and workspaces as it happens, so the bar shows it at once.
- A still desktop draws nothing: the bar, dock, lock screen, and wallpaper
  commit only when what they show changes.
- Super tapped alone toggles the launcher, an app the shell launches takes the
  keyboard with its first window, and the bar and dock mark a window that
  wants attention.
- The desktop starts locked. While the compositor has it locked, the shell
  shows its lock screen, and the bar's lock button and Super+L lock it. The
  lock screen takes the person's password at once and sends it to Base's
  unlock helper (`CROWBAR_UNLOCK_SOCKET`), which checks it and tells the
  compositor to unlock; a wrong one shows "Wrong password" after a pause.
  Without a helper, as in the nested experiment, its Unlock button or Enter
  asks the compositor to unlock.
- The shell asks Base's launcher service to start an app, and the launcher
  starts it as the person's account. The shell reads the person's saved
  appearance through a read-only request to the launcher, and follows the
  theme they saved; it does not theme other apps.
- The shell lends Studio Settings the display layout through a bounded socket
  of its own (`CROWBAR_DISPLAY_SOCKET`), which takes only the person's and
  the shell's user IDs and passes on only `outputs` and `configure_output`.
- Desktop-entry discovery honors XDG precedence, visibility, locale, and executable availability. Search text is never executed as a command.
- The compositor and the shell restart independently under runit. A shell
  restart keeps the person's windows open; a compositor restart comes back
  locked.

These messages, the lock screen's namespace, and the unlock helper's socket
are accepted in ADR 0032. Desktop shortcuts and window-layout behavior come
from the compositor.

`src/session.rs` retains Hyprland, `systemd-run`, and `systemctl` fallback
paths from before Void. Base's services do not use them; do not use
`--start-session` as a Void startup command. Their old service and Lua hook
are archived and are not installed.

The [desktop acceptance record](../../docs/development/void-desktop-smoke-2026-11-15.md)
covers the desktop as it was on that date. Accessibility and input methods are
not established yet.

## Licensing

Native source is MIT. Manrope and IBM Plex Mono retain their OFL notices.
The adapted GPUI input example and layer-shell backport retain Apache-2.0 terms.
Brand origins remain in [assets/README.md](assets/README.md). No source, font,
license, asset, or Cargo lockfile was removed with the RPM recipe.
