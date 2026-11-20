# Void Linux lab

A Void Linux VM that runs the CrowbarOS compositor for its check, and the check
of the desktop, which runs on [Base](../../base/README.md). Through the substrate vision this lab also ran
[Milestone 1](../../docs/archive/substrate-vision/milestones/milestone-1.md)
and the Warden's tests; those are at the
[`substrate-final`](https://github.com/crowbaros/crowbaros/tree/substrate-final/lab/void)
tag.

| Path | Contents |
| --- | --- |
| `compositor_guest.py`, `compositor_check.py` | Void builds, and the check of the [compositor](../../compositor/README.md) on a TTY |
| `desktop_check.py` | The desktop check, on a fresh copy of Base's image, with retained diagnostics |
| `void_lab.py` | What the checks share: the pinned static xbps and the package cache |
| `config.json` | The pinned xbps build, the package lists, and the VM sizes |
| `host/` | Rootless rootfs and disk-image scripts for the graphical guest (run inside `unshare -r`) |

```sh
lab/void/compositor_guest.py check   # build, boot, run every check, record evidence
lab/void/compositor_guest.py --help
```

The scripts use only the Python standard library and need no sudo. Downloads go
to `~/.cache/crowbaros/void`. Images, VM runtime files, and evidence logs go to
`.lab/void-graphical/` in the repository root. All are outside git.

## The graphical guest

The graphical guest runs the CrowbarOS [compositor](../../compositor/README.md)
on a virtual terminal, with seatd as the seat manager and no systemd.

```sh
lab/void/compositor_guest.py check   # build, boot, run every check, record evidence
lab/void/compositor_guest.py --help
```

`check` builds and tests the compositor, Fulcrum, from `compositor/`
([ADR 0031](../../docs/adr/0031-the-compositor-is-built-in-this-repository-again.md)) inside a Void build
root, with Base's build script, so the binary links Void's own libraries. The evidence log names the
commit, and says when the build had changes that were not committed. It then boots the guest and drives it from outside:
keys and the pointer through QEMU, and a second monitor plugged into QEMU's
second virtio-gpu connector. Every pull request that touches the compositor
runs it ([ADR 0022](../../docs/archive/substrate-vision/adr/0022-the-compositor-is-built-beside-milestone-2.md)).

The compositor has no public socket, so the check connects the two ways it
allows ([ADR 0016](../../docs/archive/substrate-vision/adr/0016-the-warden-starts-every-compartment.md)):

- The compositor runs as `crowbar-compositor`, the only member of `_seatd`.
- `crowbar-shell` connects to the shell's socket.
- A stand-in for the Warden runs as root, and registers a listener for one
  compartment. `crowbar-app` is a client of that listener.

It checks what each of the three is offered, the attacks that the decision
records give slice 1, that a new window does not take the keyboard, that a
still desktop draws no frames, and a monitor plugged in while it runs. The
[slice 1 record](../../docs/research/compositor-slice-1-2026-11-15.md) holds a
run. The run of the [spike](https://github.com/crowbaros/crowbaros/tree/substrate-final/lab/compositor-spike) is in the
[spike record](../../docs/research/compositor-spike-2026-11-15.md).

Four things to know:

- `check` replaces the disk at `.lab/void-graphical/disk.raw`, and keeps the
  one it replaces beside it.

- The guest is a plain Void root booted straight from its kernel. It has no
  generations.
- QEMU needs its virtio-gpu device. Where the distribution packages it apart
  (Arch: `qemu-hw-display-virtio-gpu-pci`), install it, or set
  `QEMU_MODULE_DIR` to a directory that holds the modules.
- The guest uses software rendering. Hardware GPU qualification is separate.

## The desktop check

From the repository root:

```sh
lab/void/compositor_guest.py desktop-check
```

This builds Base's image with Base's own command, `base/void_base.py build
--lab-account`, which builds and tests the
[compositor](../../compositor/README.md), the shell, and Studio Settings in
Void, and gives the image the lab account even when you keep an account file.
It overwrites Base's image, so build again before you put it on a disk. Base
keeps its Void roots and Cargo target in `.lab/base/`, so a second run starts
from them. The check then boots a copy of
that image under UEFI, through the firmware and GRUB, as a person's machine
would, exercises the desktop, and powers the guest off. Every run gets a new
directory in `.lab/void-desktop-checks/`, with its own copy of the disk,
firmware variables, SSH port, and Unix VNC and QMP sockets. It never touches
Base's own disk, `.lab/base/disk.raw`, or the lab's,
`.lab/void-graphical/disk.raw`. It reaches the guest with the SSH key Base's
build made, `.lab/base/id_ed25519`.

The host needs what [Base's README](../../base/README.md#what-the-host-needs)
lists, QEMU's virtio-gpu device included. Avoid two builds at once: they share
Base's build directory.

The check drives real QEMU keyboard/pointer events for unlock at boot (the
confirm chord, a wrong password, then the right one), launch, focus, workspaces,
lock and unlock, the suspend hook's lock, Settings preview/Save/Undo, and
relaunch. It checks saved appearance, and a file the person wrote in their
home, after a clean reboot, service identities, linked libraries, binary hashes, render
activity, shell restart, and refusal of app access to trusted sockets. An error
returns nonzero and retains `desktop.log`, `results.json`, the build log,
service/kernel diagnostics, screenshots, and the failed disk in that run's
directory. The desktop's accounts and services are described in
[Base's README](../../base/README.md#the-desktops-accounts).
