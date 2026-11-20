# Labs

| Lab | Purpose | State |
| --- | --- | --- |
| [void/](void/README.md) | The compositor check and the desktop check, both booted from a copy of Base's image under UEFI | Active |

The [Fedora workflow](../docs/archive/fedora-workflow/README.md) is archived.
The substrate vision's Base generations lab and its
[adversarial lab](https://github.com/crowbaros/crowbaros/tree/substrate-final/lab/adversarial)
are at the `substrate-final` tag.
Current build and run instructions are in the
[Void development guide](../docs/development/local-vms.md). Both checks build
Base's image with `base/` and boot a fresh copy per run.

State lives outside Git in `.lab/void-compositor-checks/` and
`.lab/void-desktop-checks/`. Older runs may have left `.lab/void-graphical/`,
`.lab/void/`, and `.lab/adversarial/`. Retirement of source tooling does not
delete those disks, other local VM state, or checkpoints.
