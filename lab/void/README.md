# Void Linux lab

A Void Linux VM on Btrfs that the apply engine drives. It ran
[Milestone 1](../../docs/milestones/milestone-1.md), and it is where Base work
continues (Milestone 1b in the [roadmap](../../docs/VISION.md#roadmap)).

| Path | Contents |
| --- | --- |
| `void_lab.py` | The lab command: fetch, rootfs, image, start, ssh, stop, milestone |
| `compositor_guest.py`, `compositor_check.py` | The graphical guest: build the compositor for Void, boot a guest, and check the compositor on a TTY |
| `config.json` | The pinned xbps build, the package lists, and the VM sizes |
| `host/` | Rootless rootfs, build, and disk-image scripts (run inside `unshare -r`) |
| `states/` | The three Milestone 1 Crowbar States |

```sh
lab/void/void_lab.py milestone   # build everything, run the loop, record evidence
lab/void/void_lab.py --help
```

The scripts use only the Python standard library and need no sudo. Downloads go
to `~/.cache/crowbaros/void`. Images, VM runtime files, and evidence logs go to
`.lab/void/` and `.lab/void-graphical/` in the repository root. All are outside
git.

## The graphical guest

The graphical guest runs the CrowbarOS compositor
([ADR 0015](../../docs/adr/0015-our-own-compositor-on-smithay.md)) on a virtual
terminal, with seatd as the seat manager and no systemd.

```sh
lab/void/compositor_guest.py check   # build, boot, run every check, record evidence
lab/void/compositor_guest.py --help
```

`check` builds the compositor inside a Void build root, so the binary links
Void's own libraries. It then boots the guest and drives it from outside: keys
and the pointer through QEMU, and a second monitor plugged into QEMU's second
virtio-gpu connector. The first run of the [spike](https://github.com/crowbaros/crowbaros/issues/3)
is in the [spike record](../../docs/research/compositor-spike-2026-11-15.md).

Three things to know:

- The guest is a plain Void root booted straight from its kernel. It is not
  built by the apply engine and has no generations.
- QEMU needs its virtio-gpu device. Where the distribution packages it apart
  (Arch: `qemu-hw-display-virtio-gpu-pci`), install it, or set
  `QEMU_MODULE_DIR` to a directory that holds the modules.
- The guest has no GPU acceleration. The compositor renders with Mesa's
  software renderer. The GPUI apps do not run in this guest yet, so the
  [Fedora lab](../fedora/README.md) stays for interface work.
