# Compositor on NVIDIA: DRM, Void's `nvidia` package, and suspend and resume

**Result (November 15, 2026): nothing that was run gives a reason to reopen
[ADR 0015](../adr/0015-our-own-compositor-on-smithay.md).** The spike's
compositor ran on an RTX 4090 through DRM, on Void Linux with Void's `nvidia`
package, under runit and seatd with no systemd. The GPUI shell and Studio
Settings drew every frame on it, through a resize, at 3840×2160 and at
6144×2560, and drew again after a suspend to RAM without a restart. A flip
did not wait for its drawing (`needs_sync=false`), the path that had not run
anywhere before.

This is the record that [issue 5](https://github.com/crowbaros/crowbaros/issues/5)
asks for. It finishes the three NVIDIA runs that the
[spike record](compositor-spike-2026-11-15.md) lists as not run. The spike's
record stays as written. The evidence is in
[`evidence/compositor-nvidia-2026-11-15/`](evidence/compositor-nvidia-2026-11-15/):
the logs the runs wrote, copied byte for byte, and Void's package list.

## Summary

| Item in the issue | Result | Evidence |
| --- | --- | --- |
| The compositor on NVIDIA through DRM | Shown, at 3840×2160 at 60 Hz and 6144×2560 at 120 Hz. `needs_sync=false` on every run | `tty-apps-…181753Z.log`, `tty-apps-…184840Z.log` |
| Explicit sync | Not wired. Without it, Jim, watching the screen, saw no flicker, tearing, or frame out of order | [Explicit sync](#explicit-sync) |
| Void's `nvidia` package | Shown, under runit and seatd. Packages, parameters, and what Base must do are below | `void-packages.txt` |
| The GPUI apps for at least a minute, with a resize | Shown. Every surface committed in every interval. The resize was a maximize, made by a person, with Studio Settings patched for the run | `tty-apps-…184106Z.run.log` |
| Suspend and resume | Shown. 25 seconds in `deep` sleep through `zzz`; every surface drew again without a restart | `tty-suspend-…181921Z.log` |

**Nothing here was a run the compositor could not work around.** One run was
spoiled by the way it was started, not by the driver; see
[A run that was not drawn](#a-run-that-was-not-drawn).

## What was run on

- **Machine:** the spike's development host, booted from a separate SSD with
  its own Void install. The host's own disk and session were not used, so the
  GPU had no other user. One GPU, an NVIDIA GeForce RTX 4090. One monitor, on
  HDMI-A-1. It prefers 3840×2160 at 60 Hz and offers up to 6144×2560 at 120 Hz.
- **System:** Void Linux (glibc), Linux 6.18.63, runit, seatd 0.9.3, Mesa
  26.2.7, no systemd and no elogind.
- **Driver:** Void's `nvidia` 595.112.07 from the nonfree repository, as DKMS
  modules (`nvidia-dkms`), with the proprietary kernel modules.
- **Compositor:** `compositor/` from `main` at `f41268f`, plus the `--mode`
  option and the maximize log line this change adds, built on the machine in
  the debug profile. Smithay is the spike's pinned commit.
- **Apps:** `apps/shell` and `apps/studio-settings` from their pinned
  lockfiles (GPUI 0.2.2, blade), built on the machine. For the resize run
  only, Studio Settings had a two-line patch; see [Resize](#resize).

Each run's log names the SHA-256 of the three binaries it ran.

## The runs

Every run used [`compositor/checks/tty-apps.sh`](../../compositor/checks/tty-apps.sh),
new in this change. It is `nested-apps.sh` for a machine without a desktop
session: it starts the compositor on VT 2 with `openvt`, as the user with the
`_seatd` group and `LIBSEAT_BACKEND=seatd`, the way
[`lab/void/compositor_check.py`](../../lab/void/compositor_check.py) does in
the guest. It starts the shell and Studio Settings under it and counts commits
per surface every five seconds. It was run over SSH from another machine.

| Run | Mode | Length | Result |
| --- | --- | --- | --- |
| `tty-apps-20261115T181753Z` | 3840×2160 at 60 Hz, the monitor's preferred | 65 s, 13 intervals | Pass |
| `tty-suspend-20261115T181921Z` | 3840×2160 at 60 Hz | 125 s, 19 intervals, 25 s asleep | Pass |
| `tty-apps-20261115T182419Z` | 6144×2560 at 120 Hz | 150 s | Logged as a pass. **Not drawn after the first 45 s**; see below |
| `tty-apps-20261115T184106Z` | 6144×2560 at 120 Hz | 74 s, stopped early | The resize. Stopped by hand, so the script wrote no summary; the run log is complete |
| `tty-apps-20261115T184840Z` | 6144×2560 at 120 Hz | 65 s, 13 intervals | Pass, with the corrected script |

In every passing run, the bar, the dock, the wallpaper, and Studio Settings
each committed about 60 times a second in every interval, with none stalled.
The apps logged at `info`, where blade's warning for a stale swapchain would
show, and logged no `out of date` and no `Acquire failed`. Both named the
adapter `NVIDIA GeForce RTX 4090`. The compositor logged no DRM error.

The commits come at about 60 a second even at 120 Hz, because the spike
redraws on a 16 ms timer (a known defect in the spike record), and GPUI
commits once per frame callback.

### On DRM, and `needs_sync`

The compositor opened `/dev/dri/card0` with the `nvidia-drm` driver through
GBM, with the OpenGL ES renderer `NVIDIA GeForce RTX 4090/PCIe/SSE2` on driver
595.112.07. The output lit up at the mode asked for and stayed drawing.

Every run logged `output=HDMI-A-1 needs_sync=false`. Smithay skips the wait
for a frame's drawing before a flip on NVIDIA 560.35.3 or later, and this was
the first run of that path anywhere. Nothing in the logs or on the screen
showed a frame flipped before it was drawn.

### Explicit sync

The compositor still does not offer `linux-drm-syncobj-v1`. What was seen
without it: Jim watched the monitor during the 6144×2560 runs at 18:24 and
18:41 UTC and saw no flicker, no tearing, and no frame out of order. Of the
18:24 run, only the first 45 seconds were on screen (see below). The 18:41 run
was on screen throughout. So there was nothing for explicit sync to fix in
these runs, and it was not wired.

That does not show it is not needed. These clients are GPUI apps through
blade's Vulkan path, on one driver version. Other clients, such as EGL ones,
or other drivers, may behave differently.

### Resize

A TTY has no host window to resize, as `nested-apps.sh` had. So the resize was
a maximize, made by a person at the screen.

The first attempt did nothing. Studio Settings' title bar starts a window move
on mouse-down and does not handle a double-click, so it never asked to be
maximized. For the run, a copy of Studio Settings on the test machine had the
title bar's handler call GPUI's `zoom_window()` on a double-click, which sends
`xdg_toplevel.set_maximized`. That patch is not part of this change, and the
machine's copy was restored after the run.

With it, Jim double-clicked the title bar. The compositor logged `window
maximized width=6144 height=2420`, the screen less the bar and the dock, and
Studio Settings went on committing about 60 times a second at the new size for
about 30 seconds, until Jim closed it. A surface that is closed stops
committing, and the compositor's count keeps its last value, so the run log
shows Studio Settings as stalled after 18:41:50. The app's own log shows the
window closing. The shell's launcher was opened after it and committed.

### Suspend and resume

With the compositor and both apps running, the script set the RTC to wake the
machine in 25 seconds and suspended it with `zzz`, which ran Void's
`nvidia-suspend` hook. The kernel logged `PM: suspend entry (deep)` and, 25
seconds later, `PM: suspend exit`.

The compositor logged `tty: session paused` and `tty: session resumed` 0.1
seconds apart as the machine went to sleep, and five renders failed with
`Device is currently paused` between them. After resume, no render failed,
and each surface committed 1,490 more times in the 30 seconds watched.
Nothing was restarted.

### A run that was not drawn

The first 6144×2560 run, `tty-apps-20261115T182419Z`, logged `RESULT: pass`
and was not on screen for most of its length. It was started while the
3840×2160 run before it was still in its wait. That earlier run had been
stopped by hand, but its script woke at 18:25:05, and its exit handler
switched to VT 1. The newer compositor's session paused, and every render
from then to the end, 6,726 of them, failed with `Device is currently
paused`. Its clients went on committing, because the compositor goes on
sending frame callbacks while paused, so the commit counts looked healthy.

The script now refuses to start when another compositor is running on a TTY,
and fails a run when any render fails other than while paused, when the
session ends paused, or when a run without `--suspend` pauses at all. Replayed
on the four logs before it, those checks fail this run and pass the other
three. The 6144×2560 run was then made again with the corrected script
(`tty-apps-20261115T184840Z`) and passed.

Two attempts are not in the evidence, both stopped by mistakes in the script.
In the first, its process lookup gave up two seconds in and switched to VT 1
while the compositor was opening the GPU, so every mode-set failed with
`EACCES`. In the second, at 18:47, the new pause check exited the script
before its summary when the log had no pause in it. Both were fixed before the
runs above.

## Void's `nvidia` package

| Part | Value |
| --- | --- |
| Packages | `nvidia`, `nvidia-dkms`, `nvidia-libs`, `nvidia-firmware`, `nvidia-gtklibs`, 595.112.07; `dkms` 3.3.0; `linux6.18` and `linux6.18-headers` 6.18.63; `pahole` 1.32 |
| Kernel parameters | `nvidia-drm.modeset=1 nvidia-drm.fbdev=1` |
| Module options | `NVreg_PreserveVideoMemoryAllocations=1 NVreg_TemporaryFilePath=/var/tmp` |
| Sleep hooks | `/etc/zzz.d/suspend/nvidia-suspend` and `/etc/zzz.d/resume/nvidia-resume`, from the package |
| Services | `udevd`, `seatd`, `sshd`, `dhcpcd`, `chronyd`, the gettys. No elogind, no acpid service |

The full list is in `void-packages.txt`.

### What Base has to do that a systemd desktop does for free

The spike record lists making `XDG_RUNTIME_DIR` and putting the person in
`_seatd`. Both were needed here too: nothing made `/run/user/1000`, and the
script makes it. With NVIDIA, add:

- **Install `pahole` before `nvidia-dkms`.** The first DKMS build of the
  module failed without it, while the system was being installed. A rebuild
  after installing it succeeded. Base must order this, or build the module
  itself.
- **Keep GPU memory across suspend.** `NVreg_PreserveVideoMemoryAllocations=1`
  and the package's `zzz` hooks are what systemd's `nvidia-suspend.service`
  and `nvidia-resume.service` do elsewhere. They worked here. Base's suspend
  path must run them.
- **Choose the mode.** The compositor took the monitor's preferred mode,
  3840×2160 at 60 Hz, though the monitor offers 6144×2560 at 120 Hz. The
  `--mode` option this change adds is for the checks only. The real
  compositor needs output configuration, which the design brief lists.

## Seen on the way

- **The pointer is a white square** at both modes. The spike draws its own
  pointer and ignores clients' cursor images, as its record says. GPUI also
  logged `Unable to fallback on default cursor icon 'left_ptr'` for every
  pointer change, because no cursor theme is installed.
- **Studio Settings cannot be maximized** from its own title bar. It handles
  a drag and not a double-click ([issue 36](https://github.com/crowbaros/crowbaros/issues/36)). The
  compositor does not handle un-maximize.
- **The commit counts list a `window 0x1` with an empty app id and one
  commit**, next to Studio Settings, which is also `window 0x1`. It is
  probably the window's first commit, counted before its app id was set. It
  did not affect the result.
- `Unable to become drm master, assuming unprivileged mode` was logged at
  start in every run, as in the spike's guest. Output worked all the same.

## Not run

- **A resize by the client or by the compositor on its own.** The only resize
  was a maximize, with a patched Studio Settings.
- **Other NVIDIA drivers**, the open kernel modules on Void, a second output,
  or hot-plug.
- **Hibernation.** Void's hook handles it; it was not tried.
- **Clients other than GPUI ones**, which may need explicit sync where these
  did not.
- **The Milestone 1 loop** (`lab/void/void_lab.py milestone`). Nothing in
  `lab/void/` or in how Base is built changed.

## Other runs

- **The tests:** the 20 tests in `compositor/` pass, on the test machine.
  `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` are
  clean there.

## How to repeat

On a machine with an NVIDIA GPU and nothing else using it, seatd running, and
the user in `_seatd` with passwordless `sudo`, over SSH:

```sh
cd compositor && cargo build
(cd ../apps/shell && cargo build) && (cd ../apps/studio-settings && cargo build)
shell=../apps/shell/target/debug/crowbaros-shell
studio=../apps/studio-settings/target/debug/crowbaros-studio-settings

checks/tty-apps.sh "$shell" "$studio"
checks/tty-apps.sh --suspend "$shell" "$studio"
COMPOSITOR_ARGS="--mode HDMI-A-1=6144x2560" WATCH_SECONDS=150 checks/tty-apps.sh "$shell" "$studio"
```

The logs are written to `.lab/compositor/`. Start one run at a time.

*Made by Claude Opus 5.5 in Claude Code (run through T3 Code), with Jim at the
screen for the explicit-sync and resize checks.*
