# Fulcrum

Fulcrum is the CrowbarOS Wayland compositor, written from the ground up in
Rust. It is part of CrowbarOS Base and part of what CrowbarOS trusts, and lives in
CrowbarOS's `compositor/` directory. It builds
one program, `crowbar-compositor`.

[The plan](docs/PLAN.md) says how it is built. Fulcrum writes its own wire
protocol, event loop, display, input, rendering, and window-management code,
and builds on no other compositor's crates
([CrowbarOS ADR 0028](../docs/adr/0028-the-compositor-is-written-from-the-ground-up.md)).

CrowbarOS decides what the compositor must do, in its decision records:

| Decision | Record |
| --- | --- |
| CrowbarOS owns its compositor | [ADR 0015](../docs/adr/0015-our-own-compositor-on-smithay.md) |
| Its two kinds of client, the shell and the person's apps, how each connects, and what each is offered | [ADR 0032](../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md) |
| It is built in this repository, and reaches Base when a change merges | [ADR 0031](../docs/adr/0031-the-compositor-is-built-in-this-repository-again.md), which replaces [ADR 0027](../docs/adr/0027-the-compositor-is-built-in-its-own-repository.md) |
| It is written from the ground up | [ADR 0028](../docs/adr/0028-the-compositor-is-written-from-the-ground-up.md) |

What Fulcrum does as a desktop, its frames, workspaces, chords, and
placement, is what this README says, and changes by pull request
(ADR 0032). The substrate vision's records in
[the archive](../docs/archive/substrate-vision/adr/) bind nothing; where this
README names one, as ADR 0020, it says where an idea came from.

## Where it stands

Milestones 1 and 2 are done: the Wayland core
([record](docs/wayland-core-2026-11-17.md)) and first light
([record](docs/first-light-2026-11-17.md)). Fulcrum shows windows on a virtual
terminal, and Void's own `foot` and `wayland-info` run on it. What is built:

- **`fulcrum-wire`**, the wire protocol: the codec, connections that pass
  descriptors beside the bytes, and Rust code generated from the protocol XML.
  Every queue is bounded, and nothing a peer sends can make it panic.
- **The server core**: an event loop on `epoll`; two path sockets, the
  shell's and the one every app connects to, each taking one user ID as the
  kernel reports it; and the registry, which advertises and binds a global
  only if the policy offers it (ADR 0032). Each client is offered every
  global the compositor has, less a short list: an app is refused layer
  shell, session lock, and security contexts, and the shell security
  contexts. A client that breaks the protocol gets
  `wl_display.error` and is disconnected, and everyone else is served.
- **Surfaces and shared memory**, with double-buffered state, held commits for
  synchronized subsurfaces, `wp_viewporter`'s crop and scale, and pools read
  with positioned file I/O. If a file ends mid-buffer, the client gets a
  protocol error.
- **xdg-shell windows and popups**: the configure handshake, window geometry,
  positioners, `wl_surface.enter` and `leave`, and the protocol's errors. A
  new window opens inside the usable area of the output the pointer is on,
  offset from the last, opens above, and takes the keyboard, as on any
  desktop. A dialog takes it only if its parent has it.
- **Windows maximize, go fullscreen, and snap to a half** of their output, at
  a client's request or by Super+Up, Super+F, and Super+Left or Right, and go
  back to where and as they floated (ADR 0020). A maximized or snapped window
  fills the room the shell's layers leave and fits it again when it changes.
  A fullscreen window covers the bar and the dock, and nothing else the shell
  draws.
- **Windows move and resize with the pointer**: when a client asks during a
  press on its window, or when the person holds Super and drags with the
  left button to move or the right button to resize from the nearest corner.
  No client hears the pointer while it drives a window. A resize keeps within
  the size the client can take, and the edge it did not grab stays put.
- **A frame around every window**, which no client can draw or cover: a thin
  line whose colour shows which window has the keyboard. A fullscreen window
  keeps its frame inside the output's edge. Its look is CrowbarOS issue 22's
  to decide; until then, plain colours.
