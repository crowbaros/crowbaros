# Base on a real machine

How to put Base's image on a disk, boot a real x86_64 machine from it, and join
a Wi-Fi network. The same image boots in the VM, on a machine with an NVIDIA
GPU, and on one that draws with Mesa.

## What the machine needs

- An x86_64 machine with UEFI firmware. Base has no BIOS boot.
- **Secure Boot off.** The image's GRUB is not signed, so firmware with Secure
  Boot on refuses it. Turn it off in the firmware's settings.
- A disk to give the image whole: a USB stick or SSD of 16 GB or more, as the
  image is 8.1 GiB. Writing the image erases everything on it.
- For NVIDIA's driver, a Turing (RTX 20) GPU or later. Older NVIDIA cards are
  not carried; on them, and on any other GPU, Mesa draws.

## Build the image

Set your own account first, in `base/account.json`; see Base's README,
["The person's account"](README.md#the-persons-account). Without it the image
has the lab account, whose password is published in this repository. Then:

```sh
base/void_base.py build
```

The build's last line names the image's account. A check run since your last
build leaves an image with the lab account, so build again after any check.
The image is `.lab/base/base.raw`.

## Write it to a disk

Find the disk. Plug it in and compare `lsblk` before and after; its size and
model tell you which it is. Make sure it is not a disk you need: everything on
it goes.

```sh
lsblk -o NAME,SIZE,MODEL,TRAN
```

Then write the image to the whole disk, not to a partition (`/dev/sdX`, not
`/dev/sdX1`), unmounting anything your desktop mounted from it first:

```sh
sudo dd if=.lab/base/base.raw of=/dev/sdX bs=4M conv=fsync status=progress
```

The image is 8.1 GiB, most of it empty.

## Boot it

1. Plug the disk into the machine and turn it on.
2. Open the firmware's boot menu. The key differs between makers: F12, F11,
   F8, F10, or Esc, pressed as the machine starts. The firmware's own splash
   screen usually names it.
3. Pick the disk's UEFI entry. The image boots from the path every UEFI
   firmware knows, `\EFI\BOOT\BOOTX64.EFI`, so it needs no boot entry of its
   own.
4. GRUB shows the CrowbarOS menu for two seconds and boots.

On the first boot, the root grows to fill the disk; the console says by how
much. Every boot checks again and finds nothing to do.

The machine comes up on the desktop's lock screen. Unlock it with your
account's password.

Wired Ethernet needs nothing: `dhcpcd` gets an address on every interface at
boot.

## Join Wi-Fi

Wi-Fi is `iwd`, used from a terminal with `iwctl`. Your account is in the
`netdev` group, which may use it. Apps do not run as your account yet, so use
the console:

1. Press Ctrl+Alt+F1 for the first console, and log in with your account's name
   and password.
2. Find the Wi-Fi device, usually `wlan0`:

   ```sh
   iwctl device list
   ```

3. Scan, list the networks, and join one by its name. `iwctl` asks for the
   passphrase:

   ```sh
   iwctl station wlan0 scan
   iwctl station wlan0 get-networks
   iwctl station wlan0 connect "Network name"
   ```

4. Check for an address. `dhcpcd` gets it once `iwd` has joined:

   ```sh
   ip -4 addr show wlan0
   ```

5. Press Ctrl+Alt+F2 to go back to the desktop.

`iwd` remembers the network and joins it again on every boot.

If `iwctl device list` shows no device, the machine's Wi-Fi may need firmware
the image does not carry. `dmesg | grep -i firmware` names what the driver
asked for.

## Reach it over SSH

SSH takes root's key alone, the one Base's build made: `.lab/base/id_ed25519`
in the checkout that built the image. With the machine's address from
`ip -4 addr`:

```sh
ssh -i .lab/base/id_ed25519 root@<the machine's address>
```

The desktop's logs are in `/var/log/crowbar-*.log`.
