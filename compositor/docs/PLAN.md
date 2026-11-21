# Fulcrum

## A Wayland compositor for CrowbarOS, written from the ground up

Fulcrum is the CrowbarOS Wayland compositor, written from the ground up in Rust. Fulcrum writes its Wayland wire protocol, event loop, display and input handling, renderer, and window management. It studies Smithay and the compositors built on it for design ideas. The build excludes crates from the Smithay organization, including `smithay`, `wayland-rs`, `drm-rs`, `gbm.rs`, `input.rs`, `calloop`, and `libseat-rs`. It links Mesa's EGL, GLES, and GBM, plus libinput and libxkbcommon ([ADR 0028](../../docs/adr/0028-the-compositor-is-written-from-the-ground-up.md)). [Smithay](https://github.com/Smithay/smithay)

## What CrowbarOS needs of it

[The vision](../../docs/VISION.md) is a small desktop with an Agent at the person's side. Its Milestone 2, a desktop, is the compositor running the shell and Studio Settings on Base, in the VM and on real hardware. Fulcrum is the part of that desktop that owns the pixels and the input. The vision needs three things of it, as [ADR 0032](../../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md) says:

- **No app can pass for the shell.** Only the shell draws the bar, the dock, the launcher, the wallpaper, and the lock screen, and only the shell arranges the desktop through the control channel.
- **The compositor owns the summon gesture.** Every chord with Super is the compositor's, and no client hears it. Which key summons the Agent is the vision's Milestone 3 to decide.
- **A yes comes from the person's own hand.** The confirm chord, Ctrl+Alt+Enter, is the compositor's and reaches no client. What it confirms is Milestone 3's to decide; until then it does nothing.

Everything else is what a person expects of a desktop: windows that open where they look and take the keyboard, nine workspaces, copy and paste and drag and drop between their apps, displays that keep their layout, a lock that holds across a restart and a suspend, and the GPU drawing, on NVIDIA's driver and on Mesa.

No archived record binds Fulcrum. What it does as a desktop is what [its README](../README.md) says, and changes by pull request (ADR 0032).

**Where it started.** [Milestone 0](independent-build-2026-11-17.md) moved CrowbarOS's Smithay-based compositor into a repository of its own, `crowbaros/fulcrum`, and made CrowbarOS build a pinned commit. That compositor, at `d11400f`, was the behavioral specification: its 57 tests and the 36 checks of CrowbarOS's Void guest said what the new compositor had to do. The new one replaced it at Milestone 2. On November 18, 2026, Fulcrum came back to CrowbarOS's `compositor/`, and the pin went ([ADR 0031](../../docs/adr/0031-the-compositor-is-built-in-this-repository-again.md)). Bare pull request numbers in this plan, such as #41, are the archived `crowbaros/fulcrum`'s; `crowbaros#` numbers are CrowbarOS's.

## What Fulcrum writes, and what it learns from

| Layer | Fulcrum writes | Fulcrum uses | Smithay's counterpart to study |
| --- | --- | --- | --- |
| Wire protocol | The message codec, descriptor passing, object IDs, and per-client limits. Rust code generated from the protocol XML at build time, for the server and the client side | Unix sockets and `SCM_RIGHTS` through `rustix` | `wayland-backend`, `wayland-scanner` |
| Event loop | One thread on `epoll`, with timers and signals as descriptors | `rustix` | `calloop` |
| Protocol state | Every global: double-buffered surface state, subsurface sync, buffer release, frame callbacks, the seat and focus | — | `smithay::wayland` |
| Shared memory | Copy only committed pixel rows and report a short read as an invalid buffer | Positioned file reads | `smithay::wayland::shm` |
| Rendering | A CPU renderer for tests and fallback, a GLES 2 renderer, damage tracking, and later scanout planes | EGL, GLES, and GBM from Mesa, through Fulcrum's own bindings | `smithay::backend::renderer`, its damage tracker, and DRM compositor |
| Displays | KMS atomic modesetting and page flips through the kernel's ioctls; connectors, CRTCs, and planes; hotplug from netlink uevents | The kernel's DRM interface | `drm-rs`, `smithay::backend::drm`, `udev` |
| Session | A client of seatd's socket protocol: opening devices, switching virtual terminals, pausing and resuming | seatd, which CrowbarOS already runs | `libseat-rs`, `smithay::backend::session` |
| Input | Device handling and key state, with the keymap shared with clients through a memfd | libinput's path backend and libxkbcommon, through Fulcrum's own bindings | `input.rs`, `smithay::input` |
| Nested backend | Fulcrum as a Wayland client of the host session, for development | The client side of Fulcrum's own wire code | Smithay's winit backend |

Fulcrum's code is split into four crates. Only one of them may hold `unsafe` code:

