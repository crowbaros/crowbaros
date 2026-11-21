# The desktop on the integrated Radeon, again, after the fixes

**Result (November 21, 2026): on the test machine's integrated Radeon 610M, a
still desktop draws nothing but the clock, and each app's GPU is known.** With
both GPUs on, Fulcrum showed the desktop on the Radeon, the shell and Studio
Settings drew on the RTX 5060, Chromium drew on the Radeon, and `foot` drew on
neither. The two rows the [Radeon 610M record](fulcrum-radeon-610m-2026-11-20.md)
left with issues now pass: the shell waits for a monitor that is unplugged
([#282](https://github.com/crowbaros/crowbaros/pull/282)), and the shell is
told the session is locked before the machine suspends
([#288](https://github.com/crowbaros/crowbaros/pull/288)).

This finishes the record that
[issue 275](https://github.com/crowbaros/crowbaros/issues/275) asks for. The
rows not named here stand as the Radeon 610M record has them. Jim Martin was at
the machine for the rows done by hand. A coding agent built and wrote the
image, drove the reboots and the suspend over SSH, and read the logs. The
evidence is in
[`evidence/radeon-610m-rerun-2026-11-21/`](evidence/radeon-610m-rerun-2026-11-21/).

## The machine and the image

| | |
| --- | --- |
| Machine | The Radeon 610M record's: a Ryzen 5 7500X3D, 16 GB, an AMD B650 board, UEFI with Secure Boot off |
| GPUs | The CPU's Radeon 610M (Raphael, RDNA 2), with Integrated Graphics set to Force in the firmware, and the RTX 5060 in its slot |
| Monitor | One, on the motherboard's HDMI (`HDMI-A-1`), 3840x2160 at 59.982 Hz, 600x340 mm |
| Commit | `main` at `6de90ace`, which has #280, #282, #283, and #288 |
| Kernel | `linux6.18-6.18.64_1` |
| Mesa | `mesa-dri-26.2.7_1`: `radeonsi` for Fulcrum's GL, RADV for Vulkan |
| NVIDIA | `nvidia-595.112.07_2`, NVIDIA's open kernel module |
| Account | The person's own, from `base/account.json` |

`base/void_base.py build` wrote the image, which was written whole to the
machine's USB SSD. Times are UTC.

## Results

The first boot had both GPUs on, as the image ships. For the second boot, one
file added by hand to the SSD, `/etc/modprobe.d/nvidia-off-test.conf`, kept
NVIDIA's modules from loading. It was removed after the run.

| Row | Result | Evidence |
| --- | --- | --- |
| Boots the machine on the motherboard's output | **Pass**, both boots | `compositor.log` lines 2 and 108 |
| `renderer=gles` with the Radeon | **Pass**, both boots: `radeonsi, raphael_mendocino` | `compositor.log` lines 17 and 123 |
| The pointer on the cursor plane, the default scale | **Pass.** `pointer=cursor-plane`, `scale=180` | `compositor.log` lines 18, 19 |
| Unlock by password; open `foot`, Studio Settings, and Chromium; arrange; copy and paste between `foot` and Chromium both ways | **Pass**, by hand, both GPUs on | `compositor.log` line 38, `shell.log` line 39 |
| Which GPU each app drew on, both GPUs on | **Recorded.** The shell: the RTX 5060. Studio Settings: the RTX 5060. Chromium: the Radeon. `foot`: neither, since it draws with the CPU into shared memory. Fulcrum showed all of them on the Radeon | `gpu-per-app.txt`, `shell.log` line 7 |
| Which GPU each app drew on, NVIDIA off | **Recorded.** The shell used RADV on the Radeon | `shell.log` line 96 |
| A still desktop, locked, both GPUs on | **Pass.** One frame a minute, when the clock changed, for three minutes | `timed.txt` |
| A still desktop, unlocked with `foot`, Studio Settings, and Chromium open, both GPUs on | **Pass.** No app committed. Two frames a minute, when the clock changed, and none between, for three minutes | `timed.txt` |
| A still desktop, locked, the Radeon alone | **Pass.** One frame a minute, for three minutes | `timed.txt` |
| Locked across suspend | **Pass.** At 21:32:15 Fulcrum locked, drew the lock screen, and the shell had the `locked` event, before the kernel suspended at 21:32:16. It woke at 21:33:08. Jim saw the lock screen at once on wake, then unlocked to a working desktop with the three apps | `compositor.log` lines 67 to 82, `shell.log` line 42, `kernel.log` lines 103 to 136 |
| Suspend and resume | **Pass**, into `deep` sleep. `amdgpu` logged no errors on resume | `kernel.log` lines 116, 117 |
| Monitor unplugged and plugged in | **Pass.** With no output, the shell exited, runit started it once, and it waited for a display. It came back on its own when the monitor returned, 14 seconds later. No panic | `compositor.log` lines 99 to 105, `shell.log` lines 59 to 62 |

## Noticed

- The shell, Studio Settings, and Chromium chose their GPUs differently with
  both on: GPUI's Vulkan picked the RTX 5060, and Chromium's GPU process
  opened the Radeon's render node. This run did not look at why. Each drew
  correctly either way.
- Unlocked, with apps open, Fulcrum drew two frames each minute, where the
  RTX 5060 record saw one. Both came when the clock changed.
- At boot, `amdgpu` logged a `REG_WAIT timeout` in `optc31_disable_crtc`
  (`kernel.log` line 100), as it takes over the screen from the firmware. The
  desktop came up as it should.

## Not run

- The desktop flow, suspend, and the monitor rows on the Radeon alone. The
  Radeon 610M record ran them that way, and Jim judged the both-GPU run
  enough.
- Save a theme and reboot. The Radeon 610M record's row stands.
- A second monitor, a panel dense enough to need another scale, Wi-Fi.

## The evidence

| File | What it is |
| --- | --- |
| `compositor.log` | Fulcrum's log for both boots: both GPUs on (from line 1) and the Radeon alone (from line 108) |
| `shell.log` | The shell's log for both boots, without GPUI's per-buffer and shader messages |
| `kernel.log` | The kernel's lines about the GPUs, the display, and suspend, from the first boot |
| `gpu-per-app.txt` | Each render node's PCI address, the DRM nodes each app had open, and the Vulkan adapter each GPUI app chose |
| `timed.txt` | The three watched stretches, each report with the time it was written |