- **Nine workspaces**, each spanning every output, one showing at a time
  (ADR 0020). A window on a hidden workspace is nowhere: not drawn, given no
  input, on no output, and without the keyboard. Every chord with Super
  belongs to the compositor and never reaches a client: Super+1 to 9 show a
  workspace, and Super+Shift+1 to 9 move the window with the keyboard there.
  Super tapped alone sends the shell a `launcher` event on its control
  channel. ADR 0032 accepts the launcher event, the `launching` request,
  and the snapshot's `attention`.
- **The shell's control channel**: a path socket of its own, checked against
  the shell's user ID, where the shell asks for a snapshot of the windows and
  shows a workspace or a window. One JSON object per line, as the spike
  spoke and the shell already speaks. The compositor sends the snapshot as
  an event whenever it changes, so the bar shows a workspace as it shows.
- **Redraw on change**: an output is drawn only when something it shows
  changed, then waits for the display, so a still desktop draws nothing and a
  busy client is held to the display's rate. Frame callbacks are answered
  after each draw.
- **Layer shell** for the shell's bar, dock, launcher, and wallpaper: layers
  placed by anchors and margins, exclusive zones that make the usable area
  where windows open, background and bottom layers below the windows and top
  and overlay layers above, popups of layers, and a layer closed when its
  output goes.
- **The pointer's image**: a client's cursor surface, or a shape it names
  with cursor shape, set while the pointer is on one of its surfaces, or
  else the compositor's own. A shape, and the compositor's own pointer, are
  the system XCursor theme's images (`XCURSOR_THEME`, `XCURSOR_SIZE`, and
  `XCURSOR_PATH`), or a white square where there is no theme. It is drawn
  above everything at its hotspot, and a client's surface is clipped so that
  it covers no frame (ADR 0020). On the TTY, the compositor's own image goes
  on the display's cursor plane where it has one, so moving the pointer
  redraws no frame.
- **The seat**: one keyboard and one pointer. The keymap is compiled by
  libxkbcommon and shared through a sealed memfd. The topmost mapped top or
  overlay layer that requests exclusive interactivity takes the keyboard
  until it goes away or releases it. This lets the person type into the
  launcher as soon as it opens. The keyboard then returns to the desktop's
  current focus. On-demand layers take the keyboard on a click. A click on a
  window raises and activates it. A new window takes the keyboard, and a
  dialog does where its parent has it. A new window that does not get it,
  as one that maps while the session is locked, wants attention, which the
  shell's snapshot shows.
  The shell's `launching` request is still answered, and changes nothing.
  The pointer follows what
  is under it, by input region, and stays with its surface while a button is
  held.
- **linux-dmabuf**, version 4: feedback names the GPU's device and a sealed
  table of what the compositor takes. Where the GLES renderer draws, the
  table is every format and layout EGL imports, tiled ones included. The GPU
  imports such a buffer when it is made and draws from it where it is, with
  no copy, and the compositor holds it until other content replaces it. Only
  a real dma-buf reaches the import. Elsewhere, or where the GPU refuses a
  buffer, the CPU copies single-plane `XR24` and `AR24` at each commit.
  This path maps real dma-bufs and uses positioned reads for regular files.
  The shell and every app are offered it (ADR 0032).
- **Presentation time**: a client learns when its content was shown, on
  which output, and the refresh, so it can pace itself. A page flip's time
  is the kernel's, on the monotonic clock, with the display's vertical blank
  count. Content replaced before it was drawn is discarded.
- **Output scale** from 1 to 3 in steps of a quarter, for each output
  (ADR 0020). The desktop is in logical units: an output covers its pixels
  over its scale, outputs meet at their logical edges, and what an output
  shows, frames included, is drawn at its pixels. A client hears the scale
  rounded up as `wl_output.scale`.
