# Void Linux lab

The two graphical checks for CrowbarOS: the compositor check and the desktop
check. Both build [Base](../../base/README.md)'s image with `base/void_base.py
build --lab-account` and boot a fresh copy of it under UEFI in an isolated run
directory. Base builds the only Void root and the only disk image; this lab
keeps the checks and the code that drives a guest.

Through the substrate vision this lab also ran
[Milestone 1](../../docs/archive/substrate-vision/milestones/milestone-1.md)
and the Warden's tests; those are at the
[`substrate-final`](https://github.com/crowbaros/crowbaros/tree/substrate-final/lab/void)
tag.

| Path | Contents |
| --- | --- |
| `compositor_check.py` | The check of the [compositor](../../compositor/README.md) on a TTY in Base's image |
| `desktop_check.py` | The desktop check and Base boot checks, on a fresh copy of Base's image, with retained diagnostics |
| `guest.py` | What both checks share: isolated UEFI VM runs on a copy of Base's disk, QMP input/screendumps, VNC monitor hotplug, and SSH |
| `test_*.py` | Unit tests for the guest driver and the checks |

```sh
lab/void/compositor_check.py   # build Base, boot a copy, run every compositor check, record evidence
lab/void/desktop_check.py      # build Base, boot a copy, run Base's boot checks and the desktop checks
```

The scripts use only the Python standard library and need no sudo. Base keeps
downloads in `~/.cache/crowbaros/void` and its roots, Cargo target, and image
in `.lab/base/`. Each check run gets its own directory under
`.lab/void-compositor-checks/` or `.lab/void-desktop-checks/`, outside git,
with its own grown copy of Base's disk, firmware variables, SSH port, and Unix
VNC and QMP sockets. Neither check touches Base's own `.lab/base/disk.raw`.
Both reach the guest with Base's SSH key, `.lab/base/id_ed25519`.

The host needs what [Base's README](../../base/README.md#what-the-host-needs)
lists, QEMU's virtio-gpu device included. Avoid running two checks or a Base
build at once: they share Base's build directory, `.lab/base/`, and both
checks overwrite `.lab/base/base.raw` with the lab account. Build Base again
before writing `.lab/base/base.raw` to a real disk.

## The compositor check

From the repository root:

```sh
lab/void/compositor_check.py
```

`compositor_check.py` builds Base's image with `base/void_base.py build
--lab-account` (which builds and tests Fulcrum from `compositor/`,
[ADR 0031](../../docs/adr/0031-the-compositor-is-built-in-this-repository-again.md),
in Base's Void build root), boots a copy under UEFI, stops Base's boot-time
desktop services, and starts Fulcrum directly on virtual terminal 2 with
seatd, debug logging, and no systemd. The evidence log names the commit, and
says when the build had changes that were not committed. It drives the guest
from outside: keys and the pointer through QEMU, and a second monitor plugged
into QEMU's second virtio-gpu connector. Every pull request that changes how
the compositor serves clients runs it.

The compositor has two kinds of client, and the check connects each the way
it allows ([ADR 0032](../../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)):

- The compositor runs as `crowbar-compositor`, the only member of `_seatd`.
- `crowbar-shell` connects to the shell's socket and its control channel.
- The lab account (`crowbar`, UID 1003) runs every app at the apps' socket.

It checks what the shell and an app are offered; that an app is refused
layer shell, session lock, security contexts, the shell's socket, and the
control channel; that only the person's user ID is served at the apps'
socket, `/run/crowbar-apps/wayland-0`; that Void's own `foot` runs as an
app; that a new window takes the keyboard; that switching to the console and
back while a second keyboard is unplugged keeps typing working; that a still
desktop draws no frames; and a monitor plugged in while it runs. The
[slice 1 record](../../docs/research/compositor-slice-1-2026-11-15.md) holds an
earlier run. The run of the
[spike](https://github.com/crowbaros/crowbaros/tree/substrate-final/lab/compositor-spike)
is in the [spike record](../../docs/research/compositor-spike-2026-11-15.md).

The guest uses software rendering. Hardware GPU qualification is separate.

## The desktop check

From the repository root:

```sh
lab/void/desktop_check.py
```

`desktop_check.py` builds Base's image with `base/void_base.py build
--lab-account`, boots a grown copy under UEFI through the firmware and GRUB as
a person's machine would, runs every Base boot check from `base/void_base.py`
(Void, the kernel, the Btrfs root growing to fill the disk, UEFI, runit,
services, microcode, Wi-Fi, SSH, the person's account, the system's log,
reboot/poweroff, the network, package groups, NVIDIA's module and command
line, and kernel messages) and reports each in `desktop.log` and
`results.json`, exercises the desktop, and powers the guest off.

The check drives real QEMU keyboard/pointer events for unlock at boot (a
wrong password, then the right one, with nothing typed first), launch, focus,
workspaces, lock by Super+L and by the bar's button, Ctrl+Alt+Enter neither
locking nor unlocking, the suspend hook's lock, Settings preview/Save/Undo, and
relaunch. It checks saved appearance, and a file the person wrote in their
home, after a clean reboot, service identities, linked libraries, binary
hashes, render activity, and shell restart. It checks that apps run as the
person at the ordinary socket for apps, that copy and paste, the primary
selection, and drag and drop work between two apps, and that the shell's
sockets, its control channel, and the unlock helper refuse an app. An error
returns nonzero and retains `desktop.log`, `results.json`, the build log,
service/kernel diagnostics, screenshots, and the failed disk in that run's
directory. The desktop's accounts and services are described in
[Base's README](../../base/README.md#the-desktops-accounts).
