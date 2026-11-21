# Running Fulcrum on real hardware

The plan's hardware rows ask for CrowbarOS's desktop on two machines, from the same Base image:
- the test machine, with an RTX 5060, on NVIDIA's driver ([crowbaros#227](https://github.com/crowbaros/crowbaros/issues/227));
- Jason Alexander's machine, on Mesa ([crowbaros#228](https://github.com/crowbaros/crowbaros/issues/228)).

CI and CrowbarOS's checks cover the rest in the VM, on llvmpipe and virtio-gpu, so these runs are what show the compositor on a real GPU. This page says how to run them, what to look at, and what to record. Each run's results go in a dated record of its own, `docs/hardware-<machine>-<date>.md`, with the log attached.

## What the runs show

| Row | What to see | Milestone |
| --- | --- | --- |
| The desktop flow | Unlock by password, launch `foot` and Studio Settings, arrange windows, copy and paste between them, save a theme, reboot, and find the theme kept | 9 |
| The GPU draws | `renderer=gles` with the machine's GPU, not llvmpipe or the CPU | 3 |
| A client's dma-buf is drawn where it is | A GPU client runs, and the log says how many formats the GPU imports | 3 |
| Explicit sync | `wp_linux_drm_syncobj_manager_v1` is offered; on NVIDIA, GPU clients draw without tearing or stalls | 3 |
| The cursor plane | `pointer=cursor-plane`, and moving the pointer over a still desktop draws no frame | 3 |
| A still desktop draws nothing | The frame count in the log stops while nothing moves | 3 |
| Displays come and go | A monitor unplugged and plugged in again: windows stay, the layout comes back | 7 |
| A readable scale by default | A display the layout does not name starts at a scale picked from its density, and text is readable at it | 7 |
| Suspend and resume | After resume, the displays show frames again | 7 |
| Locked across suspend | Locked before suspend, with a second monitor attached: after resume, every display shows the lock screen or black, and no client hears a key before unlock | 8 |

The first six must pass on both machines. The last four are run and recorded; a failure opens an issue.

## Before

- **The image:** build it with your own account file, write it to a disk, and boot the machine from it, as [Base on a real machine](../../base/HARDWARE.md) says. The machine comes up on the lock screen, with the compositor, the shell, the launcher, and the unlock helper running as runit services. Nothing is built on the machine.
- **A second way in:** SSH from another machine, with the key Base's build made, as Base's guide says. Read the logs from there while you work at the desktop.
- **Clients:** `foot` and Studio Settings are in the launcher. For a GPU client, open `foot` and install one from Void's repository: `sudo xbps-install -S vulkan-tools mesa-demos`, for `vkcube` and `eglgears_wayland`. `crowbar-probe` is in the image too: `crowbar-probe globals`, run in `foot`, lists what an app is offered.
- **Displays:** to read and set a display's mode, place, and scale, the person's account talks to the shell's display bridge, `/run/crowbar-displays/socket`, one JSON request per line. The image has no client for it, so install `socat` the same way.

## What to look at

The compositor's log is `/var/log/crowbar-compositor.log`, at `FULCRUM_LOG=info`. Near the start of each run of the service:

- `renderer=gles gpu=…` with the machine's GPU. `renderer=cpu` means GBM or EGL did not load, and the runs show nothing about the GPU.
- `linux-dmabuf imported by the GPU formats=N`: N is in the tens on a real GPU.
- `output scale from its density name=… scale=… physical=…mm`, for each monitor the layout does not name. The physical size should be the display's own, and the scale one you can read at: 120 is 1, 180 is 1.5.
- `output connected name=… size=… refresh=… pointer=cursor-plane`, for each monitor. `pointer=frame` means the display offered no cursor plane, or refused one: a line saying so follows.
- `no explicit sync` must **not** appear on NVIDIA.

Later:

- **`commits so far: … frames=N` lines.** With nothing moving for ten seconds, N stays the same: a still desktop draws nothing. Moving the pointer adds frames that only move the cursor plane, and then N stops again.
- **No `could not show a frame`.**

The shell's log is `/var/log/crowbar-shell.log`, the apps' `/var/log/crowbar-apps.log`, and the system's, with the kernel's messages, `/var/log/socklog/everything/current`.

Then each row:

1. **The desktop flow:** at the lock screen, type a wrong password first, then yours. Tap Super for the launcher, and open `foot` and Studio Settings. Arrange them: move and resize with Super and the left and right mouse buttons, maximize with Super+Up, fullscreen with Super+F, snap with Super+Left and Right. Fullscreen keeps its frame inside the display's edge. Copy text in one and paste it in the other. Save a theme in Settings, `sudo reboot`, unlock, and find the theme kept.
2. **GPU clients:** `vkcube` spins smoothly in its window, and `eglgears_wayland` turns. On NVIDIA, watch for tearing and stalls.
3. **Displays:** unplug a monitor and plug it in again. The log says `output disconnected` and `output connected`. Windows that were on it are on a display again, and a mode or scale set through the display bridge comes back. As the person, in `foot`:

   ```sh
   echo '{"op":"outputs"}' | socat - UNIX-CONNECT:/run/crowbar-displays/socket
   echo '{"op":"configure_output","name":"eDP-1","scale":180}' | socat - UNIX-CONNECT:/run/crowbar-displays/socket
   ```

   The scale is in 120ths, as fractional scale counts, so 180 is 1.5. The compositor keeps what is set in `/home/crowbar-compositor/layout`.
4. **Scale:** check that the scale each display started at is readable. `foot` redraws sharp at a scale you set. With a second monitor at another scale, drag a window from one display to the other.
5. **Suspend and resume:**
   - Suspend with `sudo zzz`, then resume.
   - The displays show the desktop again, the `frames=` count grows when something moves, and the pointer is drawn.
   - Also switch to the first console with Ctrl+Alt+F1 and back with Ctrl+Alt+F2, which seatd handles the same way.
6. **Lock:** zzz's suspend hook locks the desktop before the machine sleeps. With a second monitor attached, suspend and resume: every display shows the lock screen or black, and no app hears a key before you unlock. Super+L locks too, and Ctrl+Alt+Enter neither locks nor unlocks.

To stop or restart the compositor, from SSH as root: `sv restart crowbar-compositor`. It comes back locked.

## What to record

A dated record per machine, `docs/hardware-<machine>-<date>.md`. Dated records stay as written, so a second run gets a record of its own. It should say:

- the machine, its GPU and driver, the kernel, and the commit the image was built from;
- each row above, met or not, with what was seen;
- the start of the log, the `renderer=`, `linux-dmabuf`, and `output connected` lines, and a still-desktop stretch of `frames=` lines;
- what broke, with the log around it.

Attach the whole log under `docs/assets/hardware-<machine>-<date>/`, as the first-light record does. Leave out anything about the network the machine is on: its address, its Wi-Fi's name, and the machine's own name.
