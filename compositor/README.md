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
- **The summon gesture and the Agent's surface**
  ([ADR 0035](../docs/adr/0035-fulcrum-draws-every-ask-and-holds-what-the-agent-may-take.md)):
  Super+Space sends the shell a `summon` event, and no client hears the
  Space. Locked, the gesture is dropped. The Agent's surface is the shell's
  layer surface of namespace `crowbaros-agent`, which the shell shows by
  making it and puts away by destroying it. Made with no output, it goes on
  the display the pointer was on at the gesture. An overlay, it is above
  every window, a fullscreen one included, and reserves no room. The shell
  holds its state. Fulcrum keeps only what the shell cannot know: the
  display of the last gesture, and what had the keyboard when the surface
  took it, so that the shell's `unfocus_agent` can give it back. After an
  unlock the surface is up as it was, with the keyboard if it held it.
- **The shell's control channel**: a path socket of its own, checked against
  the shell's user ID, where the shell asks for a snapshot of the windows and
  shows a workspace or a window. Each message is a length and then JSON, the
  framing of every channel of Fulcrum's own (ADR 0035).
  [The control channel](#the-control-channel) lists its messages. The
  compositor sends the snapshot as an event whenever it changes, so the bar
  shows a workspace as it shows.
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
  descriptors the shell needs, since a reserve is kept for it. The reserve
  covers every connection an app opens, every descriptor an app sends,
  whatever request carries it, and every descriptor the compositor makes for
  an app: a keyboard's keymap, linux-dmabuf's format table, and the eventfd
  of a commit that waits for its acquire point. One client cannot make the
  compositor hold pools or pixels without end.
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

The later milestones, as [the plan](docs/PLAN.md) sets them. The test machine's
runs, on its RTX 5060 and on its integrated Radeon with Mesa, are
[recorded](../docs/development/README.md):

| Milestone | Where it stands |
| --- | --- |
| 3 · Buffers and timing | Built, and run on the test machine's RTX 5060 and its integrated Radeon. The run on Jason Alexander's Mesa machine is next ([how](docs/hardware-runs.md)) |
| 4 · The shell's channel | Built: the control channel, the launcher's key, and the confirm chord, which does nothing until the vision's Milestone 3 decides what it confirms |
| 5 · Working window manager | Built |
| 6 · Clipboard and drag and drop | Built: copy and paste, the primary selection, and drag and drop between the shell and every app. Capture waits for a record |
| 7 · Displays, input, and hardware | Scale, the layout of displays, a readable scale by default, and hotplug. Suspend and a monitor unplugged are recorded on the test machine. Input devices' settings wait for a record. The Mesa machine's run, mixed scale with two monitors, and a dense panel's scale are still to come |
| 8 · Lock and recovery | Built: the desktop starts locked, the person's password unlocks it through Base's helper, and a compositor restart comes back locked. Locked across suspend is recorded on the test machine, with one monitor, and waits for a run with two |
| 9 · CrowbarOS cutover | Done: CrowbarOS's desktop runs on Fulcrum, built into Base's image |

[The plan](docs/PLAN.md) says what each milestone has, the records that decide Fulcrum, and what is next. [Picking up the work](docs/picking-up.md) says where the work paused, how to run everything, and how to start each next piece.

The compositor Milestone 0 brought here, the baseline, is at commit
`d11400f`. Fulcrum passed the baseline's 57 tests and the Void guest's checks,
and CrowbarOS has built it in the baseline's place since
[crowbaros#198](https://github.com/crowbaros/crowbaros/pull/198) (ADR 0028).

## The control channel

The shell's control channel is a path socket of its own, at
`--shell-control-socket`, which takes the shell's user ID alone (ADR 0032).
There the shell learns which windows exist and on which workspace, asks
Fulcrum to show a workspace or a window, locks the desktop, and reads and
sets the layout of displays, and moves the keyboard to the Agent's surface
and back. The requests are a fixed set, and none runs a
command or loads code. `src/control.rs` serves it.

### Framing

Every channel of Fulcrum's own speaks one framing
([ADR 0035](../docs/adr/0035-fulcrum-draws-every-ask-and-holds-what-the-agent-may-take.md)),
which `src/frames.rs` holds:

- **A message is a frame**: its length in four bytes, the most significant
  first, and then that many bytes of JSON in UTF-8. Nothing comes between
  frames. A request is one flat object whose values are strings, whole
  numbers, booleans, or null. A reply or an event may also hold lists.
- **The first frame each way states the protocol version** and nothing else:
  `{"version":1}`. The peer sends its first. Fulcrum answers with its own,
  and the channel goes on. A peer whose first frame is anything else, or
  states a version Fulcrum does not speak, is closed with no answer, and
  Fulcrum logs why. Until a peer has stated the version, it is sent nothing.
- **A request is at most 4096 bytes**, not counting its length. A longer
  frame closes the connection as soon as its length is read. A peer that
  reads nothing until a mebibyte waits for it is closed too. The shell may
  hold four connections at once.

The control channel's version is 1. Here the shell connects, is told the
snapshot, and says the person started an app. The lengths are shown in
hexadecimal:

```text
-> 00 00 00 0d {"version":1}
<- 00 00 00 0d {"version":1}
<- 00 00 00 4c {"event":"snapshot","workspace":1,"active":null,"locked":false,"windows":[]}
-> 00 00 00 12 {"op":"launching"}
<- 00 00 00 0e {"reply":"ok"}
```

### Messages

Each request gets one reply, in the order the requests came. A request
Fulcrum does not know or cannot carry out gets
`{"reply":"error","message":"..."}`, and the channel goes on. Every other
request below is answered `{"reply":"ok"}` unless the table gives its reply.

| Request | What it does |
| --- | --- |
| `{"op":"snapshot"}` | Lists every window with its workspace, the workspace that shows, the window with the keyboard, and whether the desktop is locked. Its reply: `{"reply":"snapshot","workspace":1,"active":"0x2","locked":false,"windows":[{"id":"0x2","app_id":"foot","title":"~","workspace":1,"attention":false,"fullscreen":false}]}`. `active` is null when no window on the workspace that shows has the keyboard. `attention` marks a window that opened without the keyboard and has not had it since, for the bar to show, and `fullscreen` marks a window that covers its output |
| `{"op":"focus_workspace","id":2}` | Shows a workspace, from 1 to 9 |
| `{"op":"focus_window","id":"0x2"}` | Shows a window's workspace, raises the window, and gives it the keyboard |
| `{"op":"focus_agent"}` | Gives the Agent's surface the keyboard. The shell asks as it makes the surface, so one not shown yet takes the keyboard when it shows. Refused while the desktop is locked |
| `{"op":"unfocus_agent"}` | Takes the keyboard from the Agent's surface, or from nothing where the surface has just gone, and gives it to the window that had it when the surface took it, if that window is still on the workspace that shows. A keyboard that is in a window stays there. Refused while the desktop is locked |
| `{"op":"launching"}` | Says the person just started an app from the shell. It changes nothing, since every new window takes the keyboard |
| `{"op":"lock"}` | Locks the desktop. It is answered once every display shows it locked, or after a second where one shows nothing, and the connection is sent nothing in between |
| `{"op":"unlock"}` | Unlocks the desktop. Refused where an unlock authority listens |
| `{"op":"outputs"}` | Lists each display with its modes. Its reply: `{"reply":"outputs","outputs":[{"name":"HDMI-A-1","identity":"DEL DELL U2723QE ABC1234","width":3840,"height":2160,"refresh":60000,"x":0,"y":0,"scale":150,"modes":[{"width":3840,"height":2160,"refresh":60000}]}]}`. `refresh` is in millihertz and `scale` in 120ths, as fractional scale counts |
| `{"op":"configure_output","name":"HDMI-A-1","width":2560,"height":1440,"refresh":59951,"x":0,"y":0,"scale":120}` | Sets what it names of a display's mode, place, and scale, and keeps the display's placement for its monitor (`src/layout.rs`). A field it leaves out stays as it is |

Fulcrum also sends the shell events, at any time, which get no reply:

| Event | When |
| --- | --- |
| `{"event":"snapshot","workspace":2,"active":null,"locked":false,"windows":[]}` | It carries what the snapshot reply does. A connection is sent it once it has stated the version, and again after each turn of the event loop in which something it carries changed, so the bar shows a workspace or a window as it changes, with no asking |
| `{"event":"launcher"}` | The person tapped Super alone, for the shell to open or close its launcher |
| `{"event":"summon","output":"HDMI-A-1","width":2560,"keyboard":false}` | The person pressed Super+Space, the summon gesture. It names the display the pointer is on, with its width on the desktop, in logical units, and says whether the Agent's surface has the keyboard. The shell decides what the gesture does: show the surface, bring the keyboard back to it, or put it away. Not sent while the desktop is locked |
| `{"event":"locked"}`, `{"event":"unlocked"}` | The desktop locked or unlocked, whoever asked: Super+L and the idle timer lock it too. A connection that asks is sent the event before its reply |

ADR 0032 accepts these messages but the summon gesture's: `summon`,
`focus_agent`, and `unfocus_agent`. ADR 0035, which is proposed, decides
that Fulcrum takes the gesture and tells the shell, and that the shell shows
the Agent's surface, puts it away, and moves the keyboard by asking. It
leaves each message to the
[Milestone 3 spec](https://github.com/crowbaros/crowbaros/issues/291), which
has them written here. The spec names requests to show and hide the surface;
the shell asks for those through layer shell, by making the surface and
destroying it, so the control channel carries only the keyboard's two.

### The unlock authority's socket

Where `--unlock-socket` is given, the unlock authority has a socket of its
own, which takes `--unlock-uid` alone. It speaks the same framing and the
same version, and is served by the same code. The authority may send `lock`
and `unlock`, with no other field, and nothing else, and it is sent no
events. Base's `crowbar-unlock` is its one peer.

### The Harness's socket

Where `--agent-socket` is given, the Harness has a socket of its own
([ADR 0035](../docs/adr/0035-fulcrum-draws-every-ask-and-holds-what-the-agent-may-take.md)),
which takes the person's user ID (`--apps-uid`) alone. It speaks the same
framing and the same version, and is served by the same code. A new
connection from the person's user ID replaces the old one; a peer with any
other user ID is refused and leaves the live connection alone. It is sent no
events.

| Request | What it does |
| --- | --- |
| `{"op":"snapshot"}` | The desktop context: the workspace that shows, the window with the keyboard (or the one that had it when the Agent's surface took it, while still on the workspace that shows), whether the desktop is locked, every window by workspace, and every display. Never window contents. Its reply: `{"reply":"snapshot","workspace":1,"active":"0x2","locked":false,"windows":[{"id":"0x2","app_id":"foot","title":"~","workspace":1}],"displays":[{"name":"HDMI-A-1","width":3840,"height":2160,"x":0,"y":0,"scale":150}]}` |

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
| `src/agent.rs` | The summon gesture, and what Fulcrum keeps for the Agent's surface |
| `src/cursor.rs` | The pointer's image |
| `src/cursor_shape.rs` | Cursor shapes a client names |
| `src/xcursor.rs` | Cursor images read from the system's XCursor theme |
| `src/seat.rs` | The keyboard and the pointer, who has them, and the compositor's Super chords |
| `src/workspaces.rs` | The nine workspaces |
| `src/modes.rs` | Maximized, fullscreen, and snapped windows |
| `src/grabs.rs` | Moving and resizing windows with the pointer |
| `src/control.rs`, `src/frames.rs`, `src/json.rs` | The shell's control channel and the unlock authority's socket, the framing of every channel of Fulcrum's own, and the flat JSON a request is |
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
| `tests/` | Tests that run the compositor in-process against real client connections. The files in `suite/` are the modules of one test binary, `suite/main.rs`, and their tests run side by side in one process; a file directly in `tests/` is a binary of its own, for a test that needs the process to itself. `suite/table.rs`, `suite/connections.rs`, `suite/attacks.rs`, and `descriptors.rs` are ported from the baseline; `suite/protocol.rs` covers what the compositor refuses; `suite/surfaces.rs` checks pixels, held commits, and the shared-memory attacks; `exhaustion.rs` checks the limits on each client; `suite/redraw.rs` is the baseline's, ported; `suite/windows.rs` covers xdg-shell; `suite/layers.rs` covers layer shell; `suite/keyboard.rs` covers who gets the keys, and `suite/focus.rs` which new window takes them; `suite/seat.rs` covers the pointer and activation; `suite/cursor.rs` and `suite/cursor_shape.rs` cover the pointer's image; `suite/outputs.rs` covers outputs that change size; `suite/data_device.rs`, `suite/primary_selection.rs`, and `suite/viewporter.rs` cover those protocols; `suite/nested.rs` runs Fulcrum nested in Fulcrum; `suite/workspaces.rs` and `suite/control.rs` cover the workspaces and the shell's control channel, with its framing; `suite/summon.rs` covers the summon gesture and the Agent's surface; `suite/agent_socket.rs` covers the Harness's socket and its desktop-context snapshot; `suite/modes.rs` covers maximized, fullscreen, and snapped windows and their frames; `suite/grabs.rs` covers moving and resizing; `suite/gles.rs` draws each scene with both renderers and compares them; `suite/scale.rs` covers outputs at other scales; `suite/layout.rs` covers the layout of displays and the scale a display starts at; `suite/dmabuf.rs` covers linux-dmabuf; `suite/idle_inhibit.rs` covers idle inhibit; `suite/lock.rs` covers the locked session; `suite/gpu_import.rs` covers dma-bufs the GPU draws where they are; `suite/damage.rs` holds each frame drawn where it changed beside the frame drawn whole; `suite/probe_apps.rs` runs the probe's apps as the desktop check does; `shell_outputs.rs` runs the real shell while its only output goes away and comes back, when `CROWBAR_SHELL` names its binary; `suite/linked.rs` reads the built binary for what it links; `suite/dependencies.rs` fails if a crate [the plan](docs/PLAN.md) rules out enters `Cargo.lock` |
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

`--shell-control-socket PATH` adds [the shell's control
channel](#the-control-channel), from the shell's user ID. `--agent-socket PATH` adds [the Harness's
socket](#the-harnesss-socket), from the person's user ID (`--apps-uid`).

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

The tests in `tests/suite/` are one binary, so that Cargo links them once.
A new file there takes a `mod` line in `tests/suite/main.rs`, and its tests
must not change what the process shares: the environment, the working
directory, or a resource limit. To run one file's tests, name its module:

```sh
cargo test --locked --test suite table::
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
