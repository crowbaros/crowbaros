# Apps: the GPUI interfaces

Native Rust/GPUI interfaces designed and built by Jason Alexander against the
[design system](../DESIGN.md) and [product brief](../PRODUCT.md). Their portable
source, tests, fonts, licenses, assets, and pinned dependencies carry forward
to Void.

| App | Current role |
| --- | --- |
| [shell/](shell/README.md) | Workspace bar, dock, launcher, and wallpaper; starting point for the trusted shell |
| [studio-settings/](studio-settings/README.md) | Appearance preview, Save/Undo, and restored preferences in the Void Alpha session |
| [gpui-probe/](gpui-probe/README.md) | Optional GPUI rendering/input reference |

Use the [Void build/run guide](../docs/development/local-vms.md). Each app is a
separate Cargo project. The shell's verified GPUI layer-shell backport and
`packaging/prepare-gpui.py` remain active; the RPM recipes and systemd/Hyprland
startup hooks are in the [workflow archive](../docs/archive/fedora-workflow/README.md).
Portable desktop entries and icons remain beside the source.
The `.desktop` launcher is Alpha scaffolding. Production installed software
comes from the pinned catalog in [ADR 0017](../docs/archive/substrate-vision/adr/0017-a-pinned-catalog-and-workbenches.md)
and starts through the Warden; Tools run through the broker.

The [Void desktop smoke command](../lab/void/README.md#the-desktop-check)
builds and exercises the compositor, shell, and Settings in a fresh guest,
preserving development VMs. Separate runit services and UIDs follow ADR 0016's
temporary Alpha rules. The [acceptance record](../docs/development/void-desktop-smoke-2026-11-15.md)
covers launch, focus, workspaces, saved appearance, restart, and reboot.
The apps have also rendered on [Void hardware](../docs/research/compositor-nvidia-2026-11-15.md).

The Alpha path does not call the retained Hyprland or systemd adapters. Their
removal and replacement with production shell/Settings IPC are bounded by
[#71](https://github.com/crowbaros/crowbaros/issues/71). The old hooks are archived
and are not installed. Production compartment isolation remains separate work.

Trusted asks, declarative Tool UI, powerbox, preview/promote, point-and-say,
task visibility, emergency stop, and recovery are still roadmap work. See the
[vision](../docs/archive/substrate-vision/VISION.md). Retiring packaging does not implement those features.
