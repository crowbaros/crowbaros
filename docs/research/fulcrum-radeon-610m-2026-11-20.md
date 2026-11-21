# Base and Fulcrum on an integrated Radeon, with Mesa

**Result (November 20, 2026): Base's image runs the desktop on Mesa, on the
CPU's integrated Radeon 610M, on real hardware.** Fulcrum drew with
`renderer=gles` on `radeonsi`, put the pointer on the cursor plane, and drew
GPU clients' buffers, and RADV used the explicit sync it offers. The desktop
flow passed by hand. The rows found the same bugs as the
[RTX 5060 record](fulcrum-rtx-5060-2026-11-20.md): a still desktop keeps
drawing ([#269](https://github.com/crowbaros/crowbaros/issues/269)), and the
shell crashes while the monitor is unplugged
([#271](https://github.com/crowbaros/crowbaros/issues/271)). Suspend woke to
the lock screen, but the lock can reach the screen after the machine
suspends ([#276](https://github.com/crowbaros/crowbaros/issues/276)).

This is the record that
[issue 275](https://github.com/crowbaros/crowbaros/issues/275) asks for, run
by hand with [Base's hardware guide](../../base/HARDWARE.md) on the machine
that ran the RTX 5060 record, a few minutes later. Jim Martin was at the
machine for the rows done by hand, and a coding agent drove the rest over SSH
and read the logs. The evidence is in
[`evidence/fulcrum-radeon-610m-2026-11-20/`](evidence/fulcrum-radeon-610m-2026-11-20/).
Jason's own Mesa machine is
[#228](https://github.com/crowbaros/crowbaros/issues/228), a separate run.

## The machine and the image

| | |
| --- | --- |
| Machine | The RTX 5060 record's: a desktop PC with a Ryzen 5 7500X3D, 16 GB, an AMD B650 board, and UEFI with Secure Boot off |
| GPU | The CPU's Radeon 610M (Raphael, RDNA 2, `1002:164e`), turned on in the firmware: Integrated Graphics set to Force. The RTX 5060 stayed in its slot |
| Monitor | One, on the motherboard's HDMI, 3840x2160 at 59.982 Hz, 600x340 mm |
| Commit | `main` at `3bba5262`, the same image as the RTX 5060 record |
| Kernel | `linux6.18-6.18.64_1`, with `amdgpu` |
| Mesa | `mesa-dri-26.2.7_1`: `radeonsi` for Fulcrum's GL, RADV for the apps' Vulkan |
| Network | Wired Ethernet |
| Account | The person's own, from `base/account.json` |

Base's image already carries the GPU's firmware, from `linux-firmware-amd`.
Times in the logs are UTC.

## NVIDIA off, and the first boot with both GPUs

On the first boot both GPUs were on. Fulcrum picks the first GPU with a
monitor connected, so it chose the Radeon. GPUI in the shell picked the RTX
5060 for its Vulkan (`shell.log` line 7), so the shell drew on one GPU and
Fulcrum showed it on the other. The lock screen drew correctly that way. No
further rows were run with both GPUs.

For the run, one file was added to the SSD's root by hand,
`/etc/modprobe.d/nvidia-off-test.conf`, which keeps `nvidia`, `nvidia_drm`,
`nvidia_modeset`, `nvidia_uvm`, and `nouveau` from loading. From then on the
Radeon was the only GPU (`card0` alone), and the shell and Studio Settings
used RADV. The image does not carry the file.

## Results

| Row | Result | Evidence |
| --- | --- | --- |
| Built with the person's account and boots the machine | **Pass**, on the motherboard's output | `compositor.log` line 1 |
| The network | **Pass**, wired, on boot | |
| Unlock by password | **Pass.** A wrong password was refused, then the right one unlocked | `shell.log`: `unlock ok=false`, `unlock ok=true` |
| Launch `foot` and Studio Settings, arrange windows | **Pass.** The same moves as on the RTX 5060. Jim saw no tearing or stutter, and the same late bar on a workspace change ([#273](https://github.com/crowbaros/crowbaros/issues/273)) | |
| Copy and paste between apps | **Pass**, between `foot` and Chromium, both ways | |
| Save a theme, reboot, the theme kept | **Pass.** Saved Dark with the Nightwatch wallpaper. After a clean reboot the shell started on it, locked | `shell.log`: `appearance theme=Dark wallpaper=Nightwatch` at 04:56:37 |
| `renderer=gles` with the GPU | **Pass**, on all three boots | `compositor.log`: `renderer=gles gpu=AMD Ryzen 5 7500X3D 6-Core Processor (radeonsi, raphael_mendocino, ...)` |
| A GPU client's buffers are drawn | **Pass.** Fulcrum imports dma-bufs with the GPU (`formats=356`). The shell, Studio Settings, and Chromium drew on the Radeon | `compositor.log`, `shell.log`: `Adapter: ... (RADV RAPHAEL_MENDOCINO)` |
| Explicit sync | **Pass.** Studio Settings on RADV sent its buffers as dma-bufs and set an acquire and a release point on each | `studio-settings-wayland-debug.log` |
| The pointer on the cursor plane | **Pass** | `compositor.log`: `pointer=cursor-plane` |
| A still desktop draws no frames | **Fail.** On the untouched lock screen, over 15 seconds, each of the shell's layers committed about 61 times a second and Fulcrum drew about 7 frames a second, as on the RTX 5060. [#269](https://github.com/crowbaros/crowbaros/issues/269) | `compositor.log`: the `commits so far` lines of the second boot, before `session unlocked` |
| Monitor unplugged and plugged in | **Pass for the person, with a shell crash.** The desktop came back. With no output, the shell panicked in GPUI's Vulkan backend, as on NVIDIA, and runit restarted it about seven times. [#271](https://github.com/crowbaros/crowbaros/issues/271) | `compositor.log` lines 176 to 192, `shell.log` line 77 |
| The default scale | **Pass.** 180%, from the same monitor's size, as on the RTX 5060 | `compositor.log`: `scale=180` |
| Suspend and resume | **Pass**, twice, through `zzz` into `deep` sleep. Both woke to the lock screen and unlocked to a working desktop. `amdgpu` logged no errors on resume | `kernel.log`: `PM: suspend entry` and `exit` |
| Locked across suspend | **Pass, with a gap.** Fulcrum locked before each suspend (`session locked by=authority`), but the shell logged the `locked` event only on resume, at 04:58:24 and 05:01:05, after the kernel had suspended at 04:58:05 and 05:00:46. Jim, watching the second wake, saw the lock screen, perhaps after a very quick flash. Fulcrum answers the lock before its locked frame is on screen. [#276](https://github.com/crowbaros/crowbaros/issues/276) | `shell.log`, `kernel.log` |

## Not run

- The rows with both GPUs on, beyond the lock screen on the first boot.
- A second monitor, or a panel dense enough to need another scale.
- Wi-Fi.

## The evidence

| File | What it is |
| --- | --- |
| `compositor.log` | Fulcrum's log for this run's three boots: both GPUs on (from line 1), the Radeon alone (from line 45), and after the reboot that kept the theme (from line 131) |
| `shell.log` | The shell's log for those boots, without GPUI's renderer messages except the adapter it chose |
| `kernel.log` | The kernel's lines about the GPUs, the display, and suspend, from those boots |
| `studio-settings-wayland-debug.log` | Studio Settings' dma-buf and explicit-sync requests from a 6-second run on RADV with `WAYLAND_DEBUG=1`, and a count of each |
