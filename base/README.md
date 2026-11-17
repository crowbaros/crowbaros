# The CrowbarOS base

Plain Void Linux, built without root and booted under UEFI. This is the first
step of the rebuild on the `new-vision` branch: a base that boots, before
anything is built on it. It has no apply engine, no generations, and no
desktop.

```sh
base/void_base.py check          # build, boot, check, record a log, power off
base/void_base.py build          # install the Void root and write the boot disk
base/void_base.py boot           # boot a fresh copy of the disk (--window to watch it)
base/void_base.py ssh [CMD]      # a root shell, or one command
base/void_base.py console        # the serial console (Ctrl-] to leave)
base/void_base.py stop           # power off
```

## What it builds

| Part | What it is |
| --- | --- |
| Packages | `base-minimal`, `linux`, `btrfs-progs`, `grub-x86_64-efi`, `mtools`, `openssh`, `dhcpcd`, `iproute2`, from Void's `current` repository ([`config.json`](config.json)) |
| Disk | GPT. An ESP holds a standalone GRUB at `\EFI\BOOT\BOOTX64.EFI`, the path any UEFI firmware boots without a boot entry. The root is Btrfs, labelled `crowbaros-root` |
| Boot | GRUB finds the root by label and reads `/boot/grub/grub.cfg` there. The kernel and initramfs are the ones xbps built |
| Services | runit, with gettys on the consoles and the serial port, `dhcpcd`, and `sshd`. Root logs in over SSH with a key made for the lab, and has no password |

The VM is QEMU with KVM, OVMF firmware, virtio disk and network, and SSH
forwarded to `127.0.0.1:22240`.

## Where things go

- Downloads: `~/.cache/crowbaros/void`, shared with `lab/void/`. The static xbps
  is pinned by SHA-256 and refused if it does not match.
- The root, the disks, the SSH key, and boot logs: `.lab/base/`, outside git.
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

The host needs Python 3.11 or later, QEMU with KVM, OVMF at the paths in
`config.json`, `unshare` from util-linux with `newuidmap` and `newgidmap`,
`mkfs.btrfs`, `mkfs.vfat`, `sfdisk`, `curl`, `ssh`, and `socat` for the console.

## What it checks

`check` boots a fresh copy of the disk, waits for the login prompt on the
serial console and for SSH, and then checks in the guest: that it is Void, that
the running kernel is the one in the boot menu, that the root is the Btrfs
volume and writable, that it booted under UEFI, the hostname, that runit is
PID 1, that `sshd`, `dhcpcd`, and the serial getty run, that the network has an
address, that packages kept their groups, and that the kernel logged nothing
critical.

## Not yet

- Real hardware. The disk has only what a VM needs: no firmware packages, no
  Wi-Fi, no microcode.
- Kernel updates in the guest. Void's kernel hook would rewrite
  `/boot/grub/grub.cfg` with `update-grub`, and that path is untested.
- The initramfs is dracut's generic one, about 200 MB.
- No users besides root, no locale or time zone, and no installer.
