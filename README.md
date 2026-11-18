# Fulcrum

Fulcrum is the CrowbarOS Wayland compositor, written in Rust on
[Smithay](https://github.com/Smithay/smithay). It is part of CrowbarOS Base
and part of what CrowbarOS trusts. It builds one program, `crowbar-compositor`,
and a test client, `crowbar-probe`. [The plan](docs/PLAN.md) says how it grows
into the compositor that runs the CrowbarOS desktop.

CrowbarOS decides what the compositor must do. Fulcrum builds it. Its decision
records stay in the CrowbarOS repository:

| Decision | Record |
| --- | --- |
| CrowbarOS owns its compositor | [ADR 0015](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/docs/adr/0015-our-own-compositor-on-smithay.md) |
| Who its clients are, and how each connects | [ADR 0016](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/docs/adr/0016-the-warden-starts-every-compartment.md) |
| What desktop it is, what each client is offered, and its slices | [ADR 0020](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/docs/adr/0020-the-first-compositor-is-small-on-purpose.md) |
| Which rows of that table the shell and the Warden are offered | [ADR 0023](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/docs/adr/0023-the-shell-is-offered-a-compartments-protocols-and-layer-shell.md) |
| It is built beside Milestone 2 | [ADR 0022](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/docs/adr/0022-the-compositor-is-built-beside-milestone-2.md) |

The [spike](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/lab/compositor-spike/README.md)
that tested ADR 0015 is not this code. It stays in CrowbarOS's
`lab/compositor-spike/` because the Alpha desktop check still runs it. It is
deleted when this compositor passes that check.

## Where it came from

This repository began as CrowbarOS's `compositor/` directory at commit
[`30fadd8`](https://github.com/crowbaros/crowbaros/tree/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/compositor).
`git subtree split` kept the seven commits that touched it, with their authors
and dates. The tag `import/crowbaros-30fadd8` marks the last of them. Its tree
is byte for byte that directory. Every later change is made here.

CrowbarOS builds one Fulcrum revision at a time. The full commit ID is pinned
in CrowbarOS's `compositor/fulcrum.json`, and changing the compositor in Base
means changing that pin.

## What is built

ADR 0020 builds the compositor in six slices. This is slice 1
([issue 49](https://github.com/crowbaros/crowbaros/issues/49)).

| Slice | Issue | State |
| --- | --- | --- |
| 1. A clean skeleton, client identity, the protocol table, redraw on change only, and the guard on what is linked | [49](https://github.com/crowbaros/crowbaros/issues/49) | Built |
| 2. The shell protocol, arming an ask, the chord, and the restart notice | [71](https://github.com/crowbaros/crowbaros/issues/71) | Not started |
| 3. Frames, what is drawn above what, focus, floating windows, workspaces, and the compositor's keys | [78](https://github.com/crowbaros/crowbaros/issues/78) | Not started |
| 4. The clipboard and capture | [166](https://github.com/crowbaros/crowbaros/issues/166) | Not started |
| 5. Displays, scale, input devices, explicit sync, and the hardware rows | [79](https://github.com/crowbaros/crowbaros/issues/79) | Not started |
| 6. Lock and recovery from failure | [87](https://github.com/crowbaros/crowbaros/issues/87) | Not started |

So this compositor does not run the desktop yet. It shows windows and the
shell's layer surfaces, and it gives a window the keyboard on a click. These
are not built, and each waits for its slice:

- **The confirm chord.** Ctrl+Alt+Enter reaches the focused client like any
  other key until slice 2.
- **Frames, moving and resizing a window, workspaces, and the compositor's
  keys.** A new window never takes the keyboard. Slice 3 adds the two cases in
  which it does.
- **The clipboard.** The data device global does not exist, so nothing can be
  copied or pasted.
- **Display settings.** Every output runs in the mode its monitor prefers, at
  scale 1.
- **Clients' cursor images.** On a TTY the pointer is a white square.
- **A limit on what one compartment can make the compositor hold.** Every
  connection and every buffer costs the compositor a descriptor, and nothing
  caps them. A compartment that uses them all up does not end the compositor.
  But until the Warden stops that compartment, the compositor can accept no
  new client, and a client that sends it a descriptor, as for a buffer, can
  lose its connection
  ([issue 174](https://github.com/crowbaros/crowbaros/issues/174)).

## How a client connects

There are two ways, and no third. The compositor has no public socket, and no
flag or build option that opens one.

| Client | How it connects | How the compositor knows who it is |
| --- | --- | --- |
| The shell | A path socket of its own | The kernel reports the shell's user ID for the peer |
| The Warden | A path socket of its own | The kernel reports the Warden's user ID for the peer |
| Everything else | A listener that the Warden registers for one compartment, and binds into that compartment's sandbox alone | The Warden gave the listener a principal ID and a compartment ID. Every client of that listener carries them |

- Base makes the directory of each trusted socket, and decides who can enter
  it. The compositor's check is the peer's user ID.
- The Warden registers a listener through `wp_security_context_manager_v1`,
  with the principal ID as the app ID and the compartment ID as the instance
  ID. A listener with either one missing accepts nothing.
- Nothing a client says about itself changes who it is.
- The compositor starts no process, and opens every descriptor close-on-exec.
- A listener that cannot accept a connection is left alone for a second and
  then tried again. It does not end the compositor.

The Warden is a library so far, and not yet a service. The tests and the
checks use a stand-in: a client of the Warden's socket that registers
listeners (`crowbar-probe warden`).

## What each client is offered

`src/policy.rs` is ADR 0020's table "What each client is offered", with what
ADR 0023 adds for the shell, and the one place that decides it. Every global passes through it, so a global with no row
is offered to nobody. That includes one a later Smithay adds.

The table decides who is offered a global. It does not create one. Slice 1
creates the globals a client needs to show a window. Each of the others in
the table's rows is created with the slice that builds what it does:

| Global | Waits for |
| --- | --- |
| The data device | Slice 4, with the clipboard rules it is offered under |
| Cursor shape | Slice 3, which draws cursors |
| Fractional scale, presentation time, explicit sync | Slice 5 |
| Idle inhibit | Slice 6, which decides when it is honoured |

Today a client sees these:

| Client | Globals |
| --- | --- |
| Every compartment with a listener | `wl_compositor`, `wl_subcompositor`, `wl_shm`, `wl_seat`, `wl_output`, `xdg_wm_base`, `wp_viewporter` |
| A compartment whose needs include the GPU | Those, and `zwp_linux_dmabuf_v1` |
| The shell | Those, `zwp_linux_dmabuf_v1`, and `zwlr_layer_shell_v1` |
| The Warden | `wp_security_context_manager_v1` and nothing else |

**Nothing tells the compositor yet that a compartment's needs include the
GPU.** The Warden says who a compartment is and nothing more (ADR 0016). The
broker knows each principal's needs, and
[issue 71](https://github.com/crowbaros/crowbaros/issues/71) decides how it
tells the compositor. Until then no compartment is offered
`zwp_linux_dmabuf_v1` outside the tests, which say it in the broker's place
(`Compartment::record_gpu_need`).

## Drawing

The compositor draws an output only after something on it changed. A still
desktop draws no frames and wakes on no timer. `src/redraw.rs` says how, and
how a client that commits on every frame is held to the display's rate.

## Layout

This is a Cargo workspace with a pinned `Cargo.lock`. It pins Smithay to a git
commit and needs system libraries (DRM, libinput, libseat, udev).
`rustfmt.toml` holds the line width CrowbarOS formats Rust to, so code formats
the same here as it did there.

| Path | Contents |
| --- | --- |
| `docs/` | [The plan](docs/PLAN.md), and dated records of what was run |
| `src/identity.rs` | Who a client is |
| `src/connections.rs` | Every way a client can connect: the two trusted sockets, and a compartment's listener |
| `src/policy.rs` | The one table that says which client is offered which Wayland global |
| `src/handlers/` | Smithay protocol handlers. `mod.rs` puts every global through `policy.rs` |
| `src/redraw.rs` | Drawing only when something changed |
| `src/input.rs` | Where every key and pointer event enters |
| `src/windows.rs` | Where a new window goes, and which window has the keyboard |
| `src/backend/` | Where frames go: a TTY (`tty.rs`). A build with `--features nested` can also draw into a window on a host session (`winit.rs`) |
| `probe/` | `crowbar-probe`: Wayland clients for looking at the compositor from outside, and the stand-in for the Warden. A crate of its own, and never part of Base |
| `tests/` | Tests that run the compositor in-process, without a display, against real Wayland clients |
| `checks/` | The check that needs a display on this host |
| `.github/workflows/ci.yml` | What CI runs: the commands under [Build and test](#build-and-test) |

`tests/attacks.rs` holds the attacks that the decision records give this
slice: attack 1 of ADR 0020, and attacks 11 and 12 of ADR 0016.
`tests/linked.rs` is the guard on what is linked. `tests/exhaustion.rs` uses
up the compositor's descriptors.

Two things were carried over from the spike's display check and belong to
slice 5: an output that is attached or removed while the compositor runs, and
the move of a window off an output that went away. They are as the spike had
them.

The TTY backend is adapted from Smithay's `anvil` example, and the handlers
began from its `smallvil` example. Both are MIT; see
[`LICENSE-smithay.txt`](LICENSE-smithay.txt).

Do not copy code from niri or cosmic-comp. Both are GPL-3.0, and Fulcrum, like
CrowbarOS, is MIT. Smithay and its example compositors are MIT. The other
compositors [the plan](docs/PLAN.md) learns from are design references: take
their ideas, and check a project's license before taking anything more.

## Build and test

Smithay's default features are off. That leaves out `backend_libei` (input
emulation), `xwayland`, `backend_x11`, and `backend_vulkan`. The `reis` crate
is still built, because Smithay depends on it without a feature gate, so the
list of crates does not show what is left out. `tests/linked.rs` reads the
symbols of the built binary, and fails if it holds any of these:

- libei, XWayland, or X11-backend code;
- X11 client code;
- code that opens a public socket.

The backend that draws into a window on a host session can make that window
on an X11 session, which takes X11 client code. So it is left out unless the
compositor is built with `--features nested`. Base never builds it. It is for
development and for `checks/nested.sh`, and it opens no other way in.

```sh
cargo test --locked                         # no display needed
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --all-targets --locked --features nested -- -D warnings
cargo fmt --all --check
```

## Run

All four of these are required. There is no default for any of them. The
compositor takes a TTY, with a seat from seatd.

```sh
crowbar-compositor \
    --shell-socket /run/crowbar/compositor-shell/wayland --shell-uid 1001 \
    --warden-socket /run/crowbar/compositor-warden/wayland --warden-uid 0
```

The checks that need a display:

```sh
# Nested, in a window on a Wayland session. The log goes to .lab/compositor/.
cargo build --features nested
checks/nested.sh /usr/bin/wayland-info
```

The other check runs the compositor on a TTY in a Void Linux guest, with a seat
from seatd and no systemd. Every pull request that changes the compositor runs
it (ADR 0022). The guest lives in the CrowbarOS lab, so run it from a CrowbarOS
checkout. Commit the change here first. Then point the lab at this repository
and the commit:

```sh
cd ../crowbaros
FULCRUM_REPOSITORY=../fulcrum \
FULCRUM_REVISION=$(git -C ../fulcrum rev-parse HEAD) \
    lab/void/compositor_guest.py check
```

Without the two variables, the lab builds the revision CrowbarOS pins. The
evidence log names the Fulcrum revision it built, and says when it is not the
pinned one. The [Void lab](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/lab/void/README.md#the-graphical-guest)
says what the guest check covers. The run for slice 1, before the move here, is
in the [slice 1 record](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/docs/research/compositor-slice-1-2026-11-15.md).