- **The layout of displays** (ADR 0020): each monitor's mode, place, and
  scale, kept by its identity from its EDID in the file `--layout` names,
  and set only through the shell's control channel (`outputs` and
  `configure_output`). A monitor the layout does not name takes its
  preferred mode, the place to the right of the others, and a scale picked
  from its density: its pixels over the physical size its EDID reports. A
  scale of 1 suits about 110 pixels an inch on a monitor, and about 135 on
  a panel under 20 inches, which is looked at from closer. The scale is the
  density over that, to the nearest quarter, from 1 to 3, so a 27-inch 4K
  monitor starts at 1.5 and a 1080p one at 1. A display that reports no
  size, or one that cannot be its own, starts at 1. The picked scale is not
  kept: only a scale the person sets is, and it wins.
  ADR 0032 accepts the wire format.
- **Fractional scale**: a surface hears the scale it should draw at, in
  120ths: the largest scale of the outputs it is on, again whenever that
  changes.
- **Lock** (ADR 0032): the shell's `lock` request,
  Super+L, or `--lock-after` seconds without input lock the session, unless
  a shown surface inhibits idle. Locked, the shell's lock screen (its layer
  surface of namespace `crowbaros-lock`) is all that is drawn, every other
  display is blank, and input goes to it alone, at once: the person types
  their password with nothing first. An unlock authority ends it: a part of
  Base at `--unlock-socket`, checked against `--unlock-uid`, that checks the
  person's password and says `unlock`. With one, the shell's `unlock` is
  refused, and `--start-locked` starts the session locked; without one, the
  shell's `unlock` ends it. A `lock` is answered once every display shows
  the session locked, or after a second where one shows nothing, so the
  suspend hook, which waits for the answer, never sleeps on the desktop. The
  confirm chord, Ctrl+Alt+Enter, waits for the vision's Milestone 3, the
  Agent at hand, to decide what it confirms. Until then it does nothing, and
  no client hears its Enter.
- **Idle inhibit**: a surface that is shown, drawn on an output on the
  workspace the person is on, keeps that output from going idle. One that is
  hidden or gone does not. The idle timer that locks the session
  (`--lock-after`) honours it.
- **Explicit sync**: a commit with a dma-buf waits, without blocking the
  compositor, until its acquire point on the client's timeline is signalled,
  and the compositor signals its release point when it is done with the
  buffer: at once for a copy, and when other content replaces it for a
  buffer the GPU draws where it is. Drivers without implicit sync, as NVIDIA's, need it. It is offered
  where the display's device has timeline syncobjs.
- **The data device**: copy and paste work between the shell and every app,
  as on any desktop (ADR 0032). There is one selection, and the client that
  holds the keyboard is offered it, as it gains the keyboard and when the
  selection changes. The primary selection, which a middle click pastes,
  follows the same rules. A drag drops on any client. A drag starts from a held pointer press, negotiates an action
  with its destination, and ends if the session locks or its source goes
  away. Any drag icon follows the pointer below trusted frames.
- **Limits on each client** (ADR 0032): the apps cannot take the
  descriptors the shell needs, since a reserve is kept for it, and one
  client cannot make the compositor hold pools or pixels without end.
- **The TTY backend**: the seat from seatd, through Fulcrum's own client of
  its socket; displays through the kernel's atomic mode setting; input
  through libinput; and monitors and input devices plugged in and out, heard
  from the kernel's netlink uevents. Ctrl+Alt+F1 to F12 switch virtual
  terminals. The kernel never sees Ctrl+Alt+Del, so with
  `--ctrl-alt-del-socket` the compositor says `ctrl-alt-del` at that socket,
  even locked, and Base reboots.
- **The GLES renderer**: where Mesa's EGL and GBM load, each monitor's frames
  are drawn by the GPU into GBM buffers the display scans out. Where they do
  not, or `FULCRUM_RENDERER=cpu` says so, the CPU renderer draws into dumb
  buffers. Both draw the same scene: what each output shows, back to front.
  The GPU draws only what changed: each frame's scene beside the last one
  gives its damage, and a buffer is brought up to date with the damage of
  every frame since it was last drawn. EGL, GLES, and GBM are loaded at run
  time, so building Fulcrum needs none of them.
