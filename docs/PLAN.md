# Fulcrum

## A Wayland compositor for CrowbarOS, written from the ground up

Fulcrum is the CrowbarOS Wayland compositor, written from the ground up in Rust. Fulcrum writes every layer of compositor code itself: the Wayland wire protocol, the event loop, display and input handling, the renderer, and window management. [Smithay](https://github.com/Smithay/smithay) is the reference. Fulcrum studies how Smithay solves each problem, may adapt its MIT code with attribution, and takes proven ideas from the compositors built on it. Fulcrum depends on no crate from the Smithay organization: not `smithay`, `wayland-rs`, `drm-rs`, `gbm.rs`, `input.rs`, `calloop`, or `libseat-rs`. The only C libraries it links are the ones a GPU desktop cannot avoid: Mesa's EGL, GLES, and GBM; libinput; and libxkbcommon.

Its first customer is the CrowbarOS Base image. A successful release lets a person boot CrowbarOS, launch and arrange applications, use multiple displays, move data deliberately between apps, and lock and recover the session. The GPUI shell supplies the bar, dock, launcher, and Settings UI. Fulcrum owns the pixels and input decisions those components must be able to trust.

**Where it starts.** [Milestone 0](independent-build-2026-11-17.md) moved CrowbarOS's Smithay-based compositor into this repository and made CrowbarOS build a pinned commit. That compositor, at `d11400f` and tag `smithay-baseline`, is now the behavioral specification. Its 57 tests and the 36 checks of CrowbarOS's Void guest describe what the new compositor must do before CrowbarOS moves its pin. Fulcrum's `main` is replaced from Milestone 1 on. CrowbarOS stays pinned to `d11400f` until Milestone 2 passes the same checks.

## What Fulcrum writes, and what it learns from

| Layer | Fulcrum writes | Fulcrum uses | Smithay's counterpart to study |
| --- | --- | --- | --- |
| Wire protocol | The message codec, descriptor passing, object IDs, and per-client limits. Rust code generated from the protocol XML at build time, for the server and the client side | Unix sockets and `SCM_RIGHTS` through `rustix` | `wayland-backend`, `wayland-scanner` |
| Event loop | One thread on `epoll`, with timers and signals as descriptors | `rustix` | `calloop` |
| Protocol state | Every global: double-buffered surface state, subsurface sync, buffer release, frame callbacks, the seat and focus | — | `smithay::wayland` |
| Shared memory | Pool mappings that a client truncating its file cannot turn into a crash | `rustix` `mmap` | `smithay::wayland::shm` |
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
| `probe/` (`crowbar-probe`) | Test clients and the stand-ins for the Warden, the shell, and the broker, on `fulcrum-wire`'s client side. Never part of Base |

`protocols/` holds the vendored XML for every protocol Fulcrum speaks, with each protocol's license. The binary stays `crowbar-compositor` until CrowbarOS changes its services in a pull request of their own.

### References

Read each reference to learn how it solves a problem. A project's license decides what may be taken.

| Reference | License | What to take |
| --- | --- | --- |
| [Smithay](https://github.com/Smithay/smithay) and its Anvil and Smallvil examples | MIT | The design of each layer listed above. Code may be adapted with attribution in `LICENSE-smithay.txt`. The cached surface state, the SIGBUS handling for shared memory, the damage tracker, and the DRM compositor's plane assignment are where its experience is densest |
| [Jay](https://github.com/mahkoh/jay) | GPL-3.0 | Not on Smithay's list. It shows that this approach works in Rust: its own wire code generated from XML, its own DRM, GBM, and graphics code, and its own event loop. Ideas only |
| [niri](https://github.com/YaLTeR/niri) | GPL-3.0 | Frame clock and presentation timing, deliberate window placement when a monitor goes away and comes back, and randomized layout tests. Its scrolling columns are a post-Beta layout candidate. Ideas only |
| [COSMIC](https://github.com/pop-os/cosmic-comp) | GPL-3.0 | The compositor, shell, and settings each have one owner and restart independently, yet ship as one desktop. Ideas only |
| [Pinnacle](https://github.com/Ottatop/pinnacle) | GPL-3.0 | Layout as a policy separate from the window model, with configuration kept outside the trusted process. Ideas only |
| [Otto](https://github.com/nongio/otto) | MIT | Placement, snapping, and keyboard switching that feel like one interaction in a stacking desktop. Code may be adapted with attribution |

## Security comes with the protocol code

Owning the wire layer lets Fulcrum enforce the CrowbarOS decisions where messages enter, not on top of someone else's dispatch.

- **One registry, with the policy table** ([ADR 0020](https://github.com/crowbaros/crowbaros/blob/main/docs/adr/0020-the-first-compositor-is-small-on-purpose.md)). A global a client is not offered is never advertised, and binding it by number is a protocol error. A global with no row is offered to nobody.
- **Identity at accept** ([ADR 0016](https://github.com/crowbaros/crowbaros/blob/main/docs/adr/0016-the-warden-starts-every-compartment.md)). It works as in the baseline: two trusted path sockets checked by peer user ID, and listeners the Warden registers for compartments.
- **Limits for each client and each compartment.** Fulcrum caps objects, descriptors, and shared-memory bytes per client, and descriptors per compartment. A client that passes its cap is disconnected, and every other client keeps being served. This closes [issue 174](https://github.com/crowbaros/crowbaros/issues/174) at its root, before CrowbarOS cutover.
- **Malformed input is a disconnect, never a panic.** Handlers return errors and do not unwrap client data. The codec gets property tests over random and truncated messages.
- **A test guards what is linked.** As in the baseline, the build fails if the binary gains X11, libei, or code that opens a public socket. It also fails if a crate from the Smithay organization appears in `Cargo.lock`.

## Implementation milestones

| Milestone | Deliverable | Exit check |
| --- | --- | --- |
| **0 · Independent build** (done) | The Smithay-based compositor in its own repository, built by CrowbarOS at a pinned commit. [Record](independent-build-2026-11-17.md) | Met: 36 of 36 guest checks, byte-identical binaries on both sides |
| **1 · Wayland core** | Replace `main`. The four crates; `fulcrum-wire` with generated code for the core protocol, xdg-shell, layer shell, the security context, and viewporter; the event loop; the trusted sockets and identity; the registry and policy table; `wl_compositor`, `wl_surface`, `wl_region`, `wl_subcompositor`, `wl_shm`, `wl_output`, and frame callbacks; per-client limits; a headless backend with the CPU renderer; and the probe on `fulcrum-wire` | Ported from the baseline: the connection, table, attack, descriptor, exhaustion, and linked-code tests. Codec property tests pass. A client's shared-memory pixels appear in the CPU renderer's output |
| **2 · First light** | xdg-shell, layer shell, and the seat, with libxkbcommon keymaps and the pointer. Redraw on change with damage tracking. The GLES renderer, KMS atomic modesetting with GBM, the seatd client, libinput, uevent hotplug, and the nested backend | All 57 baseline tests ported and passing. The Void guest check passes 36 of 36 against the new binary, and Void's own `foot` and `wayland-info` run on it. **CrowbarOS then moves its pin to the new compositor** |
| **3 · Buffers and timing** | linux-dmabuf through GBM and EGL, explicit sync, presentation time, fractional scale, cursor images clipped so they cover no frame, and scanout planes | The GPUI shell draws on Fulcrum in the Void guest and on the RTX 4090 and Intel machines. A still desktop still draws no frames |
| **4 · Trusted shell session** | Slice 2, [issue 71](https://github.com/crowbaros/crowbaros/issues/71): the shell and broker control channels, asks and arming, the confirm chord, the restart notice, and the broker's GPU-need declaration. The local branch `wip/slice-2-on-smithay` holds a first wire format and the rules, ready to port | ADR 0018's tests 1 to 21 that fall to the compositor. One physical confirmation answers exactly one shown ask. A changed ask or a shell restart before the chord cannot be confirmed |
| **5 · Working window manager** | Slice 3, [issue 78](https://github.com/crowbaros/crowbaros/issues/78): trusted frames and stacking, click and keyboard focus, move, resize, maximize, fullscreen, half-screen snap, and nine workspaces shared across displays | Launch two real apps from the shell, arrange them by mouse and keyboard, open a child dialog, switch workspaces, and return. Focus follows the person's action. A fullscreen app keeps its trusted frame |
| **6 · Clipboard and capture** | Slice 4, [issue 166](https://github.com/crowbaros/crowbaros/issues/166): clipboard, primary selection, and drag and drop tied to the person's act and reported to the broker, and shell-owned capture | Keyboard paste, middle click, menu paste, and drop each work between compartments. A focus change alone transfers nothing. The broker records source, destination, and act |
| **7 · Displays, input, and hardware** | Slice 5, [issue 79](https://github.com/crowbaros/crowbaros/issues/79): saved modes, positions, and scale per display, hotplug, input devices, and suspend and resume | Results on the Void guest, the RTX 4090, the Intel iGPU, and the Surface ARM target, including mixed scale, display removal, frames after resume, and missed presentation deadlines measured against each display's refresh |
| **8 · Lock and recovery** | Slice 6, [issue 87](https://github.com/crowbaros/crowbaros/issues/87): start locked, input to the lock screen alone, blank secondary displays, and broker-authorized unlock, with idle, suspend, and service restart | Restart the shell during an ask, restart the compositor during a session, and suspend with a second monitor attached. Each reaches its documented state before an app receives input |
| **9 · CrowbarOS cutover** | The Alpha desktop runs on Fulcrum with the production interfaces, and the spike is deleted | The full desktop smoke check passes, the real Warden and broker satisfy the contracts the stand-ins exercised, and the previous Base generation still boots |

Milestones 1 and 2 are where this approach costs the most. They rebuild what Smithay gave the baseline for free before any new behavior lands. The cost is bounded by building only what ADR 0020's table offers: no XWayland, one seat, and GLES as the only GPU renderer until a measurement asks for more.

## Decisions this needs in CrowbarOS

[ADR 0015](https://github.com/crowbaros/crowbaros/blob/main/docs/adr/0015-our-own-compositor-on-smithay.md) says the compositor is built on Smithay, and ADR 0020 sets rules for bumping a Smithay pin. A new CrowbarOS record, ADR 0028, replaces both: Fulcrum is written from the ground up, with no crate from the Smithay organization and only the C libraries named above. Every other part of those records stands, including what each client is offered, the attacks the tests must refuse, and that no code is copied from GPL-3.0 compositors.

## After the first release

1. **Everyday access.** Work with the shell team on trusted input methods and cross-app screen-reader support. Qualify each with a user completing a real task across two compartments.
2. **Window sharing.** Implement a broker-mediated picker for a single window. Use a real call to verify target selection, resize, closing the shared window, and revocation.
3. **Optional layouts.** Prototype niri-style scrolling columns and Pinnacle-style dynamic tiling against the same window model. Compare them on a three-app workflow, a transient dialog, keyboard navigation, monitor removal, and a trusted confirmation. Ship the mode that earns its complexity.
4. **Hardware expansion.** Add AMD and hybrid-GPU rows as machines become available. Treat high-refresh frame pacing, VRR, color management, and a Vulkan renderer as separate features, each with a measurable acceptance test.

**First implementation pull request:** the four crates, the codec with its property tests, and code generation for the core protocol. The Smithay code leaves `main` in the same pull request.
