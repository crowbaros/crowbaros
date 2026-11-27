# Void Linux lab

The two graphical checks for CrowbarOS: the compositor check and the desktop
check. Both build [Base](../../base/README.md)'s image with `base/void_base.py
build --lab-account` and boot a fresh copy of it under UEFI in an isolated run
directory. Base builds the only Void root and the only disk image; this lab
keeps the checks and the code that drives a guest.

Through the substrate vision this lab also ran
[Milestone 1](../../docs/archive/substrate-vision/milestones/milestone-1.md)
and the Warden's tests; those are at the
[`substrate-final`](https://github.com/crowbaros/crowbaros/tree/substrate-final/lab/void)
tag.

| Path | Contents |
| --- | --- |
| `compositor_check.py` | The check of the [compositor](../../compositor/README.md) on a TTY in Base's image |
| `desktop_check.py` | The desktop check and Base boot checks, on a fresh copy of Base's image, with retained diagnostics |
| `stub_model.py` | Scripted OpenAI-compatible stub Model in Python, installed only in lab images |
| `guest.py` | What both checks share: isolated UEFI VM runs on a copy of Base's disk, QMP input/screendumps and their pixels, VNC monitor hotplug, and SSH |
| `test_*.py` | Unit tests for the guest driver and the checks |

```sh
lab/void/compositor_check.py   # build Base, boot a copy, run every compositor check, record evidence
lab/void/desktop_check.py      # build Base, boot a copy, run Base's boot checks and the desktop checks
```

The scripts use only the Python standard library and need no sudo. Base keeps
downloads in `~/.cache/crowbaros/void` and its roots, Cargo target, and image
in `.lab/base/`. Each check run gets its own directory under
`.lab/void-compositor-checks/` or `.lab/void-desktop-checks/`, outside git,
with its own grown copy of Base's disk, firmware variables, and SSH port. Its
Unix VNC and QMP sockets are in a short directory of its own under the
system's temporary directory, since a socket's path may be no longer than 107
bytes and a worktree's run directory can be. Neither check touches Base's own
`.lab/base/disk.raw`.
Both reach the guest with Base's SSH key, `.lab/base/id_ed25519`. A run that passes
deletes its disk, 12 GB a run; a run that fails keeps it.

Either check takes `--no-build`, for trying a change to the check itself
sooner: it boots the image the last build left and builds nothing. That image must
be one a check built: a person's own `base/void_base.py build` leaves one
without the lab account, which refuses the checks' login. Such a run
says `NOT EVIDENCE` in its log and on its `RESULT` line, and the desktop check
writes `results-not-evidence.json` in place of `results.json`. A pull request needs a
run without it.

The host needs what [Base's README](../../base/README.md#what-the-host-needs)
lists, QEMU's virtio-gpu device included. Avoid running two checks or a Base
build at once: they share Base's build directory, `.lab/base/`, and both
checks overwrite `.lab/base/base.raw` with the lab account. Build Base again
before writing `.lab/base/base.raw` to a real disk.

## The compositor check

From the repository root:

```sh
lab/void/compositor_check.py
```

`compositor_check.py` builds Base's image with `base/void_base.py build
--lab-account` (which builds and tests Fulcrum from `compositor/`,
[ADR 0031](../../docs/adr/0031-the-compositor-is-built-in-this-repository-again.md),
in Base's Void build root), boots a copy under UEFI, stops Base's boot-time
desktop services, and starts Fulcrum directly on virtual terminal 2 with
seatd, debug logging, and no systemd. The evidence log names the commit, and
says when the build had changes that were not committed. It drives the guest
from outside: keys and the pointer through QEMU, and a second monitor plugged
into QEMU's second virtio-gpu connector. Every pull request that changes how
the compositor serves clients runs it.

The compositor has two kinds of client, and the check connects each the way
it allows ([ADR 0032](../../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)):

- The compositor runs as `crowbar-compositor`, the only member of `_seatd`.
- `crowbar-shell` connects to the shell's socket and its control channel.
- The lab account (`crowbar`, UID 1003) runs every app at the apps' socket.

It checks what the shell and an app are offered; that an app is refused
layer shell, session lock, security contexts, the shell's socket, and the
control channel; that only the person's user ID is served at the apps'
socket, `/run/crowbar-apps/wayland-0`, and at Fulcrum's Harness socket; that Void's own `foot` runs as an
app; that a new window takes the keyboard; that switching to the console and
back while a second keyboard is unplugged keeps typing working; that a still
desktop draws no frames; and a monitor plugged in while it runs. The
[slice 1 record](../../docs/research/compositor-slice-1-2026-11-15.md) holds an
earlier run. The run of the
[spike](https://github.com/crowbaros/crowbaros/tree/substrate-final/lab/compositor-spike)
is in the [spike record](../../docs/research/compositor-spike-2026-11-15.md).

The guest uses software rendering. Hardware GPU qualification is separate.

## The desktop check

From the repository root:

```sh
lab/void/desktop_check.py
```

`desktop_check.py` builds Base's image with `base/void_base.py build
--lab-account`, boots a grown copy under UEFI through the firmware and GRUB as
a person's machine would, runs every Base boot check from `base/void_base.py`
(Void, the kernel, the Btrfs root growing to fill the disk, UEFI, runit,
services, microcode, Wi-Fi, SSH, the person's account, the system's log,
reboot/poweroff, the network, package groups, NVIDIA's module and command
line, the compositor's service restarting, and kernel messages) and reports
each in `desktop.log` and `results.json`, exercises the desktop, and powers
the guest off.

The check drives real QEMU keyboard/pointer events for unlock at boot (a
wrong password, then the right one, with nothing typed first), launch, focus,
workspaces, lock by Super+L and by the bar's button, Ctrl+Alt+Enter neither
locking nor unlocking, the suspend hook's lock, a restart of the compositor's
service mid-session that comes back locked behind the lock screen, with an
app's window behind it hearing none of the keys typed, Settings
preview/Save/Undo, and relaunch. It checks saved appearance, and a file the
person wrote in their home, after a clean reboot, service identities, linked
libraries, binary hashes, render activity, and shell restart. It checks that a
still desktop, locked at boot and unlocked with apps open, commits nothing and
draws nothing once it settles, and that the bar draws a workspace that
Super+1 to 9 show within two frames, by the times Fulcrum logs. It unplugs
the only display, by forcing its connector off in sysfs while QEMU raises a
hotplug event, and plugs it in
again: the shell must stop once without panicking, wait for a display, and
draw its layers when it comes back (#271). It summons the Agent's surface with Super+Space and checks where it comes
up, by what Fulcrum and the shell log and by a pixel of the drawer's ground
in a screenshot: on the right of the display, its full height and a third of
its width, with the keyboard; Escape leaving it up; a click on a window
taking the keyboard and leaving it up; the gesture bringing the keyboard
back, then putting the drawer away with the keyboard back in the window that
had it; the drawer over a fullscreen window; its close control; Super alone
still opening the launcher; the gesture doing nothing while locked, with the
drawer up or put away after unlock as it was; and, with a second display
plugged in, the drawer on whichever display the pointer is on. A still
desktop with the drawer up must commit and draw nothing, too. It checks that
the drawer connects to Lever's surface socket with OpenCode configured and no
OpenCode process started while idle, then drives a thread in the drawer against
the local stub Model: a new thread in Ask first asking before the Model's
command runs, with the exact command, the request waiting with the drawer put
away and the desktop locked while the mark shows `waiting`, answered once in
the drawer; a second command answered no, which does not run and which the
Model is told of; Full access and the starting mode chosen from the drawer's
mode menu, where the same command runs with no request;
desktop context reaching the Model without window or
clipboard contents, a command's output and streamed answer in the drawer, work
continuing behind a put-away drawer with the `working` and `done` mark in the
bar and over a fullscreen window, Stop ending a turn and its spawned commands,
a failed turn showing why, a message sent from the drawer while a turn works
steered into that turn by the pinned OpenCode and answered by the Model in
the same turn, after the command it was held on, a shell restart redrawing the
thread from Lever, a
command the person's own OpenCode settings refuse refused in Ask first with no
request to allow it, and the person's own OpenCode settings left unchanged and
free of Lever's additions in a session they start by hand.
The recovery checks restart the shell and Fulcrum during real OpenCode work,
send a surface message while Fulcrum is unavailable and verify it has no desktop
context, and kill Lever and OpenCode separately while a request waits. They
verify interrupted turns, withdrawn requests, readable records, no automatic
retry, and the next message receiving earlier act output from the loaded
OpenCode session. Before and after captures show each recovery in the drawer.
It checks that apps run as the person at the ordinary socket for apps, that
copy and paste, the primary selection, and drag and drop work between two apps,
that Fulcrum's Harness socket serves the person alone, and that the shell's
sockets, its control channel, and the unlock helper refuse an app. An error
returns nonzero and retains `desktop.log`, `results.json`, the build log,
service/kernel diagnostics, screenshots, and the failed disk in that run's
directory. The desktop's accounts and services are described in
[Base's README](../../base/README.md#the-desktops-accounts).
