# NVIDIA keeps the desktop across suspend

**Result (November 21, 2026): Base's image, with its own NVIDIA options and
nothing added by hand, suspended to RAM twice on an RTX 5060 and woke each
time to the lock screen, with no Xid, and unlocked to a working desktop.**

This is the hardware run that
[issue 272](https://github.com/crowbaros/crowbaros/issues/272) asks for. On
November 20 the same machine woke grey after a suspend, and a file added to
the root by hand fixed it ([RTX 5060 record](fulcrum-rtx-5060-2026-11-20.md)).
The image now carries that setting itself, in
`/etc/modprobe.d/crowbaros-nvidia.conf`. Jim Martin was at the machine and
read the screen. A coding agent drove the rest over SSH and read the logs. The
evidence is in
[`evidence/nvidia-suspend-2026-11-21/`](evidence/nvidia-suspend-2026-11-21/).

## The machine and the image

| | |
| --- | --- |
| Machine | The machine of the [RTX 5060 record](fulcrum-rtx-5060-2026-11-20.md): Ryzen 5 7500X3D, 16 GB, UEFI with Secure Boot off |
| GPU | GeForce RTX 5060 (GB206, Blackwell), 8151 MiB, 531 to 819 MiB in use |
| Monitor | One, on HDMI-A-2, 3840x2160 |
| Disk | 4 TB USB-C SSD, given whole to the image |
| Commit | `8b8eabbe`, the fix for #272, on `main` at `1a0ae9d2` |
| Kernel | `linux6.18-6.18.64_1` |
| Driver | `nvidia-595.112.07_2`, NVIDIA's open kernel module |
| Account | The person's own, from `base/account.json` |

The image was built with `base/void_base.py build`, written to the SSD with
`dd`, and booted from a one-time UEFI boot entry for the SSD's
`\EFI\BOOT\BOOTX64.EFI`. Times are UTC.

## Results

| Row | Result | Evidence |
| --- | --- | --- |
| The image sets the module's options | **Pass.** The file the build wrote was there, and the driver read it: `PreserveVideoMemoryAllocations: 1`, `TemporaryFilePath: "/var/tmp"` | `state.txt` |
| `/var/tmp` is on the disk | **Pass.** Btrfs, the root, which had grown to fill the 4 TB SSD | `state.txt` |
| Suspend from the lock screen | **Pass.** `zzz` at 19:06:03, `deep` sleep (S3), woken by the RTC at 19:06:49. No Xid. Jim saw the lock screen drawn, and unlocking gave a working desktop | `kernel.log`, `compositor.log` lines 24 to 58 |
| Suspend from the unlocked desktop | **Pass.** Base's hook locked the session first. `zzz` at 19:08:08, S3, woken by the RTC at 19:08:54. No Xid. Jim saw the lock screen, and unlocking gave the desktop as he left it | `kernel.log`, `compositor.log` lines 60 to 90 |
| Fulcrum kept running | **Pass.** The same process through both suspends, and the session came back after each | `compositor.log`: `the session is back` |
| The kernel logged nothing critical | **Pass** | `kernel.log` |

## How it ran

Each suspend was `rtcwake -m no -s 45` to set an alarm, then `zzz`, run as
root over SSH. Void's `nvidia` package's hooks ran `nvidia-sleep.sh`, which
switched to virtual terminal 63, so Fulcrum paused its session, and wrote
`suspend` to `/proc/driver/nvidia/suspend`. With
`PreserveVideoMemoryAllocations` at 1, the driver saves all video memory then,
in an unnamed file under `TemporaryFilePath`. Such a file has no name to find
afterwards, so this run shows where it would go, not the file.

On the second suspend, Fulcrum logged one warning as the hook switched
terminals, between locking and pausing:

```text
WARN crowbar_compositor: could not show a frame on HDMI-A-2: Permission denied (os error 13)
```

The switch had taken the display from it. The lock screen it had committed was
what Jim saw on waking.

The kernel also logged `xHC error in resume, USBSTS 0x401, Reinit` for one USB
controller on each resume. The keyboard and mouse worked after both.

## Not run

- Hibernate. Base has no swap to hibernate to.
- A suspend with the GPU's memory nearly full. At most 819 MiB was in use.
- A suspend woken by the keyboard or the power button. The RTC woke it both
  times.

## The evidence

| File | What it is |
| --- | --- |
| `compositor.log` | Fulcrum's whole log for the boot, as it was on the disk |
| `kernel.log` | The kernel's lines about the drivers, the display, and suspend |
| `state.txt` | The unlock helper's log, both runs' `zzz` output, the module's options and the driver's values for them, the file system under `/var/tmp`, the sleep mode, the GPU's memory, and the package versions |