- **The nested backend**, for development: the desktop in a window on a host
  session. Only in a build with `--features nested`, which Base never is.
- **A headless backend** that draws each output with the CPU renderer, and
  `crowbar-probe`, the test client, on `fulcrum-wire`'s client side.

The later milestones, as [the plan](docs/PLAN.md) sets them. No hardware
record has been made yet:

| Milestone | Where it stands |
| --- | --- |
| 3 · Buffers and timing | Built. The runs on the test machine's RTX 5060 and on a Mesa machine are next ([how](docs/hardware-runs.md)) |
| 4 · The shell's channel | Built: the control channel, the launcher's key, and the confirm chord, which does nothing until the vision's Milestone 3 decides what it confirms |
| 5 · Working window manager | Built |
| 6 · Clipboard and drag and drop | Built: copy and paste, the primary selection, and drag and drop between the shell and every app. Capture waits for a record |
| 7 · Displays, input, and hardware | Scale, the layout of displays, a readable scale by default, and hotplug. Input devices' settings wait for a record; suspend and mixed scale wait for the hardware runs |
| 8 · Lock and recovery | Built: the desktop starts locked, the person's password unlocks it through Base's helper, and a compositor restart comes back locked. Locked across suspend waits for the hardware runs |
| 9 · CrowbarOS cutover | Done: CrowbarOS's desktop runs on Fulcrum, built into Base's image |

[The plan](docs/PLAN.md) says what each milestone has, the records that decide Fulcrum, and what is next. [Picking up the work](docs/picking-up.md) says where the work paused, how to run everything, and how to start each next piece.

