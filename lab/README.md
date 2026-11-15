# Labs

| Lab | Purpose | State |
| --- | --- | --- |
| [void/](void/README.md) | Base generations and integrated Alpha desktop checks on Void/runit/seatd | Active |
| [adversarial/](adversarial/README.md) | M2 fixture services and security tests | Not started |

The [Fedora workflow](../docs/archive/fedora-workflow/README.md) is archived.
Current app build/run instructions are in the
[Void development guide](../docs/development/local-vms.md). The full desktop
smoke check creates a fresh guest per run in `.lab/void-desktop-checks/`.

State lives outside Git in `.lab/void/` and `.lab/void-graphical/`. Retirement of
source tooling does not delete those disks, other local VM state, or checkpoints.
