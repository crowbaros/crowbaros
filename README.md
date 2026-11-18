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

Milestone 1, the Wayland core, is done ([record](docs/wayland-core-2026-11-17.md)).
Milestone 2, first light, is under way. What is built:

- **`fulcrum-wire`**, the wire protocol: the codec, connections that pass
  descriptors beside the bytes, and Rust code generated from the protocol XML.
  Every queue is bounded, and nothing a peer sends can make it panic.
- **The server core**: an event loop on `epoll`; the two trusted sockets, each
  checking the peer's user ID; compartment listeners the Warden registers; and
  the registry, which advertises and binds a global only if the policy table
  offers it (ADR 0020). A client that breaks the protocol gets
  `wl_display.error` and is disconnected, and everyone else is served.
- **Surfaces and shared memory**, with double-buffered state, held commits for
  synchronized subsurfaces, and pools read through `fulcrum-sys`'s
  `SIGBUS`-guarded mapping.
- **xdg-shell windows and popups**: the configure handshake, window geometry,
  positioners, `wl_surface.enter` and `leave`, and the protocol's errors. A
  new window opens inside the usable area of the output the pointer is on,
  offset from the last, and does not take the keyboard.
- **Redraw on change**: an output is drawn only when something it shows
  changed, then waits one frame time, so a still desktop draws nothing and a
  busy client is held to the display's rate. Frame callbacks are answered
  after each draw.
- **Budgets** (CrowbarOS ADR 0029): one compartment cannot take the
  compositor's descriptors from the others.
- **A headless backend** that draws each output with the CPU renderer, and
  `crowbar-probe`, the test client, on `fulcrum-wire`'s client side.

Still to come in Milestone 2: layer shell, the seat (keyboard and pointer),
the GLES renderer, KMS, the seatd client, libinput, hotplug, and a nested
backend. `wl_seat`, `wp_viewporter`, `zwlr_layer_shell_v1`, and
`zwp_linux_dmabuf_v1` are advertised to the clients the table names, and a
request on one is a protocol error that says this build does not carry it
out yet.

CrowbarOS still builds the Smithay-based compositor that Milestone 0 brought
here: commit `d11400f`, tagged `smithay-baseline`. Its 57 tests and the 36
checks of CrowbarOS's Void guest are the specification the new compositor
must pass before CrowbarOS moves its pin.

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
| `src/redraw.rs`, `src/backend/` | Drawing only on change, and where frames go: the headless backend so far |
| `sys/` | `fulcrum-sys`: the only crate with `unsafe` code. Every binding to the kernel and to C goes here |
| `src/log.rs` | One line per event to standard error, at the level `FULCRUM_LOG` sets |
| `probe/` | `crowbar-probe`: test clients and the stand-in for the Warden. Never part of Base |
| `tests/` | Tests that run the compositor in-process against real client connections. `table.rs`, `connections.rs`, `attacks.rs`, and `descriptors.rs` are ported from the baseline; `protocol.rs` covers what the compositor refuses; `surfaces.rs` checks pixels, held commits, and the shared-memory attacks; `exhaustion.rs` checks the budgets; `redraw.rs` is the baseline's, ported; `windows.rs` covers xdg-shell; `linked.rs` reads the built binary for what it links; `dependencies.rs` fails if a crate from the Smithay organization enters `Cargo.lock` |
| `docs/` | [The plan](docs/PLAN.md), and dated records of what was run |

Only `sys/` may hold `unsafe` code. Every other crate forbids it.

## Run

All four flags are required. There is no default for any of them.

```sh
crowbar-compositor \
    --shell-socket /run/crowbar/compositor-shell/wayland --shell-uid 1001 \
    --warden-socket /run/crowbar/compositor-warden/wayland --warden-uid 0
```

A compartment's client reaches it through a listener the Warden registers.
`crowbar-probe warden` stands in for the Warden:

```sh
WAYLAND_DISPLAY=/path/to/warden-socket \
    crowbar-probe warden org.example.App compartment-1 /tmp/c1 wayland-info
```

## Build and test

Nothing here needs a system library yet.

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
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
