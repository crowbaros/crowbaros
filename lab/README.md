# Labs

| Lab | Purpose | State |
| --- | --- | --- |
| [void/](void/README.md) | Compositor and integrated Alpha desktop checks on Void/runit/seatd | Active |
| [compositor-spike/](compositor-spike/README.md) | The spike that tested ADR 0015. The Alpha desktop check runs it until the [compositor](../compositor/README.md) passes that check | Kept, not developed. Deleted then |

The [Fedora workflow](../docs/archive/fedora-workflow/README.md) is archived.
The substrate vision's Base generations lab and its
[adversarial lab](https://github.com/crowbaros/crowbaros/tree/substrate-final/lab/adversarial)
are at the `substrate-final` tag.
Current app build/run instructions are in the
[Void development guide](../docs/development/local-vms.md). The full desktop
smoke check creates a fresh guest per run in `.lab/void-desktop-checks/`.

State lives outside Git in `.lab/void-graphical/` and
`.lab/void-desktop-checks/`. Older runs may have left `.lab/void/` and
`.lab/adversarial/`. Retirement of source tooling does not delete those disks,
other local VM state, or checkpoints.
