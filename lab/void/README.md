# Void Linux lab

A Void Linux VM that runs the CrowbarOS compositor, and the Alpha desktop of
the shell and Studio Settings. Through the substrate vision this lab also ran
[Milestone 1](../../docs/archive/substrate-vision/milestones/milestone-1.md)
and the Warden's tests; those are at the
[`substrate-final`](https://github.com/crowbaros/crowbaros/tree/substrate-final/lab/void)
tag.

| Path | Contents |
| --- | --- |
| `compositor_guest.py`, `compositor_check.py` | Void builds, and the check of the [compositor](../../compositor/README.md) on a TTY |
| `desktop_check.py` | Fresh, isolated full-desktop smoke checks, with retained diagnostics |
| `void_lab.py` | What the checks share: the pinned static xbps and the package cache |
| `guest/` | Alpha runit services and the temporary app-launch stand-in |
| `config.json` | The pinned xbps build, the package lists, and the VM sizes |
| `host/` | Rootless rootfs, build, and disk-image scripts (run inside `unshare -r`) |

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

# A Fulcrum commit that is not pinned yet, from a local clone
FULCRUM_REPOSITORY=../fulcrum FULCRUM_REVISION=<full commit ID> lab/void/compositor_guest.py check
```

The compositor is built in Fulcrum, its own repository
([ADR 0027](../../docs/adr/0027-the-compositor-is-built-in-its-own-repository.md)).
`check` fetches the commit that `compositor/fulcrum.json` pins into
`.lab/fulcrum.git`, and writes the commit's tree, not a working copy, to
`.lab/fulcrum/<commit>/`. It builds and tests that tree inside a Void build
root, so the binary links Void's own libraries. The evidence log names the
CrowbarOS and Fulcrum commits, and says when the Fulcrum one is not pinned. It then boots the guest and drives it from outside:
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
run. The run of the [spike](../compositor-spike/README.md) is in the
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

## Full desktop smoke check

From the repository root:

```sh
python3 lab/void/compositor_guest.py desktop-check
```

This builds and tests the compositor at the Fulcrum commit
[`compositor/`](../../compositor/README.md) pins, the shell, and Settings in
Void, creates a fresh guest image, exercises the desktop, and powers off the
test guest. To try a Fulcrum commit that is not pinned yet, set
`FULCRUM_REPOSITORY` and `FULCRUM_REVISION` as for `check`. Every
run gets a new directory in `.lab/void-desktop-checks/`, its own SSH port, key,
and Unix VNC sockets. It never stops or replaces `.lab/void-graphical/disk.raw`.
Do not use the `check` or `desktop` command against a preserved preview:
each replaces the disk at `.lab/void-graphical/`.

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
[ADR 0016](../../docs/archive/substrate-vision/adr/0016-the-warden-starts-every-compartment.md):

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
user and can reach one another. The compositor has no public socket: the
launcher registers one compartment for every app through the compositor's
Warden socket, with `crowbar-probe warden`, and starts each app on that
compartment's listener (ADR 0016). There are no per-compartment namespaces,
cgroups, broker authorization, or production resource enforcement.
The fixed lab UIDs are not production compartment IDs.
