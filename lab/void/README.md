# Void Linux lab

A Void Linux VM on Btrfs that the apply engine drives. It ran
[Milestone 1](../../docs/milestones/milestone-1.md), and it is where Base work
continues (Milestone 1b in the [roadmap](../../docs/VISION.md#roadmap)).

| Path | Contents |
| --- | --- |
| `void_lab.py` | The lab command: fetch, rootfs, image, start, ssh, stop, milestone |
| `config.json` | The pinned xbps build, the base package list, and the VM size |
| `host/` | Rootless rootfs and disk-image build scripts (run inside `unshare -r`) |
| `states/` | The three Milestone 1 Crowbar States |

```sh
lab/void/void_lab.py milestone   # build everything, run the loop, record evidence
lab/void/void_lab.py --help
```

The script uses only the Python standard library and needs no sudo. Downloads go
to `~/.cache/crowbaros/void`. Images, VM runtime files, and evidence logs go to
`.lab/void/` in the repository root. Both are outside git.

The lab has no graphical guest yet. When it has one, the GPUI apps move here and
the [Fedora lab](../fedora/README.md) is removed.
