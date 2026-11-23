# Apps: the GPUI interfaces

Native Rust/GPUI interfaces built against the [design system](../DESIGN.md) and
[product brief](../PRODUCT.md). Jason Alexander designed and built the shell and
Studio Settings. Their portable source, tests, fonts, licenses,
assets, and pinned dependencies carry forward to Void.

| App | Current role |
| --- | --- |
| [shell/](shell/README.md) | Workspace bar, dock, launcher, wallpaper, and the Agent's surface; starting point for the trusted shell |
| [files/](files/README.md) | Default file manager: Columns, List, and Grid views, inspector preview, quick preview, Trash, and safe file operations |
| [studio-settings/](studio-settings/README.md) | Appearance preview, Save/Undo, and restored preferences on CrowbarOS's desktop |
| [gpui-probe/](gpui-probe/README.md) | Optional GPUI rendering/input reference |

Use the [Void build/run guide](../docs/development/local-vms.md). Each app is a
separate Cargo project. The shell's [GPUI patches](shell/patches/README.md),
which `shell/packaging/prepare-gpui.py` applies, serve the shell, Files, and
Studio Settings; the RPM recipes and systemd/Hyprland startup hooks are in the
[workflow archive](../docs/archive/fedora-workflow/README.md).
Portable desktop entries and icons remain beside the source.
The shell launches desktop entries, and the launcher service starts each as
the person's account, as on any desktop
([ADR 0032](../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)).

The [Void desktop smoke command](../lab/void/README.md#the-desktop-check)
builds Base's image with the compositor, shell, Files, and Studio Settings, and
exercises them on a copy of it, preserving development VMs. The compositor and
the shell run as service accounts of their own, and every app as the person. The [acceptance record](../docs/development/void-desktop-smoke-2026-11-15.md)
covers launch, focus, workspaces, saved appearance, restart, and reboot.
The apps have also rendered on [Void hardware](../docs/research/compositor-nvidia-2026-11-15.md).

Base's desktop does not call the shell's and Settings' retained Hyprland or
systemd adapters, which are from before Void. The old hooks are archived and
are not installed.
