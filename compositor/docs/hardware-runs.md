# Running Fulcrum on real hardware

The plan's hardware rows ask for Fulcrum on three machines:
- a desktop with an RTX 4090, on NVIDIA's driver;
- a machine with an Intel GPU, on Mesa;
- Jim's Surface Laptop, a Snapdragon on aarch64.

CI and the Void guest check the rest, on llvmpipe and virtio-gpu, so these runs are what show the compositor on a real GPU. This page says how to run it on one of those machines, what to look at, and what to record. Each run's results go in a dated record of their own, `docs/hardware-<machine>-<date>.md`, with the log attached.

## What the runs show

| Row | What to see | Milestone |
| --- | --- | --- |
| The GPU draws | `renderer=gles` with the machine's GPU, not llvmpipe or the CPU | 3 |
| A client's dma-buf is drawn where it is | A GPU client runs, and the log says how many formats the GPU imports | 3 |
| Explicit sync | `wp_linux_drm_syncobj_manager_v1` is offered; on NVIDIA, GPU clients draw without tearing or stalls | 3 |
| The cursor plane | `pointer=cursor-plane`, and moving the pointer over a still desktop draws no frame | 3 |
| A still desktop draws nothing | The frame count in the log stops while nothing moves | 3 |
| Displays come and go | A monitor unplugged and plugged in again: windows stay, the layout comes back | 7 |
| Scale on a dense panel | The Surface's panel at 1.5 or 2: text is sharp, and windows are the right size | 7 |
| Suspend and resume | After resume, the displays show frames again | 7 |
| Locked across suspend | Locked before suspend, with a second monitor attached: after resume, every display shows the lock screen or black, and no client hears a key before unlock | 8 |

## Before

- **Build:** `cargo build --release` in `compositor/`. It needs Rust, libxkbcommon, and libinput, and at run time Mesa's EGL, GLES, and GBM. On NVIDIA, the driver's GBM and EGL take their place, with `nvidia-drm.modeset=1` on the kernel's command line.
- **The seat:** seatd must be running, and you must be in its group. On Void that is the `_seatd` group and the `seatd` service; elsewhere, often `seat`.
- **Clients:**
  - `foot` for a plain client;
  - a GPU client for dma-bufs and explicit sync: `vkcube` (vulkan-tools), or `eglgears_wayland` (mesa-demos);
  - `crowbar-probe`, from Fulcrum's build, which lists globals and has apps that copy, paste, drag, and drop.
- **A second way in:** ssh from another machine, or a second virtual terminal. Fulcrum holds the virtual terminal it runs on, and has no key to quit.

## Start it

From a free virtual terminal (Ctrl+Alt+F3), logged in as yourself:

```sh
export XDG_RUNTIME_DIR=/run/user/$(id -u)
D=$XDG_RUNTIME_DIR/fulcrum
mkdir -p -m 0700 $D
FULCRUM_LOG=debug ./target/release/crowbar-compositor --backend tty \
  --shell-socket $D/shell --shell-uid $(id -u) \
  --shell-control-socket $D/control \
  --apps-socket $D/apps --apps-uid $(id -u) \
  --layout $D/layout 2> $D/compositor.log &
```

You play both the shell and the person, so their user IDs are your own. The screen shows the background and the pointer as a white square, or the cursor theme's arrow if `XCURSOR_THEME` names one that is installed.

## Clients

- **The shell's socket** offers what an app is offered, and layer shell:

  ```sh
  WAYLAND_DISPLAY=$D/shell foot &
  WAYLAND_DISPLAY=$D/shell vkcube --wsi wayland &
  WAYLAND_DISPLAY=$D/shell crowbar-probe globals
  ```

- **The apps' socket** offers what a Wayland app expects, the GPU's buffers included. Run the GPU clients here too:

  ```sh
  WAYLAND_DISPLAY=$D/apps foot &
  WAYLAND_DISPLAY=$D/apps vkcube --wsi wayland &
  WAYLAND_DISPLAY=$D/apps crowbar-probe globals
  ```

- **The control channel**, from another terminal or over ssh. Each line is one request:

  ```sh
  printf '%s\n' '{"op":"outputs"}' | python3 -c '
  import socket, sys; s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1])
  s.sendall(sys.stdin.buffer.read()); print(s.makefile().readline(), end="")' $D/control
  ```

  Replace the request to change a display, as `{"op":"configure_output","name":"eDP-1","scale":180}` for a scale of 1.5. The scale is in 120ths, as fractional scale counts.

Stop it with `pkill crowbar-compositor`, from ssh or another virtual terminal.

## What to look at

In `$D/compositor.log`, near the start:

- `renderer=gles gpu=…` with the machine's GPU. `renderer=cpu` means GBM or EGL did not load, and the runs show nothing about the GPU.
- `linux-dmabuf imported by the GPU formats=N`: N is in the tens on a real GPU.
- `output connected name=… size=… refresh=… pointer=cursor-plane`, for each monitor. `pointer=frame` means the display offered no cursor plane, or refused one: a line saying so follows.
- `no explicit sync` must **not** appear on NVIDIA.

Later:

- **`commits so far: … frames=N` lines.** With nothing moving for ten seconds, N stays the same: a still desktop draws nothing. Moving the pointer adds frames that only move the cursor plane, and then N stops again.
- **No `could not show a frame`.**

Then each row:

1. **GPU clients:** `vkcube` spins smoothly in its window, and `foot` types. Move and resize them with Super and the mouse buttons. Maximize with Super+Up, fullscreen with Super+F. Fullscreen keeps its frame inside the display's edge.
2. **Displays:** unplug a monitor and plug it in again. The log says `output disconnected` and `output connected`. Windows that were on it are on a display again, and a mode or scale set with `configure_output` comes back from the `--layout` file.
3. **Scale:**
   - On the Surface, set the panel to 180 (1.5) or 240 (2) with `configure_output`.
   - `foot` redraws sharp, at the new size. A surface's preferred fractional scale follows. `WAYLAND_DEBUG=1` on a client shows `preferred_scale`.
   - With a second monitor at 120, check mixed scale: drag a window from one display to the other.
4. **Suspend and resume:**
   - Suspend: `zzz` on Void, or `loginctl suspend` where elogind or systemd runs. Then resume.
   - The displays show the desktop again, the `frames=` count grows when something moves, and the pointer is drawn.
   - Also switch to another virtual terminal and back, which seatd handles the same way.
5. **Lock:**
   - Super+L, or Ctrl+Alt+Enter, locks.
   - With no shell to draw the lock screen, every display goes black, and keys reach no client.
   - Unlock it from the control channel: `{"op":"unlock"}` is refused until Ctrl+Alt+Enter opens the unlock entry, and taken after.
   - To try the unlock authority, as CrowbarOS's desktop runs it, start Fulcrum with `--unlock-socket $D/unlock --unlock-uid $(id -u) --start-locked` as well. It starts locked, the control channel's `unlock` is refused, and `{"op":"unlock"}` on `$D/unlock` is taken after the chord. Lock it there with `{"op":"lock"}`, suspend with a second monitor attached, and resume: that is the row for Milestone 8.

## What to record

A dated record per machine, `docs/hardware-<machine>-<date>.md`. Dated records stay as written, so a second run gets a record of its own. It should say:

- the machine, its GPU and driver, the kernel, and Fulcrum's commit;
- each row above, met or not, with what was seen;
- the start of the log, the `renderer=`, `linux-dmabuf`, and `output connected` lines, and a still-desktop stretch of `frames=` lines;
- what broke, with the log around it.

Attach the whole log under `docs/assets/hardware-<machine>-<date>/`, as the first-light record does.
