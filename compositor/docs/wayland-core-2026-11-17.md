# Fulcrum serves Wayland clients on code of its own

**Date:** November 17, 2026. **Plan milestone:** [1 · Wayland core](PLAN.md#implementation-milestones).

Milestone 1 rebuilt the bottom of the compositor without Smithay. Fulcrum now
accepts its clients the two ways ADR 0016 allows, offers each its row of
ADR 0020's table, and takes surfaces and shared-memory buffers. A client's
pixels are what its CPU renderer draws. It shows nothing on a display yet;
that is Milestone 2.

| Pull request | What it built |
| --- | --- |
| [#3](https://github.com/crowbaros/fulcrum/pull/3) | `fulcrum-wire`: the codec, connections, and code generated from the protocol XML. The Smithay code left `main` |
| [#4](https://github.com/crowbaros/fulcrum/pull/4) | The event loop, the trusted sockets and compartment listeners, the registry behind the policy table, and `crowbar-probe` on `fulcrum-wire` |
| [#5](https://github.com/crowbaros/fulcrum/pull/5) | Surfaces, regions, subsurfaces, and shared memory; `fulcrum-sys` with the SIGBUS-guarded mapping; the CPU renderer |
| This one | Budgets for each client and each compartment (CrowbarOS ADR 0029, issue 174), and the guard on what is linked |

## The exit check

| Plan's check | Result |
| --- | --- |
| Ported from the baseline: the connection, table, attack, descriptor, exhaustion, and linked-code tests | Ported, and passing. `table.rs`, `connections.rs`, `attacks.rs`, and `descriptors.rs` keep the baseline's cases. `exhaustion.rs` keeps the baseline's two and adds the four of issue 174. `linked.rs` guards the same promises against Fulcrum's own binary: only the libraries ADR 0028 allows, no Smithay or wayland-rs code, and no lookup of where a public socket would go |
| Codec property tests pass | `wire/tests/protocol.rs` covers every message of all 37 interfaces in both directions: random valid messages round-trip byte for byte, and random bodies, truncations, and byte streams never panic |
| A client's shared-memory pixels appear in the CPU renderer's output | `tests/surfaces.rs`: a red buffer is drawn red, a 50% green subsurface blends to `0xff008000` over black, and held and released commits behave as the protocol says |

`cargo test --locked` passes 113 tests. `Cargo.lock` holds 14 crates, none from
the Smithay organization.

## Checked against libwayland

The built binary ran with a compartment that `crowbar-probe warden` registered,
and `wl-paste`, a libwayland client, connected through it with
`WAYLAND_DEBUG=1`. libwayland decoded every message Fulcrum sent: the
compartment's row of globals, `delete_id`, `wl_callback.done`, and the binds it
asked for. That was before #5, when surfaces were not yet carried out.

## Not done

- **The Void guest check.** The compositor has no display backend until
  Milestone 2, so there is nothing for the guest to show.
- **Roles.** No surface has a role that puts it on an output. `xdg-shell` and
  layer shell are Milestone 2.
