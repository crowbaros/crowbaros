# Fulcrum

Fulcrum is the CrowbarOS Wayland compositor, written from the ground up in
Rust. It is part of CrowbarOS Base and part of what CrowbarOS trusts. It builds
one program, `crowbar-compositor`.

[The plan](docs/PLAN.md) says how it is built. Fulcrum writes its own wire
protocol, event loop, display, input, rendering, and window-management code.
[Smithay](https://github.com/Smithay/smithay) is the reference, not a
dependency, and nothing from the Smithay organization is in the build
([CrowbarOS ADR 0028](https://github.com/crowbaros/crowbaros/blob/main/docs/adr/0028-the-compositor-is-written-from-the-ground-up.md)).

CrowbarOS decides what the compositor must do. Its decision records stay in
the CrowbarOS repository:

| Decision | Record |
| --- | --- |
| CrowbarOS owns its compositor | [ADR 0015](https://github.com/crowbaros/crowbaros/blob/main/docs/adr/0015-our-own-compositor-on-smithay.md) |
| Who its clients are, and how each connects | [ADR 0016](https://github.com/crowbaros/crowbaros/blob/main/docs/adr/0016-the-warden-starts-every-compartment.md) |
| How an ask is shown and answered | [ADR 0018](https://github.com/crowbaros/crowbaros/blob/main/docs/adr/0018-one-gesture-answers-one-ask.md) |
| What desktop it is, what each client is offered, and its slices | [ADR 0020](https://github.com/crowbaros/crowbaros/blob/main/docs/adr/0020-the-first-compositor-is-small-on-purpose.md) |
| Which rows of that table the shell and the Warden are offered | [ADR 0023](https://github.com/crowbaros/crowbaros/blob/main/docs/adr/0023-the-shell-is-offered-a-compartments-protocols-and-layer-shell.md) |
| It is built in its own repository, and Base pins one commit | [ADR 0027](https://github.com/crowbaros/crowbaros/blob/main/docs/adr/0027-the-compositor-is-built-in-its-own-repository.md) |
| It is written from the ground up, with Smithay as its reference | [ADR 0028](https://github.com/crowbaros/crowbaros/blob/main/docs/adr/0028-the-compositor-is-written-from-the-ground-up.md) |

## Where it stands

Milestones 1 and 2 are done: the Wayland core
([record](docs/wayland-core-2026-11-17.md)) and first light
([record](docs/first-light-2026-11-17.md)). Fulcrum shows windows on a virtual
terminal, and Void's own `foot` and `wayland-info` run on it. What is built:

- **`fulcrum-wire`**, the wire protocol: the codec, connections that pass
  descriptors beside the bytes, and Rust code generated from the protocol XML.
  Every queue is bounded, and nothing a peer sends can make it panic.
- **The server core**: an event loop on `epoll`; the two trusted sockets, each
  checking the peer's user ID; compartment listeners the Warden registers; and
  the registry, which advertises and binds a global only if the policy table
  offers it (ADR 0020). A client that breaks the protocol gets
  `wl_display.error` and is disconnected, and everyone else is served.
- **Surfaces and shared memory**, with double-buffered state, held commits for
  synchronized subsurfaces, `wp_viewporter`'s crop and scale, and pools read
  through `fulcrum-sys`'s `SIGBUS`-guarded mapping.
- **xdg-shell windows and popups**: the configure handshake, window geometry,
  positioners, `wl_surface.enter` and `leave`, and the protocol's errors. A
  new window opens inside the usable area of the output the pointer is on,
  offset from the last, and does not take the keyboard.
- **Redraw on change**: an output is drawn only when something it shows
  changed, then waits for the display, so a still desktop draws nothing and a
  busy client is held to the display's rate. Frame callbacks are answered
  after each draw.
- **Layer shell** for the shell's bar, dock, launcher, and wallpaper: layers
  placed by anchors and margins, exclusive zones that make the usable area
  where windows open, background and bottom layers below the windows and top
  and overlay layers above, popups of layers, and a layer closed when its
  output goes.
- **The seat**: one keyboard and one pointer. The keymap is compiled by
  libxkbcommon and shared through a sealed memfd. The keyboard goes to the
  window or key-taking layer the person clicks, and a click raises and
  activates a window; a new window never takes it. The pointer follows what
  is under it, by input region, and stays with its surface while a button is
  held.
- **The data device**, with nothing moving between clients until slice 4
  (Milestone 6): a selection is offered to no other client, and a drag is
  cancelled as it starts. `foot` will not start without it.
- **Budgets** (CrowbarOS ADR 0029): one compartment cannot take the
  compositor's descriptors from the others, and one client cannot make it
  hold pixels without end.
- **The TTY backend**: the seat from seatd, through Fulcrum's own client of
  its socket; displays through the kernel's atomic mode setting, drawn by the
  CPU renderer into dumb buffers; input through libinput; and monitors and
  input devices plugged in and out, heard from the kernel's netlink uevents.
  Ctrl+Alt+F1 to F12 switch virtual terminals.
- **The nested backend**, for development: the desktop in a window on a host
  session. Only in a build with `--features nested`, which Base never is.
- **A headless backend** that draws each output with the CPU renderer, and
  `crowbar-probe`, the test client, on `fulcrum-wire`'s client side.

Milestone 3 brings the GLES renderer, GBM buffers for the display, damage
tracking, and linux-dmabuf. Until then `zwp_linux_dmabuf_v1` is created for no
one, though the table's rows say who will be offered it.

The Smithay-based compositor that Milestone 0 brought here is at commit
`d11400f`, tagged `smithay-baseline`. Its 57 tests and CrowbarOS's Void guest
check were the specification this one passed before CrowbarOS moved its pin
to it.

## Layout

| Path | Contents |
| --- | --- |
| `wire/` | `fulcrum-wire`: `codec.rs` (the wire format), `socket.rs` (one end of a connection), and `protocol/` (generated; do not edit) |
| `codegen/` | `fulcrum-codegen`: writes `wire/src/protocol/` from `protocols/`. A development tool, never part of Base |
| `protocols/` | The protocol XML Fulcrum speaks, each file with its own license. [Where each came from](protocols/README.md) |
| `src/event_loop.rs` | One thread on `epoll`, with timers in the loop and a waker for other threads |
| `src/connections.rs` | Every way a client can connect: the two trusted sockets, and a compartment's listener |
| `src/identity.rs` | Who a client is |
| `src/policy.rs` | The one table that says which client is offered which global |
| `src/globals.rs` | Globals and the registry |
| `src/client.rs`, `src/state.rs` | Each client's objects, and how each request reaches the code that carries it out |
| `src/surface.rs` | Surfaces, regions, and subsurfaces: double-buffered state, held commits, stacking |
| `src/shm.rs` | Pools and buffers in a client's shared memory |
| `src/image.rs`, `src/geometry.rs` | The CPU renderer, rectangles, and regions |
| `src/limits.rs` | What one client, and one compartment, may make the compositor hold |
| `src/xdg.rs`, `src/windows.rs` | xdg-shell, and where windows go and what each output shows |
| `src/layer.rs` | The shell's layer surfaces and the room they reserve |
| `src/seat.rs` | The keyboard and the pointer, and who has them |
| `src/data_device.rs` | The data device, which moves nothing between clients until slice 4 |
| `src/viewporter.rs` | `wp_viewporter`: a surface's crop and scale |
| `src/redraw.rs` | Drawing only on change |
| `src/backend/` | Where frames go: `tty.rs` (a virtual terminal), `session.rs` (Fulcrum's client of seatd), `uevent.rs` (hotplug from netlink), `nested.rs` (a window on a host session), and `headless.rs` (the tests) |
| `sys/` | `fulcrum-sys`: the only crate with `unsafe` code. Every binding to the kernel and to C goes here: `drm.rs` (mode setting and dumb buffers, by ioctl), `libinput.rs`, `xkb.rs` (libxkbcommon), and `shm.rs` (the guarded mapping) |
| `src/log.rs` | One line per event to standard error, at the level `FULCRUM_LOG` sets |
| `probe/` | `crowbar-probe`: test clients and the stand-in for the Warden. Never part of Base |
| `tests/` | Tests that run the compositor in-process against real client connections. `table.rs`, `connections.rs`, `attacks.rs`, and `descriptors.rs` are ported from the baseline; `protocol.rs` covers what the compositor refuses; `surfaces.rs` checks pixels, held commits, and the shared-memory attacks; `exhaustion.rs` checks the budgets; `redraw.rs` is the baseline's, ported; `windows.rs` covers xdg-shell; `layers.rs` covers layer shell; `keyboard.rs` is the baseline's, unchanged; `seat.rs` covers the pointer and activation; `outputs.rs` covers outputs that change size; `data_device.rs` and `viewporter.rs` cover those protocols; `nested.rs` runs Fulcrum nested in Fulcrum; `linked.rs` reads the built binary for what it links; `dependencies.rs` fails if a crate from the Smithay organization enters `Cargo.lock` |
| `docs/` | [The plan](docs/PLAN.md), and dated records of what was run |

Only `sys/` may hold `unsafe` code. Every other crate forbids it.

## Run

All four flags are required. There is no default for any of them. On a
virtual terminal, with seatd running and the compositor's user in its group:

```sh
crowbar-compositor \
    --shell-socket /run/crowbar/compositor-shell/wayland --shell-uid 1001 \
    --warden-socket /run/crowbar/compositor-warden/wayland --warden-uid 0
```

For development, a build with the nested feature runs in a window on the
session it is started from:

```sh
cargo run --features nested -- --backend nested \
    --shell-socket /tmp/fulcrum/shell --shell-uid "$(id -u)" \
    --warden-socket /tmp/fulcrum/warden --warden-uid "$(id -u)"
```

A compartment's client reaches it through a listener the Warden registers.
`crowbar-probe warden` stands in for the Warden:

```sh
WAYLAND_DISPLAY=/path/to/warden-socket \
    crowbar-probe warden org.example.App compartment-1 /tmp/c1 wayland-info
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

This repository began as CrowbarOS's `compositor/` directory at commit
[`30fadd8`](https://github.com/crowbaros/crowbaros/tree/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/compositor),
with the seven commits that touched it (tag `import/crowbaros-30fadd8`). The
[Milestone 0 record](docs/independent-build-2026-11-17.md) shows CrowbarOS
building it as a pinned commit. The Smithay-based code left `main` when this
rewrite began, and it is at the `smithay-baseline` tag.

Smithay and Otto are MIT, so their code may be adapted, with attribution in
[`LICENSE-smithay.txt`](LICENSE-smithay.txt) or a file like it. niri, COSMIC,
Pinnacle, and Jay are GPL-3.0: read them for ideas, and copy nothing.
