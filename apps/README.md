# Apps: the GPUI interfaces

Native Rust and GPUI interfaces, designed and built by Jason Alexander against
the [design system](../DESIGN.md) and the [product brief](../PRODUCT.md).

| App | What it is | Role now |
| --- | --- | --- |
| [`shell/`](shell/README.md) | Workspace bar, dock, application launcher, and wallpaper, on Wayland layer surfaces | The starting point for the Milestone 4 shell: system chrome, the powerbox, and trusted asks |
| [`studio-settings/`](studio-settings/README.md) | Appearance and reduced-motion settings with preview, explicit save, and visible errors | The reference surface for the design system's components and themes |
| [`gpui-probe/`](gpui-probe/README.md) | A small app that proves GPUI rendering and input | A toolkit probe |

## What carries over and what does not

These apps were built for the earlier Fedora plan. The [vision](../docs/VISION.md)
keeps their interface work and replaces their system wiring.

| Carries over | Does not carry over |
| --- | --- |
| Themes, tokens, fonts, and component styling | RPM specs and Mock builds (`packaging/*.spec.in`) |
| The GPUI layer-shell backport (`shell/patches/`) | Hyprland IPC (`session.rs` in both apps) and the Hyprland hooks |
| The launcher, dock, and text-input code | Launching through `systemd-run`; Void Linux uses runit |
| The settings patterns: preview, explicit save, undo, inline errors | The `.desktop` catalog as the software model; CrowbarOS runs Tools through the broker |

CrowbarOS will have its own compositor, built on Smithay
([ADR 0015](../docs/adr/0015-our-own-compositor-on-smithay.md)). Keep compositor
calls behind the existing `session.rs` adapters. The shell has two: the control
channel the compositor spike defines, and the Hyprland socket, which it uses
when no compositor hands it a control channel. The Void Alpha session uses
independent runit services and UID-checked channels; Settings has an adapter
for the compositor's lack of animations. The older Hyprland adapter remains
for the legacy workflow. The
[spike record](../docs/research/compositor-spike-2026-11-15.md) has the details.

## Build and run

Each app is its own Cargo project with a pinned `Cargo.lock`. They are not
members of the root workspace.

Use the [Void desktop smoke command](../lab/void/README.md#full-desktop-smoke-check)
to build and exercise the interfaces in a fresh guest without overwriting a
development VM. Its Alpha process model follows ADR 0016 and explicitly stops
short of production compartment isolation. Fedora tooling retirement is #65.
[`docs/development/`](../docs/development/README.md) keeps dated validation records.

## Interfaces that are not designed yet

The vision needs these, and none of them exists: trusted asks and the confirm
gesture, preview and promote, the outbox, the powerbox, point and say with its
listening indicator, the task view and stop switch, and the recovery screen. See
the [product brief](../PRODUCT.md).