| Crate | Holds |
| --- | --- |
| `wire/` (`fulcrum-wire`) | The codec and the generated protocol code for both sides. No `unsafe` |
| `sys/` (`fulcrum-sys`) | Every foreign-function binding and ioctl. The only crate with `unsafe` |
| The root (`crowbar-compositor`) | The compositor. `#![forbid(unsafe_code)]` |
| `probe/` (`crowbar-probe`) | Test clients on `fulcrum-wire`'s client side, and apps that copy, paste, drag, and drop. Base's image carries it for its desktop check |

`protocols/` holds the vendored XML for every protocol Fulcrum speaks, with each protocol's license. The binary stays `crowbar-compositor` until CrowbarOS changes its services in a pull request of their own.

### References

Read each reference to learn how it solves a problem. A project's license decides what may be taken.

| Reference | License | What to take |
| --- | --- | --- |
| [Smithay](https://github.com/Smithay/smithay) and its Anvil and Smallvil examples | MIT | Design ideas, especially cached surface state, damage tracking, and DRM plane assignment |
| [Jay](https://github.com/mahkoh/jay) | GPL-3.0 | Not on Smithay's list. It shows that this approach works in Rust: its own wire code generated from XML, its own DRM, GBM, and graphics code, and its own event loop. Ideas only |
| [niri](https://github.com/YaLTeR/niri) | GPL-3.0 | Frame clock and presentation timing, deliberate window placement when a monitor goes away and comes back, and randomized layout tests. Its scrolling columns are a candidate layout for later. Ideas only |
| [COSMIC](https://github.com/pop-os/cosmic-comp) | GPL-3.0 | The compositor, shell, and settings each have one owner and restart independently, yet ship as one desktop. Ideas only |
| [Pinnacle](https://github.com/Ottatop/pinnacle) | GPL-3.0 | Layout as a policy separate from the window model, with configuration kept outside the trusted process. Ideas only |
| [Otto](https://github.com/nongio/otto) | MIT | Placement, snapping, and keyboard switching that feel like one interaction in a stacking desktop. Code may be adapted with attribution |

## What it guards

Owning the wire layer lets Fulcrum enforce CrowbarOS's decisions where messages enter, not on top of someone else's dispatch.

- **One registry, with the policy** (ADR 0032). Each kind of client is offered every global the compositor has, less a short list: an app is refused layer shell, session lock, and security contexts, and the shell security contexts. A global a client is not offered is never advertised, and binding it by number is a protocol error. A global the compositor gains later is offered to every client unless it is added to a list.
- **Identity at accept** (ADR 0032). Two kinds of client, each at a path socket of its own checked by the peer's user ID: the shell, and the person's apps. The shell's control channel and the unlock authority's socket check their peers the same way.
- **Limits for each client.** Fulcrum caps objects, descriptors, and shared-memory bytes per client, and keeps a reserve of descriptors for the shell. A client that passes its cap is disconnected, and every other client keeps being served. This closes [issue 174](https://github.com/crowbaros/crowbaros/issues/174) at its root.
- **Malformed input is a disconnect, never a panic.** Handlers return errors and do not unwrap client data. The codec gets property tests over random and truncated messages.
- **A test guards what is linked.** As in the baseline, the build fails if the binary gains X11, libei, or code that opens a public socket. It also fails if a crate from the Smithay organization appears in `Cargo.lock`.

## Implementation milestones

These are Fulcrum's own milestones, not the vision's. Each ends in an exit check.

| Milestone | Deliverable | Exit check |
| --- | --- | --- |
| **0 · Independent build** (done) | The Smithay-based compositor in its own repository, built by CrowbarOS at a pinned commit. [Record](independent-build-2026-11-17.md) | Met: 36 of 36 guest checks, byte-identical binaries on both sides |
| **1 · Wayland core** (done) | The four crates; `fulcrum-wire` with generated code for the core protocol, xdg-shell, layer shell, the security context, and viewporter; the event loop; the sockets and identity; the registry and policy table; `wl_compositor`, `wl_surface`, `wl_region`, `wl_subcompositor`, `wl_shm`, `wl_output`, and frame callbacks; per-client limits; a headless backend with the CPU renderer; and the probe on `fulcrum-wire`. [Record](wayland-core-2026-11-17.md) | Met: the baseline's connection, table, attack, descriptor, exhaustion, and linked-code tests ported. Codec property tests pass. A client's shared-memory pixels appear in the CPU renderer's output |
| **2 · First light** (done) | xdg-shell, layer shell, and the seat, with libxkbcommon keymaps and the pointer. Redraw on change. KMS atomic modesetting with dumb buffers drawn by the CPU renderer, the seatd client, libinput, uevent hotplug, and the nested backend. [Record](first-light-2026-11-17.md) | Met: all 57 baseline tests ported and passing; the Void guest check passes against the new binary, and Void's own `foot` and `wayland-info` run on it. CrowbarOS moved to the new compositor |
| **3 · Buffers and timing** (built; the hardware runs are next) | The GLES renderer, GBM buffers for KMS, and damage tracking. linux-dmabuf through GBM and EGL, explicit sync, presentation time, fractional scale, cursor images clipped so they cover no frame, and the cursor plane | The shell and a GPU client draw on Fulcrum in the VM, on the test machine's RTX 5060 with NVIDIA's driver, and on Jason Alexander's Mesa machine. A still desktop draws no frames |
| **4 · The shell's channel** (done) | The shell's control channel, with the launcher's key, `launching`, and `attention`; the confirm chord, which reaches no client and does nothing until the vision's Milestone 3 decides what it confirms | The shell lists and shows windows and workspaces; no app reaches the control channel; Super tapped alone opens the launcher |
| **5 · Working window manager** (done) | Frames no client can draw or cover, stacking, click and keyboard focus, move, resize, maximize, fullscreen, half-screen snap, and nine workspaces shared across displays | Launch two real apps from the shell, arrange them by mouse and keyboard, open a child dialog, switch workspaces, and return. Focus follows the person's action. A fullscreen app keeps its frame |
| **6 · Clipboard and drag and drop** (done; capture waits for a record) | The clipboard, the primary selection, and drag and drop between the shell and every app (ADR 0032). Capture by the shell, once a record decides it | Keyboard paste, middle click, and drop each work between two apps in CrowbarOS's desktop check |
| **7 · Displays, input, and hardware** (in part) | Each display's mode, place, and scale kept by its identity; a readable scale for a display the layout does not name; hotplug; input devices' settings; suspend and resume | Results on the VM and the two hardware machines: a monitor unplugged and plugged in, a dense panel's scale, mixed scale, and frames after resume |
| **8 · Lock and recovery** (done in the VM; the hardware runs are next) | Start locked; the lock screen alone drawn and given input, other displays blank; unlock by the person's password through Base's unlock helper; lock by the shell, Super+L, the idle timer, and before suspend | CrowbarOS's desktop check: a wrong password then the right one, with nothing typed first; a compositor restart while the desktop is unlocked comes back locked before an app hears a key. On the hardware machines: locked across suspend with a second monitor attached |
| **9 · CrowbarOS cutover** (done) | CrowbarOS's desktop runs on Fulcrum, built into Base's image, and the spike is deleted | CrowbarOS's desktop check passes on Base's image |

## Where it stands

- **Milestones 0, 1, and 2** are done, and CrowbarOS builds the new compositor.
- **Milestone 3** has:
  - the GLES renderer (#15);
  - linux-dmabuf (#16), whose buffers the GPU imports in any layout EGL takes and draws where they are (#28);
  - fractional scale (#22), presentation time (#23), and clients' cursor images (#24);
  - explicit sync (#26);
  - damage tracking: by scene (#29), by what a client damaged (#36), and in uploads (#37);
  - the cursor plane (#33).

  Still to come: the runs on the test machine ([crowbaros#227](https://github.com/crowbaros/crowbaros/issues/227)) and the Mesa machine ([crowbaros#228](https://github.com/crowbaros/crowbaros/issues/228)), which [Running Fulcrum on real hardware](hardware-runs.md) says how to run. A fullscreen client on the primary plane is ruled out, because a fullscreen window keeps its frame inside the display's edge. Overlay planes may come later.
- **Milestone 4** has the shell's control channel (#17), the launcher's key, `launching`, and `attention` (#41, with the shell's half in [crowbaros#203](https://github.com/crowbaros/crowbaros/pull/203)), and the confirm chord (#46), which does nothing until the vision's Milestone 3 decides on it ([crowbaros#223](https://github.com/crowbaros/crowbaros/issues/223)). Every new window takes the keyboard, as on any desktop, so `launching` changes nothing; a window that maps while the desktop is locked still wants attention.
- **Milestone 5** has workspaces (#17); maximize, fullscreen, snap, and frames (#18); move and resize (#20); dialogs that take the keyboard (#25); and every global an app is offered, idle inhibit (#30) and cursor shape (#32) last.
- **Milestone 6** has copy and paste (#38), primary selection (#43), and drag and drop, between the shell and every app: one selection, one primary selection, and a drag that drops on any client. A drag takes an active pointer grab.
- **Milestone 7** has each display's scale, 1 to 3 in quarters (#35), the layout kept by each monitor's identity and set through the control channel (#39), and a scale from its density for a display the layout does not name ([crowbaros#262](https://github.com/crowbaros/crowbaros/pull/262)). Still to come: input devices' settings, once a record decides them, and suspend and resume, mixed scale, and hotplug on the hardware machines.
- **Milestone 8** has lock (#44, with the shell's half in [crowbaros#204](https://github.com/crowbaros/crowbaros/pull/204)) and the unlock authority (#49, with Base's half in [crowbaros#207](https://github.com/crowbaros/crowbaros/pull/207)):
  - The shell's request, Super+L, and the idle timer lock the desktop.
  - While it is locked, the shell's lock screen is all that shows and all that gets input, and it takes the person's password at once, with no chord first ([crowbaros#223](https://github.com/crowbaros/crowbaros/issues/223)).
  - Base's unlock helper, at a socket of its own, checks the person's password and tells the compositor to unlock. Once it listens, the shell's word is not enough.
  - Base starts the desktop locked, zzz's suspend hook locks it through the helper before the machine sleeps, and a compositor restart comes back locked before an app hears a key ([crowbaros#265](https://github.com/crowbaros/crowbaros/pull/265)).

  Still to come: locked across suspend with a second monitor, on the hardware machines.
- **Milestone 9:** CrowbarOS's desktop runs on Fulcrum ([crowbaros#199](https://github.com/crowbaros/crowbaros/pull/199)), and the spike is deleted ([crowbaros#200](https://github.com/crowbaros/crowbaros/pull/200)). Base's image builds Fulcrum, and both of CrowbarOS's graphical checks boot that image ([crowbaros#263](https://github.com/crowbaros/crowbaros/pull/263)).

Milestones 1 and 2 were where this approach cost the most: they rebuilt what Smithay gave the baseline for free before any new behavior landed. Fulcrum keeps that cost bounded by building only what the desktop needs: no XWayland, one seat, and GLES as the only GPU renderer until a measurement asks for more.

**How the plan changed.** Fulcrum was first planned against the substrate vision, whose broker, Warden, and compartments CrowbarOS archived when it started again on November 17, 2026 ([crowbaros#197](https://github.com/crowbaros/crowbaros/pull/197)). ADR 0032 replaced those records for the compositor. The Warden's socket, compartments, and their budgets are gone from the code. The parts of Milestones 4, 6, and 8 that the substrate vision gave to its broker, asks and their arming, the restart notice, the broker's GPU-need declaration, transfers it recorded, and its record of each unlock, are dropped: every app is offered the GPU, the person's act moves data between apps, and Base's password helper unlocks. They are at the `substrate-final` tag.

## Decisions

The records that decide what Fulcrum is:

| Decision | Record |
| --- | --- |
| CrowbarOS owns its compositor | [ADR 0015](../../docs/adr/0015-our-own-compositor-on-smithay.md) |
| It is written from the ground up, with no crate from the Smithay organization | [ADR 0028](../../docs/adr/0028-the-compositor-is-written-from-the-ground-up.md) |
| It is built in this repository, and reaches Base when a change merges | [ADR 0031](../../docs/adr/0031-the-compositor-is-built-in-this-repository-again.md) |
| Its two kinds of client, what each is offered, and the proposals it accepts | [ADR 0032](../../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md) |

ADR 0032 accepts as built the shell's messages on the control channel (`launcher`, `launching`, `attention`, `lock`, `unlock`, `locked`, `unlocked`), the lock screen's namespace, `crowbaros-lock`, the unlock authority's socket (`--unlock-socket`, `--unlock-uid`, and `--start-locked`), and the layout of displays (`outputs` and `configure_output`).

Records still to write before Fulcrum builds more:
- what Fulcrum offers the Agent, including the summon gesture and what the confirm chord confirms ([crowbaros#239](https://github.com/crowbaros/crowbaros/issues/239)), for the vision's Milestone 3;
- whether the shell captures the screen;
- how Studio Settings sets input devices.

Next without a new record, each with how to start it in [Picking up the work](picking-up.md): the hardware runs, on the test machine and the Mesa machine ([how](hardware-runs.md)).

## After the first release

1. **Everyday access.** Work with the shell's authors on input methods and cross-app screen-reader support. Qualify each with a person completing a real task across two apps.
2. **Window sharing.** Implement a picker the person uses to share a single window. Use a real call to verify target selection, resize, closing the shared window, and revocation.
3. **Optional layouts.** Prototype niri-style scrolling columns and Pinnacle-style dynamic tiling against the same window model. Compare them on a three-app workflow, a transient dialog, keyboard navigation, and monitor removal. Ship the mode that earns its complexity.
4. **Hardware expansion.** Add AMD, hybrid-GPU, and aarch64 rows as machines become available. Treat high-refresh frame pacing, VRR, color management, and a Vulkan renderer as separate features, each with a measurable acceptance test.
