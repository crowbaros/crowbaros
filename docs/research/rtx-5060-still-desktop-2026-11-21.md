# A still desktop draws nothing on the RTX 5060

**Result (November 21, 2026): on the RTX 5060 machine, a still desktop, locked
or unlocked, draws one frame a minute, when the clock changes, and nothing
else.** This is the row that failed in the
[RTX 5060 record](fulcrum-rtx-5060-2026-11-20.md), where the lock screen drew
about 7 frames a second
([#269](https://github.com/crowbaros/crowbaros/issues/269)). The fix,
[#280](https://github.com/crowbaros/crowbaros/pull/280), holds on real
hardware with NVIDIA's driver.

This finishes the record that
[issue 227](https://github.com/crowbaros/crowbaros/issues/227) asks for. The
other rows stand as the RTX 5060 record has them. Jim Martin unlocked the
machine at its screen. A coding agent drove the rest over SSH and read the
logs. The evidence is in
[`evidence/rtx-5060-still-desktop-2026-11-21/`](evidence/rtx-5060-still-desktop-2026-11-21/).

## The machine and the image

The machine, image, and boot of the
[NVIDIA suspend record](nvidia-suspend-2026-11-21.md). This run is the same
boot, later in the day.

| | |
| --- | --- |
| Machine | Ryzen 5 7500X3D, 16 GB, UEFI with Secure Boot off |
| GPU | GeForce RTX 5060 (GB206, Blackwell) |
| Monitor | One, on HDMI-A-2, 3840x2160 at 59.982 Hz |
| Commit | `8b8eabbe`, on `main` at `1a0ae9d2`, which is #280 |
| Kernel | `linux6.18-6.18.64_1` |
| Driver | `nvidia-595.112.07_2`, NVIDIA's open kernel module |
| Account | The person's own, from `base/account.json` |

Fulcrum logged `renderer=gles gpu=NVIDIA GeForce RTX 5060/PCIe/SSE2` and
`pointer=cursor-plane` on this boot. Times are UTC.

## How it was measured

Since #280, Fulcrum writes a `commits so far` report after a commit or a
frame, at most every 5 seconds, and never on a timer. A still desktop writes
none. Each report counts, from boot, the commits of each of the shell's layers
and the frames Fulcrum drew. The reports were watched with `tail -f` over SSH,
each stamped with the time it arrived. No app was open, and nobody touched the
keyboard or mouse during a stretch.

## Results

| Row | Result | Evidence |
| --- | --- | --- |
| Unlocked, untouched for 109 minutes | **Pass.** 110 frames from 19:09:28 to 20:58:43: one a minute | `compositor.log` lines 89 to 202 |
| Unlocked, timed | **Pass.** One report a minute, on the minute, each one frame | `timed.txt`, 20:38 and 20:39 |
| Locked | **Pass.** Locked over SSH at 20:58:43. The lock screen drew 62 frames as it came in, about a second at 60 Hz, then one a minute: 20:59, 21:00, 21:01 | `timed.txt` |
| Unlocked by password, then still | **Pass.** Jim unlocked at the screen at 21:01:43 and saw the lock screen and the desktop drawn as they should be. After the lock screen faded out, one frame a minute: 21:03, 21:04 | `timed.txt`, `shell.log` |

Before #280, on this machine, Fulcrum drew about 7 frames a second on the
untouched lock screen, and each layer committed about 61 times a second.

## Noticed

- Each minute, the bar, the dock, and the wallpaper each commit two or three
  times, but Fulcrum draws one frame. Commits that damage nothing draw
  nothing, as #280 meant. They cost the shell a little work each minute, and
  no frames.
- The `lock` reply came back at once, at 20:58:43. This build is older than
  [#288](https://github.com/crowbaros/crowbaros/pull/288), which makes the
  reply wait until every display shows the session locked.

## Not run

- A still desktop with an app open. The VM's desktop check covers that case
  since #280, with `foot` and Studio Settings open.
- A second monitor. Only one was plugged in.

## The evidence

| File | What it is |
| --- | --- |
| `compositor.log` | Fulcrum's whole log for the boot, as it was on the disk. Its first 90 lines are the NVIDIA suspend record's |
| `shell.log` | The shell's log for the boot without GPUI's renderer messages (`blade_graphics` and `naga`) |
| `timed.txt` | The three watched stretches, each report with the time it was written |
