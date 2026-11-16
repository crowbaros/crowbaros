# Void Linux lab

A Void Linux VM on Btrfs that the apply engine drives. It ran
[Milestone 1](../../docs/milestones/milestone-1.md), and it is where Base work
continues (Milestone 1b in the [roadmap](../../docs/VISION.md#roadmap)).

| Path | Contents |
| --- | --- |
| `void_lab.py` | The lab command: fetch, rootfs, image, start, ssh, stop, milestone |
| `compositor_guest.py`, `compositor_check.py` | Void builds and the earlier compositor-only TTY check |
| `desktop_check.py` | Fresh, isolated full-desktop smoke checks, with retained diagnostics |
| `warden_check.py` | Runs the Warden's root-only tests as root in the guest |
| `guest/` | Alpha runit services and the temporary app-launch stand-in |
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

## The Warden's tests

The Warden runs as root, and nobody needs root on their own desktop
([ADR 0016](../../docs/adr/0016-the-warden-starts-every-compartment.md)). So
the tests of [`crowbar-sandbox`](../../crates/README.md) that start real
compartments run here:

```sh
lab/void/warden_check.py           # fresh guest, every test, evidence log
lab/void/warden_check.py --keep    # reuse the running guest, for a quick loop
lab/void/warden_check.py --keep trusted   # only the tests whose name holds "trusted"
```

It builds the tests and their two programs as static files, copies them into
the guest, runs them as root, and writes what they print to
`.lab/void/warden-<time>.log`. This is stock Void: Milestone 2 needs nothing
from Base.

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
- The guest uses software rendering. Hardware GPU qualification is separate.

## Full desktop smoke check

From the repository root:

```sh
python3 lab/void/compositor_guest.py desktop-check
```

This builds and tests the compositor, shell, and Settings in Void, creates a
fresh guest image, exercises the desktop, and powers off the test guest. Every
run gets a new directory in `.lab/void-desktop-checks/`, its own SSH port, key,
and Unix VNC sockets. It never stops or replaces `.lab/void-graphical/disk.raw`.
Do not use the older `check` or `desktop` command against a preserved preview.

To reuse an already provisioned Void build root and Cargo target directory:

```sh
python3 lab/void/compositor_guest.py desktop-check --build-cache .lab/void-graphical
```

Only `buildroot/` and `target/` are reused. The guest root, disk, credentials,
and sockets are fresh. Avoid concurrent builds using the same build cache.
The host needs Python, Cargo and its pinned dependency cache, QEMU/KVM, Btrfs
image tools, and working user/mount namespaces; downloads use the configured
Void mirror. No host desktop configuration or installed packages are changed.

The check drives real QEMU keyboard/pointer events for launch, focus, workspaces,
Settings preview/Save/Undo, and relaunch. It checks saved appearance after a
clean reboot, service identities, linked libraries, binary hashes, render
activity, shell restart, and refusal of app access to trusted sockets. An error
returns nonzero and retains `desktop.log`, `results.json`, build/package logs,
service/kernel diagnostics, screenshots, and the failed disk in that run's
directory. SSH keys stay there and must not be published with evidence.

## Alpha process model

This applies the temporary Alpha rules in
[ADR 0016](../../docs/adr/0016-the-warden-starts-every-compartment.md):

- `crowbar-compositor` (UID 1000) owns the seat and is the only `_seatd` member.
- `crowbar-shell` (UID 1001) is a separate runit service. Its two compositor
  sockets check its kernel UID; neither service launches the other.
- `crowbar-app` (UID 1002) runs installed desktop entries through a small root
  stand-in, with no inherited trusted descriptors or supplementary groups,
  a fixed environment, and `no_new_privs`. Only the shell UID may request a
  root-owned entry in `/usr/share/applications`.

The shell reads saved appearance through a bounded, read-only request to that
stand-in. Settings retains private mode-0600 preferences in the app user's home.
The compositor currently has no animations; Settings saves the motion preference
for compatible sessions and says so without claiming an effect here.

This is not the production Warden or principal isolation. Apps still share one
user and can reach one another. The spike retains its public ordinary Wayland
socket and security-context behavior until #49; it has no per-compartment
namespaces, cgroups, broker authorization, or production resource enforcement.
The fixed lab UIDs are not production compartment IDs. The separate Base
milestone still verifies apply/generation/rollback behavior.
