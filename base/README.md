# The CrowbarOS base

Plain Void Linux with the desktop on it, built without root and booted under
UEFI. The desktop is Fulcrum, the shell, and Studio Settings, as the lab ran
them. It has no apply engine and no generations.

```sh
base/void_base.py check          # build, boot, check, record a log, power off
base/void_base.py build          # install the Void roots, build the desktop, write the boot disk
base/void_base.py boot           # boot a fresh copy of the disk (--window to see the desktop)
base/void_base.py ssh [CMD]      # a root shell, or one command
base/void_base.py console        # the serial console (Ctrl-] to leave)
base/void_base.py stop           # power off
```

## What it builds

| Part | What it is |
| --- | --- |
| Packages | `base-minimal`, `linux`, `btrfs-progs`, `grub-x86_64-efi`, `mtools`, `openssh`, `dhcpcd`, `iproute2`, and what the desktop needs: `seatd`, Mesa with lavapipe, Wayland, `foot`, `python3`, and others, from Void's `current` repository ([`config.json`](config.json)) |
| Desktop | Fulcrum (`compositor/`), the shell, and Studio Settings, built and tested in a Void build root of their own so they link Void's libraries, in `/usr/local/bin`. Their runit services and helpers are in [`desktop/`](desktop/) |
| Disk | GPT. An ESP holds a standalone GRUB at `\EFI\BOOT\BOOTX64.EFI`, the path any UEFI firmware boots without a boot entry. The root is Btrfs, labelled `crowbaros-root` |
| Boot | GRUB finds the root by label and reads `/boot/grub/grub.cfg` there. The kernel and initramfs are the ones xbps built |
| Services | runit, with gettys on the consoles and the serial port, `dhcpcd`, `sshd`, `seatd`, and the desktop's four: the compositor on virtual terminal 2, the shell, the launcher, and the unlock helper. Root logs in over SSH with a key made for the lab, and has no password |

The VM is QEMU with KVM, OVMF firmware, virtio disk and network, SSH
forwarded to `127.0.0.1:22240`, and for the desktop a virtio GPU, keyboard, and
tablet, with no VGA card.

## Where things go

- Downloads: `~/.cache/crowbaros/void`, shared with `lab/void/`. The static xbps
  is pinned by SHA-256 and refused if it does not match.
- The root, the build root, the Cargo target, the disks, the SSH key, and boot
  logs: `.lab/base/`, outside git. The build root and the target are kept, so
  the next build starts from them.
- `check` writes `.lab/base/boot-<time>.log`, with every check, what it ran, and
  the serial console from GRUB to power-off.

## How it runs without root

Each step that has to own files as root runs inside `unshare` as fake root.
Your subordinate IDs from `/etc/subuid` and `/etc/subgid` are mapped from 1
up, so packages can give files to their own users and groups, such as `tty`
and `_dhcpcd`. Without such a range the build still works, and those files
belong to root.

Because of that mapping, some files under `.lab/base/rootfs` belong to IDs
other than yours. Remove it with `base/void_base.py build --force`, which
deletes it inside the namespace, or with
`unshare --map-root-user --map-users=1:<start>:<count> --map-groups=1:<start>:<count> rm -rf .lab/base/rootfs`.
The same goes for `.lab/base/buildroot`.

## What the host needs

Python 3.11 or later, and on Arch or Omarchy:

```sh
sudo pacman -S --needed qemu-desktop edk2-ovmf btrfs-progs dosfstools socat
```

`util-linux`, `shadow`, `curl`, and `openssh` are already there. `qemu-base`
also works with `qemu-hw-display-virtio-gpu-pci`, but `boot --window` needs the
GTK display that `qemu-desktop` has. Where QEMU's virtio-gpu device is not
installed, set `QEMU_MODULE_DIR` to a directory that holds it. The desktop is
built with Cargo, and its crates are fetched on the host: install Rust through
`rustup`.
You also need read and write access to `/dev/kvm`, and a range of subordinate
IDs in `/etc/subuid` and `/etc/subgid` (Arch gives every new user one). `build`
and `boot` check for all of this first, and name what is missing.

The first `build` on a machine downloads the packages for both roots, and
compiles the desktop. It shows each install as it goes, and keeps a copy in
`.lab/base/rootfs.log` and `.lab/base/buildroot.log`. If a build stops partway,
the next `build` installs that root again: it uses a root only after an install
of the configured packages finished.

## What it checks

`check` boots a fresh copy of the disk, waits for the login prompt on the
serial console and for SSH, and then checks in the guest: that it is Void, that
the running kernel is the one in the boot menu, that the root is the Btrfs
volume and writable, that it booted under UEFI, the hostname, that runit is
PID 1, that `sshd`, `dhcpcd`, and the serial getty run, that the network has an
address, that packages kept their groups, and that the kernel logged nothing
critical.

The desktop has its own check, which boots a copy of this image under UEFI and
drives the desktop as a person would:

```sh
lab/void/compositor_guest.py desktop-check
```

See the [lab's README](../lab/void/README.md#the-desktop-check).

## The desktop's accounts

The desktop runs as the lab ran it, under the temporary rules of
[ADR 0016](../docs/archive/substrate-vision/adr/0016-the-warden-starts-every-compartment.md).
Later tickets of Milestone 2 replace them.

- `crowbar-compositor` (UID 1000) owns the seat and is the only `_seatd` member.
- `crowbar-shell` (UID 1001) is a separate runit service. Its two compositor
  sockets check its kernel UID; neither service launches the other.
- `crowbar-app` (UID 1002) runs installed desktop entries through a root
  launcher, `desktop/launcher.py`, with no inherited trusted descriptors or
  supplementary groups, a fixed environment, and `no_new_privs`. Only the shell
  UID may ask it to launch.
- `crowbar-person` (UID 1003) is the person's account. It has no login shell
  and no home. Its password, `crowbar` in the lab, unlocks the desktop.
- `crowbar-unlock` is a root service, `desktop/unlock.py`. The compositor
  starts locked and listens for it at a socket only root may use. After the
  confirm chord, the lock screen sends the person's password to the helper,
  which checks it against `/etc/shadow` and only then tells the compositor to
  unlock. zzz's suspend hook locks the desktop through the same helper.

## Not yet

- Real hardware. The disk has only what a VM needs: no firmware packages, no
  Wi-Fi, no microcode.
- Kernel updates in the guest. Void's kernel hook would rewrite
  `/boot/grub/grub.cfg` with `update-grub`, and that path is untested.
- The initramfs is dracut's generic one, about 200 MB.
- No locale or time zone, and no installer. The person's account has no home.
