# Fulcrum

## A Wayland compositor for CrowbarOS, written from the ground up

Fulcrum is the CrowbarOS Wayland compositor, written from the ground up in Rust. Fulcrum writes its Wayland wire protocol, event loop, display and input handling, renderer, and window management. It studies Smithay and the compositors built on it for design ideas. The build excludes crates from the Smithay organization, including `smithay`, `wayland-rs`, `drm-rs`, `gbm.rs`, `input.rs`, `calloop`, and `libseat-rs`. It links Mesa's EGL, GLES, and GBM, plus libinput and libxkbcommon. [Smithay](https://github.com/Smithay/smithay)

Its first customer is the CrowbarOS Base image. A successful release lets a person boot CrowbarOS, launch and arrange applications, use multiple displays, move data deliberately between apps, and lock and recover the session. The GPUI shell supplies the bar, dock, launcher, and Settings UI. Fulcrum owns the pixels and input decisions those components must be able to trust.

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
| [niri](https://github.com/YaLTeR/niri) | GPL-3.0 | Frame clock and presentation timing, deliberate window placement when a monitor goes away and comes back, and randomized layout tests. Its scrolling columns are a post-Beta layout candidate. Ideas only |
| [COSMIC](https://github.com/pop-os/cosmic-comp) | GPL-3.0 | The compositor, shell, and settings each have one owner and restart independently, yet ship as one desktop. Ideas only |
| [Pinnacle](https://github.com/Ottatop/pinnacle) | GPL-3.0 | Layout as a policy separate from the window model, with configuration kept outside the trusted process. Ideas only |
| [Otto](https://github.com/nongio/otto) | MIT | Placement, snapping, and keyboard switching that feel like one interaction in a stacking desktop. Code may be adapted with attribution |

## Security comes with the protocol code

Owning the wire layer lets Fulcrum enforce the CrowbarOS decisions where messages enter, not on top of someone else's dispatch.

- **One registry, with the policy** ([ADR 0032](../../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)). Each kind of client is offered every global the compositor has, less a short list: an app is refused layer shell, session lock, and security contexts, and the shell security contexts. A global a client is not offered is never advertised, and binding it by number is a protocol error. A global the compositor gains later is offered to every client unless it is added to a list.
- **Identity at accept** (ADR 0032). Two kinds of client, each at a path socket of its own checked by the peer's user ID: the shell, and the person's apps.
- **Limits for each client.** Fulcrum caps objects, descriptors, and shared-memory bytes per client, and keeps a reserve of descriptors for the shell. A client that passes its cap is disconnected, and every other client keeps being served. This closes [issue 174](https://github.com/crowbaros/crowbaros/issues/174) at its root.
- **Malformed input is a disconnect, never a panic.** Handlers return errors and do not unwrap client data. The codec gets property tests over random and truncated messages.
- **A test guards what is linked.** As in the baseline, the build fails if the binary gains X11, libei, or code that opens a public socket. It also fails if a crate from the Smithay organization appears in `Cargo.lock`.

## Implementation milestones

| Milestone | Deliverable | Exit check |
| --- | --- | --- |
| **0 · Independent build** (done) | The Smithay-based compositor in its own repository, built by CrowbarOS at a pinned commit. [Record](independent-build-2026-11-17.md) | Met: 36 of 36 guest checks, byte-identical binaries on both sides |
| **1 · Wayland core** (done) | Replace `main`. The four crates; `fulcrum-wire` with generated code for the core protocol, xdg-shell, layer shell, the security context, and viewporter; the event loop; the trusted sockets and identity; the registry and policy table; `wl_compositor`, `wl_surface`, `wl_region`, `wl_subcompositor`, `wl_shm`, `wl_output`, and frame callbacks; per-client limits; a headless backend with the CPU renderer; and the probe on `fulcrum-wire` | Ported from the baseline: the connection, table, attack, descriptor, exhaustion, and linked-code tests. Codec property tests pass. A client's shared-memory pixels appear in the CPU renderer's output |
| **2 · First light** (done) | xdg-shell, layer shell, and the seat, with libxkbcommon keymaps and the pointer. Redraw on change. KMS atomic modesetting with dumb buffers drawn by the CPU renderer, the seatd client, libinput, uevent hotplug, and the nested backend. The data device, which moves nothing until slice 4, and `wp_viewporter`, which `foot` needs to start. [Record](first-light-2026-11-17.md) | All 57 baseline tests ported and passing. The Void guest check passes against the new binary, and Void's own `foot` and `wayland-info` run on it. **CrowbarOS then moves its pin to the new compositor** |
| **3 · Buffers and timing** (built; not yet run on real GPUs) | The GLES renderer, GBM buffers for KMS, and damage tracking, which first light did without: it draws with the CPU into dumb buffers, as every display the guest and the lab machines have can show. Then linux-dmabuf through GBM and EGL, explicit sync, presentation time, fractional scale, cursor images clipped so they cover no frame, and scanout planes | The GPUI shell draws on Fulcrum in the Void guest and on the RTX 4090 and Intel machines. A still desktop still draws no frames |
| **4 · Trusted shell session** (in part; the rest dropped) | Slice 2, [issue 71](https://github.com/crowbaros/crowbaros/issues/71): the shell and broker control channels, asks and arming, the confirm chord, the restart notice, and the broker's GPU-need declaration. The local branch `wip/slice-2-on-smithay` holds a first wire format and the rules, ready to port | ADR 0018's tests 1 to 21 that fall to the compositor. One physical confirmation answers exactly one shown ask. A changed ask or a shell restart before the chord cannot be confirmed |
| **5 · Working window manager** (built) | Slice 3, [issue 78](https://github.com/crowbaros/crowbaros/issues/78): trusted frames and stacking, click and keyboard focus, move, resize, maximize, fullscreen, half-screen snap, and nine workspaces shared across displays | Launch two real apps from the shell, arrange them by mouse and keyboard, open a child dialog, switch workspaces, and return. Focus follows the person's action. A fullscreen app keeps its trusted frame |
| **6 · Clipboard and capture** (in part) | Clipboard, primary selection, and drag and drop between the shell and every app ([ADR 0032](../../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)), and shell-owned capture. Slice 4, [issue 166](https://github.com/crowbaros/crowbaros/issues/166), tied each transfer to the broker; that part is dropped | Keyboard paste, middle click, menu paste, and drop each work between apps |
| **7 · Displays, input, and hardware** (in part) | Slice 5, [issue 79](https://github.com/crowbaros/crowbaros/issues/79): saved modes, positions, and scale per display, hotplug, input devices, and suspend and resume | Results on the Void guest, the RTX 4090, the Intel iGPU, and the Surface ARM target, including mixed scale, display removal, frames after resume, and missed presentation deadlines measured against each display's refresh |
| **8 · Lock and recovery** (in part, as a proposal) | Slice 6, [issue 87](https://github.com/crowbaros/crowbaros/issues/87): start locked, input to the lock screen alone, blank secondary displays, and broker-authorized unlock, with idle, suspend, and service restart | Restart the shell during an ask, restart the compositor during a session, and suspend with a second monitor attached. Each reaches its documented state before an app receives input |
| **9 · CrowbarOS cutover** (the Alpha desktop) | The Alpha desktop runs on Fulcrum with the production interfaces, and the spike is deleted | The full desktop smoke check passes, and the previous Base generation still boots |

**Where it stands, November 18, 2026.**

- **Milestones 1 and 2** are done, and CrowbarOS builds the new compositor.
- **Milestone 3** has:
  - the GLES renderer (#15);
  - linux-dmabuf (#16), whose buffers the GPU imports in any layout EGL takes and draws where they are (#28);
  - fractional scale (#22), presentation time (#23), and clients' cursor images (#24);
  - explicit sync (#26);
  - damage tracking: by scene (#29), by what a client damaged (#36), and in uploads (#37);
  - the cursor plane (#33).

  Still to come: the real GPUs, the RTX 4090, Intel, and Surface rows, which [Running Fulcrum on real hardware](hardware-runs.md) says how to run. A fullscreen client on the primary plane is ruled out, because ADR 0020 keeps a fullscreen window's frame inside the display's edge. Overlay planes may come later.
- **Milestone 4** has the shell's control channel (#17) and the confirm chord, which locks the session and opens the unlock entry (#46). Asks and their arming, the restart notice, and the broker's GPU-need declaration waited on the broker, and ADR 0032 drops them: every app is offered the GPU.
- **Milestone 5** has:
  - workspaces and the shell's control channel (#17);
  - maximize, fullscreen, snap, and frames (#18);
  - move and resize (#20);
  - dialogs that take the keyboard (#25);
  - every global an app is offered, idle inhibit (#30) and cursor shape (#32) last;
  - the launcher's key, a launched app's window taking the keyboard, and the attention of any other new window (#41, with the shell's half in [crowbaros#203](https://github.com/crowbaros/crowbaros/pull/203)). Every new window now takes the keyboard, as on any desktop, so `launching` changes nothing; a window that maps while the session is locked still wants attention.

  The messages #41 adds are a proposal until a CrowbarOS record decides them: Super tapped alone, the shell's `launching`, and the snapshot's `attention`.
- **Milestone 6** has copy and paste (#38), primary selection (#43), and drag and drop, between the shell and every app: one selection, one primary selection, and a drag that drops on any client. A drag takes an active pointer grab. Capture by the shell waits for a CrowbarOS record.
- **Milestone 7** has each display's scale, 1 to 3 in quarters (#35), and the layout kept by each monitor's identity and set through the control channel (#39). Its wire format is Fulcrum's proposal until CrowbarOS's display settings take it up. Still to come: input devices' settings, and suspend and resume and mixed scale on the hardware rows.
- **Milestone 8** has lock (#44, with the shell's half in [crowbaros#204](https://github.com/crowbaros/crowbaros/pull/204)) and the confirm chord's part (#46, [crowbaros#206](https://github.com/crowbaros/crowbaros/pull/206)), as a proposal:
  - The shell's request, Super+L, Ctrl+Alt+Enter, and the idle timer lock the session.
  - While it is locked, the shell's lock screen is all that shows and all that gets input, and it takes no text until the confirm chord opens its unlock entry.
  - In the place of the broker ADR 0020 has end it, an unlock authority unlocks it (#49, with Base's half in [crowbaros#207](https://github.com/crowbaros/crowbaros/pull/207)). It is a root helper in Base, at a socket of its own, that checks the person's login password after the confirm chord. Once it listens, the shell's word is not enough.
  - Base starts the session locked, and zzz's suspend hook locks it through the helper before the machine sleeps.

  Still to come:
  - the person's proof beyond a password (issues 87 and 100), if a record asks for it; the broker's record of each unlock is dropped (ADR 0032);
  - a desktop check that restarts the compositor during a session and finds the session locked;
  - suspend on real hardware, with a second monitor, which waits for the hardware rows.
- **Milestone 9:** CrowbarOS's Alpha desktop runs on Fulcrum ([crowbaros#199](https://github.com/crowbaros/crowbaros/pull/199)), and its desktop check passes there. The spike is deleted ([crowbaros#200](https://github.com/crowbaros/crowbaros/pull/200)).

CrowbarOS started again on November 17 ([crowbaros#197](https://github.com/crowbaros/crowbaros/pull/197)). Its vision is now a small desktop with an Agent, and its substrate vision, with the broker, the Warden, and compartments, is archived. ADRs 0015 and 0028 stand. On November 18, Fulcrum came back to CrowbarOS's `compositor/`, and ADR 0031 replaced ADR 0027. The same day, [ADR 0032](../../docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md) said what the compositor offers the shell and the person's apps, and that no archived record binds it. The Warden's socket, compartments, and their budgets are gone. What it offers the Agent is for CrowbarOS's Milestone 3, the Agent, to decide, not this plan's Milestone 3.

Fulcrum builds what those records still decide:
- the window manager;
- buffers and timing;
- displays and hardware.

The parts of Milestones 4, 6, and 8 that rested on the broker are dropped: asks and their arming, transfers the broker records, and its record of each unlock. Milestone 8's unlock is Base's password helper.

Milestones 1 and 2 are where this approach costs the most. They rebuild what Smithay gave the baseline for free before any new behavior lands. The cost is bounded by building only what ADR 0020's table offers: no XWayland, one seat, and GLES as the only GPU renderer until a measurement asks for more.

## Decisions this needs in CrowbarOS

[ADR 0015](../../docs/adr/0015-our-own-compositor-on-smithay.md) says the compositor is built on Smithay, and ADR 0020 sets rules for bumping a Smithay pin. A new CrowbarOS record, ADR 0028, replaces both: Fulcrum is written from the ground up, with no crate from the Smithay organization and only the C libraries named above. Every other part of those records stands, including what each client is offered, the attacks the tests must refuse, and that no code is copied from GPL-3.0 compositors.

Fulcrum's proposals wait for records. Each is built, tested, and passes CrowbarOS's checks, and the code marks each as a proposal:
- the shell's messages on the control channel: the `launcher` event, `launching`, `attention` (#41), `lock`, `unlock`, `locked`, `unlocked` (#44), `unlock_entry` (#46), and the lock screen's namespace, `crowbaros-lock`;
- the unlock authority, `--unlock-socket`, `--unlock-uid`, and `--start-locked`, with Base's password helper in the broker's place (#49, [crowbaros#207](https://github.com/crowbaros/crowbaros/pull/207));
- the layout of displays on the control channel, `outputs` and `configure_output` (#39).

Records still to write before Fulcrum builds more:
- whether the shell captures the screen;
- how Settings sets input devices.

Next without a new record, each with how to start it in [Picking up the work](picking-up.md):
- a desktop check that restarts the compositor during a session and finds the session locked;
- the hardware rows, run by someone at the RTX 4090, Intel, and Surface machines ([how](hardware-runs.md)).

## After the first release

1. **Everyday access.** Work with the shell team on trusted input methods and cross-app screen-reader support. Qualify each with a user completing a real task across two apps.
2. **Window sharing.** Implement a picker the person uses to share a single window. Use a real call to verify target selection, resize, closing the shared window, and revocation.
3. **Optional layouts.** Prototype niri-style scrolling columns and Pinnacle-style dynamic tiling against the same window model. Compare them on a three-app workflow, a transient dialog, keyboard navigation, monitor removal, and a trusted confirmation. Ship the mode that earns its complexity.
4. **Hardware expansion.** Add AMD and hybrid-GPU rows as machines become available. Treat high-refresh frame pacing, VRR, color management, and a Vulkan renderer as separate features, each with a measurable acceptance test.

**First implementation pull request:** the four crates, the codec with its property tests, and code generation for the core protocol. The Smithay code leaves `main` in the same pull request.
