# Compositor spike: a Smithay compositor runs the shell and enforces ADR 0014

**Result (November 15, 2026): nothing that was run gives a reason to reopen
[ADR 0015](../adr/0015-our-own-compositor-on-smithay.md).** A compositor started
from Smithay's `smallvil` example runs the GPUI shell and Studio Settings,
keeps layer-shell for the shell alone, withholds a key chord from every client,
and runs on a TTY in a Void guest with seatd and no systemd.

**Three things were not run**, and the record does not cover them: the
compositor on an NVIDIA card through DRM (it ran on NVIDIA only nested in a
window), Void's `nvidia` package, and suspend and resume. See
[NVIDIA](#nvidia) and [Not run](#not-run).

This is the record that [issue 3](https://github.com/crowbaros/crowbaros/issues/3)
asks for. The spike's code is in [`compositor/`](../../compositor/README.md) and
is throwaway. The evidence is in
[`evidence/compositor-spike-2026-11-15/`](evidence/compositor-spike-2026-11-15/):
three logs, copied byte for byte from the logs the runs wrote, and four
captures.

## Summary

| Item in the issue | Result | Evidence |
| --- | --- | --- |
| Runs nested in a window on the development host | Shown | `nested-protocols.log`, `nested-apps.log` |
| Bar, dock, launcher, and wallpaper as layer surfaces, with correct exclusive zones | Bar, dock, and wallpaper shown with the real shell. Launcher and the exclusive zones shown by tests only | Capture below, `tests/layers.rs` |
| Studio Settings opens as a floating window through GPUI's Vulkan path | Shown | Capture below, `nested-apps.log` |
| The two Hyprland socket uses replaced by an interface we define | Replaced and tested on both sides. The window list was seen working in the real shell. The two focus calls were not clicked in the real shell | [The control channel](#the-control-channel) |
| A key chord is intercepted; a focused client sees neither press nor release | Shown, with real key events in the Void guest | `void-tty.log`, `tests/confirm.rs` |
| Layer-shell offered to the shell and not to a second client | Shown | `nested-protocols.log` |
| Input, clipboard-manager, and capture globals absent from `wayland-info` | Shown | `nested-protocols.log`, `void-tty.log` |
| A security-context client sees the reduced set | Shown | `nested-protocols.log`, `void-tty.log` |
| `backend_libei` off in the build | Shown | [Build](#the-build) |
| The same binary on a TTY in a Void guest: udev, libinput, libseat on seatd, DRM, no systemd | Shown | `void-tty.log` |
| Two outputs with different scales, and one hotplug | Shown | `void-tty.log`, captures below |
| NVIDIA with Void's `nvidia` package | **Not run.** A nested run on NVIDIA showed no first-frame freeze | [NVIDIA](#nvidia) |
| Lines written, Smithay commit, hours | Recorded | [Cost](#cost) |

## What was run on

- **Host:** Arch-based (Omarchy), Linux 7.2.15, Hyprland 0.57.2, one GPU: an
  NVIDIA GeForce RTX 4090 on driver 615.78.08 with the open kernel modules.
  Rust 1.99.1.
- **Guest:** Void Linux (glibc), Linux 6.18.63, runit, seatd 0.9.3, Mesa
  26.2.7, in QEMU 11.1.3 with KVM. Built by
  [`lab/void/compositor_guest.py`](../../lab/void/compositor_guest.py).
- **Smithay:** commit `fd8e3f50f9556aa6c0485989d1d49ff2c5eefae1` of `master`
  (14 November 2026).
- **Apps:** `apps/shell` and `apps/studio-settings`, built on the host from
  their pinned lockfiles (GPUI 0.2.2, blade).
- **`wayland-info`:** 1.3.0, from Arch's `wayland-utils` on the host and Void's
  `wayland-utils` in the guest.

**One binary did every run on a display.** It was built inside a Void build
root, so it links Void's libraries. Its SHA-256 is
`6bce787cc7a19e73544a1cf6ce037fd9a01580aab3606aecab23b67905d7431d`, and all
three logs name it. It ran unchanged on the host (nested) and in the guest (on
a TTY).

## It runs our apps

![The shell and Studio Settings under the nested compositor](evidence/compositor-spike-2026-11-15/nested-shell-and-studio.jpg)

*The compositor's own window on the host, captured by the host. The bar, the
dock, and the wallpaper are the shell's layer surfaces. Studio Settings is an
ordinary window.*

**Run:** `compositor/checks/nested-apps.sh` starts the compositor in a window
on the host's Hyprland session, with the shell as the shell and Studio Settings
as an ordinary client. It runs for a minute.

**Seen:**

- The compositor starts on the host's NVIDIA EGL and offers linux-dmabuf.
- The shell connects through the socket the compositor hands it. Its three
  layer surfaces appear: `crowbaros-wallpaper` (background), `crowbaros-bar`
  (top), `crowbaros-dock` (top).
- Studio Settings opens as a window and draws. Its log names the adapter
  `NVIDIA GeForce RTX 4090`, so it rendered through Vulkan on the GPU.
- When the host resized the compositor's window from 1896 to 941 pixels wide
  and back, the bar and the dock were laid out again for the new width
  ([capture](evidence/compositor-spike-2026-11-15/nested-resized.jpg)).
- The shell needed nothing beyond the globals in
  [the table below](#which-client-sees-which-global). It ran without
  xdg-decoration, cursor-shape, xdg-activation, and text-input.

**Exclusive zones.** `tests/layers.rs` maps a 44-pixel bar and a 96-pixel dock
(the shell's sizes) on a 1280 by 800 output. The bar is configured at 1280 by
44, and a maximized window is configured at 1280 by 660. In the capture above
Studio Settings sits under the bar by accident of timing: it connected before
the shell's layers existed, so it was placed against the full output. The
spike does not move windows when the usable area changes.

**The launcher** was not opened with the real shell, because the host's
keyboard and pointer cannot be driven from a script without touching the
person's session. `tests/layers.rs` covers its Wayland behaviour: an overlay
layer surface that asks for the keyboard exclusively gets every key, the
window under it gets none, and the window gets the keyboard back when the
layer surface closes.

### The control channel

The issue asks for the apps' two uses of the Hyprland socket (focus a
workspace, focus a window) to be replaced by an interface we define, and for
the choice to be written down.

**The choice: a socket pair the compositor makes and hands to the shell.** The
compositor starts the shell, and passes it one end in the
`CROWBAR_SHELL_CONTROL_FD` environment variable. Nothing is bound in the
filesystem, so no other process can open it. The wire format is one JSON
object per line. The set of requests is fixed:

```text
-> {"op":"snapshot"}
<- {"reply":"snapshot","workspace":1,"active":"0x2","windows":[{"id":"0x1","app_id":"…","title":"…","workspace":1}]}
-> {"op":"focus_workspace","id":2}
<- {"reply":"ok"}
-> {"op":"focus_window","id":"0x1"}
<- {"reply":"ok"}
<- {"event":"confirm"}
```

Anything else gets `{"reply":"error",…}`, including a known request with an
unknown field. There is no request that runs a command, evaluates code, or
loads anything. The compositor names windows itself (`0x1`, `0x2`, …); a client
never chooses its id.

**Why not a private Wayland protocol.** A private global, filtered to the shell
like layer-shell is, would need no second socket. GPUI 0.2.2 owns the shell's
Wayland connection and gives the app no way to bind a global of its own, so
the shell would need a second connection, and that connection would have to be
handed over the same way. That is the same plumbing with code generation on
top. Revisit this when the shell moves to a GPUI that exposes the connection,
or when the real compositor is started. The final interface is a decision that
is hard to reverse, so it wants its own ADR then.

**What it replaced.** `apps/shell/src/session.rs` had three kinds of call to
the Hyprland socket, one more than the issue counts: the two focus calls, and
three queries (`j/activeworkspace`, `j/clients`, `j/activewindow`) that fill the
bar. All of them now go to the control channel. The shell makes the inherited
descriptor close-on-exec before it starts anything, so a program the shell
launches does not inherit the channel.

**Tested:** `compositor/tests/control.rs` drives the compositor's end with the
literal lines above. `apps/shell/src/session.rs` has tests that drive the
shell's end against a scripted peer.

**Seen in the real shell:** the bar shows workspace 1 as active and "Studio
Settings" as the active window (capture above). Both come from `snapshot`.

**Not seen in the real shell:** a click on a workspace number or a dock entry.
Those calls are covered by the tests on both sides only.

**Left on Hyprland, and not counted by the issue:**

- `apps/studio-settings/src/session.rs` runs `hyprctl` to read and set
  Hyprland's animations for "Reduced motion". It is unchanged. Studio Settings
  is not the shell, so by ADR 0015 it cannot reach the control channel, and
  the compositor draws no animations to turn off. Where that setting lives is
  a design question, not a wiring one.
- The shell starts programs with `systemd-run` and `gio launch`, and its
  `--start-session` uses `systemctl`. Void has no systemd. None of this was
  touched.

**A consequence to decide before merging.** With this change the shell no
longer works under Hyprland: in the Fedora lab its bar shows "Connecting to the
compositor…" and the workspace and focus buttons do nothing. ADR 0015 says the
Hyprland wiring does not carry over, and the issue says the spike need not
merge. If the Fedora lab must keep a working shell for interface work, leave
`apps/shell` out of what merges.

## It enforces ADR 0014

### How the compositor tells the shell apart

**By how the connection was made, never by anything the client says.**

| Kind | How the connection is made |
| --- | --- |
| Shell | The compositor makes a socket pair, registers one end as the shell, and passes the other to the shell process it starts, in `WAYLAND_SOCKET` |
| Ordinary | The client connects to the public socket in `XDG_RUNTIME_DIR` |
| Security-context | The client connects to a listener that an ordinary client registered through security-context |

There is no lookup of a process id, an executable path, or an app id, so there
is nothing for a client to forge and no race to win. `WAYLAND_SOCKET` is the
standard variable, and GPUI's Wayland client reads it, removes it from the
environment, and marks the descriptor close-on-exec. The programs the shell
starts therefore arrive on the public socket as ordinary clients.

"Kind" is the spike's word for how a client reached the compositor. It is not
the glossary's Principal. Mapping a surface to the principal that owns it is
not built.

### Which client sees which global

**One table decides it, and it denies by default.**
[`compositor/src/policy.rs`](../../compositor/src/policy.rs) lists the globals
each kind may see. Every global the compositor creates passes through that
table, including the ones whose Smithay constructor takes no filter. A global
that is in no list is invisible to every client, the shell included.

That works because Smithay now routes every global through one pair of traits
(`Dispatch2`, `GlobalDispatch2`). The spike writes by hand the two `impl`
blocks that `smithay::delegate_dispatch2!` would generate, and adds one check
to `can_view`. This is cheaper and stricter than passing the same closure to
each constructor, which is what the research describes niri doing.

**Run:** `compositor/checks/nested.sh` asks `wayland-info` three times: as the
shell (run in the shell's place, so it gets the shell's socket), as an ordinary
client, and through `crowbar-probe security-context`, which registers a
security context and runs `wayland-info` inside it.

**Seen** (`nested-protocols.log`):

| Global | Shell | Ordinary | Security-context |
| --- | --- | --- | --- |
| `wl_compositor`, `wl_subcompositor`, `wl_shm`, `wl_seat`, `wl_output`, `xdg_wm_base`, `zxdg_output_manager_v1`, `zwp_linux_dmabuf_v1`, `wp_viewporter`, `wp_fractional_scale_manager_v1` | yes | yes | yes |
| `wl_data_device_manager` (the clipboard between windows) | yes | yes | no |
| `wp_security_context_manager_v1` | yes | yes | no |
| `zwlr_layer_shell_v1` | yes | no | no |
| Total | 13 | 12 | 10 |

For comparison, the host's Hyprland offers 73 globals to every client.

**Absent for all three kinds:** `zwp_virtual_keyboard_manager_v1`,
`zwlr_virtual_pointer_manager_v1`, `zwp_input_method_manager_v2`,
`zwp_input_method_v1`, `zwlr_data_control_manager_v1`,
`ext_data_control_manager_v1`, `ext_image_copy_capture_manager_v1`,
`ext_output_image_capture_source_manager_v1`,
`ext_foreign_toplevel_image_capture_source_manager_v1`,
`zwlr_screencopy_manager_v1`, and `zwlr_export_dmabuf_manager_v1`. The
compositor does not construct them.

**A hidden global cannot be bound by guessing.** An ordinary client that asks
for a layer surface is told `zwlr_layer_shell_v1 is not offered to this
client`. `tests/globals.rs` goes further: an ordinary client that binds
layer-shell by its number, without it being advertised, is disconnected with a
protocol error. In the shell's place the same probe gets its layer surface.

**Open choices in this table.** Whether an ordinary client should be able to
register a security context at all, or only the broker. Whether a
security-context client gets no clipboard, as here. Whether the shell itself
should lose the clipboard global. None of these was a question for the spike.

### The confirm chord

The spike's stand-in for the confirm gesture is **Ctrl+Alt+Enter**. Which key
the real gesture uses is not decided here.

**Run, in the Void guest:** QEMU presses and releases keys on the guest's
virtio keyboard. They pass through the guest kernel, libinput, and the
compositor to a test client that has a window and the keyboard focus and
prints every keyboard event it is sent.

**Seen** (`void-tty.log`): the client printed `key 30 pressed` and `key 30
released` for the letter A. For the chord it printed the press and release of
Ctrl (29) and Alt (56), which shows it held the keyboard throughout, and
nothing for Enter (28). The compositor logged `confirm chord taken by the
compositor`.

**Tested** (`tests/confirm.rs`, five tests through the one function both
backends call): a plain key arrives; the chord's key does not; Enter without
the modifiers is an ordinary key; a client that gains the focus while the
chord is held is not told that Enter is down. The shell is told on the control
channel (`{"event":"confirm"}`), and no client is.

**A leak the tests found.** Smithay forwards a key release unless the filter
intercepts the release too, whatever happened to the press. With the obvious
filter, releasing Ctrl or Alt before Enter sent the focused client a release
for Enter with no press. The compositor now remembers each key whose press it
withheld and withholds that key's release. This is the kind of hole ADR 0014
says must be closed, and it shows the filter needs adversarial tests, as ADR
0015 says.

**What the client still learns.** It sees Ctrl and Alt go down and up, so it
can tell that a chord may have been made. A gesture on a single dedicated key
would tell it nothing.

### The build

`compositor/Cargo.toml` sets `default-features = false` on Smithay and names
the features it wants. `backend_libei` is not among them, and neither are
`xwayland`, `backend_x11`, and `backend_vulkan`.

At this commit Smithay still depends on the `reis` crate (the libei protocol)
without a feature gate, so Cargo compiles it. The backend module that uses it
is compiled out, and the release binary contains no `reis` symbol and no
libei string.

## It runs on Base

**Run:** `lab/void/compositor_guest.py check`. It builds the compositor inside
a Void build root, makes a guest disk, boots it, and starts the compositor on
virtual terminal 2 as the ordinary user `crowbar` (in the groups `_seatd`,
`video`, and `input`). It passed **19 of 19 checks** (`void-tty.log`).

**Seen:**

- PID 1 is `runit`. There is no `/run/systemd` and no systemd or elogind
  process. `seatd` runs as a runit service.
- The compositor opens its seat through libseat with `LIBSEAT_BACKEND=seatd`,
  finds the GPU through udev, opens `/dev/dri/card0` (driver `virtio_gpu`),
  and brings up `Virtual-1` at 1280 by 800.
- Keys reach it through libinput (the chord check above).
- `wayland-info` inside the guest shows the same globals as on the host.

**Two outputs, two scales, one hotplug.** The guest's GPU has two connectors
and starts with one monitor. The lab plugs a second monitor in through QEMU.
The compositor logged `udev says the DRM device changed; scanning connectors`
and then `output connected name=Virtual-2 width=1024 height=768 scale=2.0`.
`wayland-info` then listed `Virtual-1` at scale 1 and `Virtual-2` at scale 2,
at x = 1280. A window opened on `Virtual-2` was sent
`preferred_buffer_scale 2`; the window on `Virtual-1` was sent no scale, which
means 1.

| `Virtual-1`, scale 1 | `Virtual-2`, scale 2 |
| --- | --- |
| ![First output](evidence/compositor-spike-2026-11-15/void-output-1.png) | ![Second output](evidence/compositor-spike-2026-11-15/void-output-2.png) |

*QEMU's own captures of each connector. Both test windows are 480 by 320
logical pixels. The white square is the compositor's pointer.*

Unplugging was not tried. The scales come from the command line
(`--scale Virtual-2=2`); nothing reads a monitor's size to choose one.

### Void packages it needs

To run, from what the binary links and what it loads:

| Needed for | Void packages |
| --- | --- |
| Linked libraries | `libseat`, `libinput` (which brings `libevdev`, `mtdev`, `libwacom`, `libgudev`, `glib`, `lua54`), `eudev-libudev`, `libxkbcommon`, `libgbm`, `libdrm`, `libelogind`, `libcap`, `expat`, `libffi`, `libpcre2`, `libgcc`, `glibc` |
| Loaded at run time | `libglvnd` and `mesa-dri` (EGL and the DRI driver), `xkeyboard-config` (keymaps) |
| The seat | `seatd` |
| The checks only | `wayland-utils`, `kbd` (for `openvt`) |

To build: `rust`, `cargo`, `gcc`, `pkg-config`, `libseat-devel`,
`libinput-devel`, `eudev-libudev-devel`, `libxkbcommon-devel`, `libgbm-devel`,
`libdrm-devel`, `wayland-devel`. Both lists are in
[`lab/void/config.json`](../../lab/void/config.json).

**`libelogind` is linked, and no elogind runs.** Void builds libseat with both
a seatd and an elogind backend, so the library comes along. To keep it out of
Base, Base would build its own libseat.

### What Base has to do that a systemd desktop does for free

- **Make `XDG_RUNTIME_DIR`.** Nothing creates `/run/user/1000` without logind.
  The lab makes it by hand.
- **Put the person in `_seatd`.** That group is what lets a process open the
  seat. Anything in that group can ask seatd for the input devices, so who is
  in it matters to ADR 0014's "creating or reading input devices".

## NVIDIA

**Not run: the item as the issue states it.** The development host has one
GPU, the RTX 4090, and the host's own session is using it. Giving it to a Void
guest would take the host's display away. So there was no run on Void's
`nvidia` package, no run of the compositor on NVIDIA through DRM, and no
suspend and resume. Suspending this workstation was also not acceptable while
other work ran on it.

**Run instead: the GPUI 0.2.2 apps under the compositor, nested, on this
NVIDIA card** (`compositor/checks/nested-apps.sh`). The known failure is that
the first frame draws and then nothing updates, with the app's log full of
`Acquire failed because the surface is out of date`. A client that has stopped
drawing stops committing, so the compositor counted commits per surface every
five seconds for a minute.

**Seen** (`nested-apps.log`): the bar, the dock, the wallpaper, and Studio
Settings each committed about 60 times a second in all twelve intervals, with
none stalled. The apps logged no `out of date` and no `Acquire failed`. A
resize in the middle did not change that.

**What this does and does not say.** The freeze did not happen here: blade on
NVIDIA 615.78.08, under this compositor, nested, with the open kernel modules
on Arch. The reports were under niri and anvil running on DRM, and this run
was not on DRM. It lowers the risk and does not close the question. It needs a
second machine, or this one with the host session stopped.

## Cost

**Smithay commit:** `fd8e3f50f9556aa6c0485989d1d49ff2c5eefae1`, pinned in
`compositor/Cargo.toml` for both `smithay` and `smithay-drm-extras`.

**Lines.** Counted with `wc -l`, comments and blank lines included.

| Part | Lines |
| --- | --- |
| The compositor (`src/`, without the probe) | 2,894 |
| … of which unchanged from `smallvil` | 1,143 |
| … of which the TTY backend, adapted from `anvil` | 412 |
| … so new or changed, outside the TTY backend | about 1,340 |
| The probe client | 668 |
| Tests | 617 |
| Check scripts and the Void lab's graphical guest | 801 |
| The shell's adapter (`session.rs`) | 242 added, 36 removed |

**Hours: about HOURS_PLACEHOLDER of wall-clock time**, in one session by one coding
agent (the model and harness are named at the end), with Jim answering two
questions. That covers reading the issue, the ADRs, and the research, all the
code and scripts above, every run, and this record.

**How far that number goes.** Not far. It measures a spike by an agent, and it
leaves out everything that made niri take one developer five and a half months
to a first release: window management a person can live in, a real cursor,
popups and drag and drop done properly, output configuration, session lock,
screen capture for the shell, text input, damage and frame timing done well,
and the long tail of hardware. What the spike does show about cost is narrower
and still useful: the three ADR 0014 rules it tested cost little once a
Smithay compositor exists (about 70 lines for the table, about 40 for the
chord, about 210 for the control channel), and the Void TTY path needed no
workaround beyond the two in [Findings](#findings-to-carry-forward). Whether
the full compositor fits two people beside Milestones 2 to 4 is a judgment for
Jim and Jason. This record does not settle it.

## Against what reopens the decision

| Condition in ADR 0015 | Reading |
| --- | --- |
| The shell's layer surfaces or GPUI's rendering cannot be made to work | Not met. Both work, nested, on NVIDIA |
| A client can still receive the confirm key, or still see a protocol it should not | Not met in what was tested. One such leak was found and closed |
| The compositor cannot run on a Void guest without systemd | Not met. It runs |
| The measured cost shows the full compositor is beyond two people | Not answered by this measurement. See [Cost](#cost) |

## Findings to carry forward

1. **Withhold the release of every key whose press was withheld.** Smithay
   does not do this for you. See [the confirm chord](#the-confirm-chord).
2. **Filter every global in one place.** Hand-writing the two dispatch `impl`
   blocks gives a default-deny table. A new global is hidden until someone
   lists it.
3. **On a software renderer, wait for the drawing before the page flip.** The
   guest has no GPU acceleration, so Mesa renders in software and attaches no
   fence to its buffers. Without a wait on the frame's sync point, the screen
   showed a half-drawn window that stayed until something else changed.
   `anvil` hides this wait behind a feature that is off by default.
4. **Offer `wl_compositor` version 6.** Smithay's default constructor offers
   version 5, and below version 6 the compositor cannot send a surface the
   scale it should draw at.
5. **Identify the shell by a socket the compositor hands over.** It worked
   with GPUI 0.2.2 unchanged.
6. **The lab needs QEMU's `-vga none`.** QEMU's default VGA card is a second
   DRM device and the kernel marks it as the primary one, so the compositor
   opened it and never saw the virtio-gpu hotplug. On real hardware the same
   question is which GPU the compositor picks.
7. **Smithay's `Space` draws layer surfaces and arranges exclusive zones.**
   The layer-shell handler in the spike is about 110 lines.

## Not run

- The compositor on NVIDIA through DRM, Void's `nvidia` package, and suspend
  and resume. See [NVIDIA](#nvidia).
- The GPUI apps in the Void guest. The guest has no Vulkan driver installed and
  the apps were built on the host. The Fedora lab is therefore still needed.
- Pointer clicks and typing in the real shell and in Studio Settings. The
  host's input was not driven. In the guest the pointer was moved, not clicked.
- The launcher, with the real shell.
- A program launched from the shell.
- Unplugging a monitor, changing a mode, and a VT switch away and back. The
  code for a VT switch exists and was not exercised.
- Any real hardware. The TTY runs were in a VM on a virtual GPU.
- CI. `compositor/` is outside the root workspace and CI does not build it.

## Known defects in the spike

- The pointer is a white square. Clients' cursor images are not drawn.
- QEMU's capture of the first output still shows a pointer square in the
  corner after the pointer moves to the second output.
- A window is not moved when a layer surface changes the usable area.
- Windows have no server-side decoration, and there is no key to move, resize,
  or close one. A client that draws its own title bar can be moved by it.
- Popups are placed but not grabbed.
- On a TTY the compositor redraws every 16 ms when nothing has changed.
- `Unable to become drm master, assuming unprivileged mode` is logged at start
  in the guest. Output works all the same. `anvil` logs the same line.

## How to repeat

```sh
# Tests that need no display.
cd compositor && cargo test
cd ../apps/shell && python3 packaging/prepare-gpui.py && cargo test --locked

# The Void guest. Needs QEMU with virtio-gpu; see lab/void/README.md.
lab/void/compositor_guest.py check

# Nested on a Wayland session, with the binary the Void build made.
export CROWBAR_BIN=$PWD/.lab/void-graphical/target/release
compositor/checks/nested.sh /usr/bin/wayland-info
compositor/checks/nested-apps.sh /path/to/crowbaros-shell /path/to/crowbaros-studio-settings
```

On the host used here, two things were not installed and were not installed
for the run: `wayland-info` and QEMU's virtio-gpu modules. Both were taken
from the distribution's signed packages, unpacked into a cache directory, and
used from there (`QEMU_MODULE_DIR` for QEMU).

---

Work done by Claude Fable 5.2 in the Claude Code harness (through T3 Code).
