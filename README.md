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

Milestone 1, the Wayland core, is under way. What is built:

- **`fulcrum-wire`**, the wire protocol. It holds the codec for headers and
  arguments, and one end of a connection that passes descriptors beside the
  bytes. It also holds Rust code for every request and event of the protocols
  in `protocols/`, generated from their XML. Every queue is bounded, and
  nothing a peer sends can make it panic.
- **`fulcrum-codegen`**, which writes that code. It reads the XML with a
  parser of its own, so it adds no crate.

`crowbar-compositor` serves no clients yet, and refuses to start. CrowbarOS
still builds the Smithay-based compositor that Milestone 0 brought here. That
is commit `d11400f`, tagged `smithay-baseline`. Its 57 tests and the 36
checks of CrowbarOS's Void guest are the specification the new compositor
must pass before CrowbarOS moves its pin.

## Layout

| Path | Contents |
| --- | --- |
| `wire/` | `fulcrum-wire`: `codec.rs` (the wire format), `socket.rs` (one end of a connection), and `protocol/` (generated; do not edit) |
| `codegen/` | `fulcrum-codegen`: writes `wire/src/protocol/` from `protocols/`. A development tool, never part of Base |
| `protocols/` | The protocol XML Fulcrum speaks, each file with its own license. [Where each came from](protocols/README.md) |
| `src/` | `crowbar-compositor` |
| `tests/dependencies.rs` | Fails if a crate from the Smithay organization enters `Cargo.lock` |
| `docs/` | [The plan](docs/PLAN.md), and dated records of what was run |

Only `sys/`, when it comes, may hold `unsafe` code. Every other crate forbids
it.

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
