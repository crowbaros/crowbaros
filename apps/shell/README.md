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

GPUI 0.2.2 uses the existing verified [layer-shell backport](patches/README.md).
Use the [full desktop smoke command](../../lab/void/README.md#the-desktop-check)
to build Base's image and run the separate runit services on a copy of it. The Alpha shell
service connects to the compositor's UID-checked private Wayland and control
sockets. The guide also describes a nested, same-user rendering experiment;
that experiment does not exercise the Alpha launch or identity rules.

The [Void NVIDIA record](../../docs/research/compositor-nvidia-2026-11-15.md)
shows rendering, resize, and suspend/resume for the tested setup. The earlier
[shell validation](../../docs/development/shell-validation-2026-11-14.md) remains
historical behavior evidence, not a current Fedora workflow.

## Current behavior and limits

- The bar and dock reserve 24 and 48 pixels through Wayland layer surfaces.
- Applications search supports keyboard navigation; closing the launcher returns focus.
- Workspace and window focus requests use the CrowbarOS control channel when supplied.
- On that channel, Super tapped alone toggles the launcher, an app the shell launches takes the keyboard with its first window, and the bar and dock mark a window that wants attention. While the compositor has the session locked, the shell shows its lock screen; the bar's lock button locks it. Where `CROWBAR_UNLOCK_SOCKET` names an unlock helper, as in the Alpha session, the lock screen takes the person's password at once and sends it to the helper, which checks it and tells the compositor to unlock; a wrong one shows "Wrong password". Without one, its Unlock button or Enter asks the compositor to unlock. These messages, and the helper, are Fulcrum's proposal until a decision record settles them.
- Saved Settings themes are read by the shell; it does not theme third-party apps.
- Desktop-entry discovery honors XDG precedence, visibility, locale, and executable availability. Search text is never executed as a command.

On Base, the shell asks the launcher service to start an app, and the
launcher starts it as the person's account. The shell reads the person's saved
appearance through a read-only request to the launcher. The compositor and
shell restart independently under runit.

`src/session.rs` retains Hyprland, `systemd-run`, and `systemctl` fallback paths.
They are not used by the Alpha services; do not use `--start-session` as a Void
startup command. Replacing these adapters and the spike IPC is
[#71](https://github.com/crowbaros/crowbaros/issues/71). Their old service and Lua
hook are archived and are not installed.

The [full Void acceptance record](../../docs/development/void-desktop-smoke-2026-11-15.md)
covers the desktop as it was then. Display policy, accessibility and input
methods, and crash recovery are not established yet. The lock's root helper
checks the person's password and tells the compositor to unlock. Desktop
shortcuts and window-layout
behavior come from the compositor, not the archived Hyprland configuration.

## Licensing

Native source is MIT. Manrope and IBM Plex Mono retain their OFL notices.
The adapted GPUI input example and layer-shell backport retain Apache-2.0 terms.
Brand origins remain in [assets/README.md](assets/README.md). No source, font,
license, asset, or Cargo lockfile was removed with the RPM recipe.