The compositor Milestone 0 brought here, the baseline, is at commit
`d11400f`. Fulcrum passed the baseline's 57 tests and the Void guest's checks,
and CrowbarOS has built it in the baseline's place since
[crowbaros#198](https://github.com/crowbaros/crowbaros/pull/198) (ADR 0028).

## Layout

| Path | Contents |
| --- | --- |
| `wire/` | `fulcrum-wire`: `codec.rs` (the wire format), `socket.rs` (one end of a connection), and `protocol/` (generated; do not edit) |
| `codegen/` | `fulcrum-codegen`: writes `wire/src/protocol/` from `protocols/`. A development tool, never part of Base |
| `protocols/` | The protocol XML Fulcrum speaks, each file with its own license. [Where each came from](protocols/README.md) |
| `src/event_loop.rs` | One thread on `epoll`, with timers in the loop and a waker for other threads |
| `src/connections.rs` | Every way a client can connect: the shell's socket and the apps' socket |
| `src/identity.rs` | Who a client is: the shell or an app |
| `src/policy.rs` | The one place that says which client is offered which global: the short list each kind of client is refused |
| `src/globals.rs` | Globals and the registry |
| `src/client.rs`, `src/state.rs` | Each client's objects, and how each request reaches the code that carries it out |
| `src/surface.rs` | Surfaces, regions, and subsurfaces: double-buffered state, held commits, stacking |
| `src/shm.rs` | Pools and buffers in a client's shared memory |
| `src/image.rs`, `src/geometry.rs` | The CPU renderer, rectangles, and regions |
| `src/limits.rs` | What one client may make the compositor hold, and the reserve kept for the shell |
| `src/xdg.rs`, `src/windows.rs` | xdg-shell, and where windows go and what each output shows |
| `src/layer.rs` | The shell's layer surfaces and the room they reserve |
| `src/cursor.rs` | The pointer's image |
| `src/cursor_shape.rs` | Cursor shapes a client names |
| `src/xcursor.rs` | Cursor images read from the system's XCursor theme |
| `src/seat.rs` | The keyboard and the pointer, who has them, and the compositor's Super chords |
| `src/workspaces.rs` | The nine workspaces |
| `src/modes.rs` | Maximized, fullscreen, and snapped windows |
| `src/grabs.rs` | Moving and resizing windows with the pointer |
| `src/control.rs`, `src/json.rs` | The shell's control channel, and the flat JSON it speaks |
| `src/data_device.rs` | Data sources and offers, and the clipboard the shell and the apps share |
| `src/drag.rs` | Drag targets, action negotiation, drop, and cancellation |
| `src/primary_selection.rs` | The primary selection, under the clipboard's rules |
| `src/viewporter.rs` | `wp_viewporter`: a surface's crop and scale |
| `src/fractional_scale.rs` | The scale a surface should draw at |
| `src/layout.rs` | The layout of displays, kept by each monitor's identity |
| `src/default_scale.rs` | The scale a display the layout does not name starts at, from its density |
| `src/edid.rs` | A monitor's identity, from its EDID |
| `src/lock.rs` | The locked session: the lock screen alone, and the idle timer |
| `src/idle_inhibit.rs` | Surfaces that keep their outputs from going idle while shown |
| `src/redraw.rs` | Drawing only on change |
| `src/explicit_sync.rs` | Commits that wait for their acquire point, and release points |
| `src/presentation.rs` | When each commit's content was shown |
| `src/evidence.rs` | The line it logs for the checks that run it: commits by window and layer, and frames drawn |
| `src/dmabuf.rs` | linux-dmabuf: feedback, params, and buffers made from dma-bufs |
| `src/scene.rs` | What an output shows, back to front, for either renderer, frames included |
| `src/render.rs` | The GLES renderer's compositor half: a texture for each surface, kept until its content changes, and a scene drawn inside its damage |
| `src/damage.rs` | What changed on an output from one frame to the next, and what a buffer of a given age must draw |
| `src/backend/` | Where frames go: `tty.rs` (a virtual terminal), `session.rs` (Fulcrum's client of seatd), `uevent.rs` (hotplug from netlink), `nested.rs` (a window on a host session), and `headless.rs` (the tests) |
| `sys/` | `fulcrum-sys`: the only crate with `unsafe` code. Every binding to the kernel and to C goes here: `dl.rs` (libraries loaded at run time), `gles.rs` (EGL and GLES: the context, textures, targets, and draws), `gbm.rs`, `dmabuf.rs` (telling a dma-buf from other files, and reading one with the CPU), `syncobj.rs` (DRM timelines), `drm.rs` (mode setting, dumb buffers, and framebuffers, by ioctl), `libinput.rs`, `xkb.rs` (libxkbcommon), and `shm.rs` (copying client pixel rows) |
| `src/log.rs` | One line per event to standard error, at the level `FULCRUM_LOG` sets |
| `probe/` | `crowbar-probe`: test clients, and apps that copy, paste, drag, and drop (`probe/src/apps.rs`). Base's image carries it for its desktop check |
| `tests/` | Tests that run the compositor in-process against real client connections. `table.rs`, `connections.rs`, `attacks.rs`, and `descriptors.rs` are ported from the baseline; `protocol.rs` covers what the compositor refuses; `surfaces.rs` checks pixels, held commits, and the shared-memory attacks; `exhaustion.rs` checks the limits on each client; `redraw.rs` is the baseline's, ported; `windows.rs` covers xdg-shell; `layers.rs` covers layer shell; `keyboard.rs` covers who gets the keys, and `focus.rs` which new window takes them; `seat.rs` covers the pointer and activation; `cursor.rs` and `cursor_shape.rs` cover the pointer's image; `outputs.rs` covers outputs that change size; `data_device.rs`, `primary_selection.rs`, and `viewporter.rs` cover those protocols; `nested.rs` runs Fulcrum nested in Fulcrum; `workspaces.rs` and `control.rs` cover the workspaces and the shell's control channel; `modes.rs` covers maximized, fullscreen, and snapped windows and their frames; `grabs.rs` covers moving and resizing; `gles.rs` draws each scene with both renderers and compares them; `scale.rs` covers outputs at other scales; `layout.rs` covers the layout of displays and the scale a display starts at; `dmabuf.rs` covers linux-dmabuf; `idle_inhibit.rs` covers idle inhibit; `lock.rs` covers the locked session; `gpu_import.rs` covers dma-bufs the GPU draws where they are; `damage.rs` holds each frame drawn where it changed beside the frame drawn whole; `probe_apps.rs` runs the probe's apps as the desktop check does; `shell_outputs.rs` runs the real shell while its only output goes away and comes back, when `CROWBAR_SHELL` names its binary; `linked.rs` reads the built binary for what it links; `dependencies.rs` fails if a crate [the plan](docs/PLAN.md) rules out enters `Cargo.lock` |
| `docs/` | [The plan](docs/PLAN.md), [picking up the work](docs/picking-up.md), [how to run Fulcrum on real hardware](docs/hardware-runs.md), and dated records of what was run |

Only `sys/` may hold `unsafe` code. Every other crate forbids it.

## Run

All four flags are required. There is no default for any of them. On a
virtual terminal, with seatd running and the compositor's user in its group:

```sh
crowbar-compositor \
    --shell-socket /run/crowbar/compositor-shell/wayland --shell-uid 1001 \
    --apps-socket /run/crowbar-apps/wayland-0 --apps-uid 1003
```

Every app connects at the apps' socket, and the compositor accepts a peer
there only if the kernel reports the person's user ID. Base makes its
directory, which only the compositor and the person's group can enter, and
links the socket into the person's `XDG_RUNTIME_DIR` as `wayland-0`, so any
Wayland app finds it.

`--shell-control-socket PATH` adds the shell's control channel, from the
shell's user ID.

For development, a build with the nested feature runs in a window on the
session it is started from:

```sh
cargo run --features nested -- --backend nested \
    --shell-socket /tmp/fulcrum/shell --shell-uid "$(id -u)" \
    --apps-socket /tmp/fulcrum/apps --apps-uid "$(id -u)"
```

Then run apps at the apps' socket:

```sh
WAYLAND_DISPLAY=/tmp/fulcrum/apps foot
WAYLAND_DISPLAY=/tmp/fulcrum/apps crowbar-probe globals
```

## Build and test

The keyboard links libxkbcommon, whose tests need the keymaps of
xkeyboard-config, and the TTY backend links libinput (Arch: `libxkbcommon
libinput`; Debian and Ubuntu: `libxkbcommon-dev xkb-data libinput-dev`; Void:
`libxkbcommon-devel libinput-devel`).

```sh
cargo test --locked
cargo test --locked --features nested
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --all-targets --features nested --locked -- -D warnings
cargo fmt --all --check
```

After changing a file in `protocols/`, regenerate the protocol code. A test
fails while the committed code differs from what the XML generates:

```sh
cargo run -p fulcrum-codegen
```

`wire/tests/protocol.rs` checks every message of every protocol. Each valid
message must survive a typed read and write unchanged. Random bodies,
truncations, and byte streams must never panic it, and a truncated message
with arguments must be refused.

## Where it came from

Fulcrum began in CrowbarOS's `compositor/` directory. On November 17, 2026,
it moved to a repository of its own,
`crowbaros/fulcrum`, starting from commit
[`30fadd8`](https://github.com/crowbaros/crowbaros/tree/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/compositor),
with the seven commits that touched it (tag `import/crowbaros-30fadd8`). The
[Milestone 0 record](docs/independent-build-2026-11-17.md) shows CrowbarOS
building it as a pinned commit. The baseline's code left `main` when this
rewrite began, and it is at commit `d11400f`.

On November 18, 2026, Fulcrum came back to CrowbarOS's `compositor/`
([ADR 0031](../docs/adr/0031-the-compositor-is-built-in-this-repository-again.md)).
It came as the tree of its `main` at `15322b2`, in one commit. The old
repository, `crowbaros/fulcrum`, is archived and keeps the 64 commits before
that. The numbers of its pull requests
in these docs, such as #41, are its own, not CrowbarOS's.

Code adapted from an MIT-licensed project keeps its attribution, in a
`LICENSE-*.txt` file beside Fulcrum's own [`LICENSE`](LICENSE). Code under GPL-3.0 is read
for ideas, and none of it is copied.
