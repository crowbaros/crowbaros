# Compositor slice 1: client identity, the protocol table, and drawing only on change

**Result (November 15, 2026): the first slice of the production compositor is
built, and every check that was run passed.** A clean compositor in
[`compositor/`](../../compositor/README.md) knows each client by how it
connected, offers each one only its row of the protocol table, and draws only
when something changed. It does not run the desktop yet. The Alpha desktop
still runs the spike, which moved to
[`lab/compositor-spike/`](../../lab/compositor-spike/README.md).

This is the record for [issue 49](https://github.com/crowbaros/crowbaros/issues/49),
slice 1 of [ADR 0020](../adr/0020-the-first-compositor-is-small-on-purpose.md).
The evidence is in
[`evidence/compositor-slice-1-2026-11-15/`](evidence/compositor-slice-1-2026-11-15/):
the logs the runs wrote, copied byte for byte, and two captures. Times in the
logs are UTC, so they read November 16.

## Summary

| Item in the issue | Result | Evidence |
| --- | --- | --- |
| A clean start, with the spike moved to `lab/compositor-spike/` | Done. The Alpha desktop check builds the spike from there and passes, 37 of 37 | `alpha-desktop.log` |
| Clients connect through a stand-in for the Warden. No public socket, and no flag or build option that opens one | Shown, in tests and in the Void guest under three user IDs | `tests/connections.rs`, `tests/attacks.rs`, `void-guest.log` |
| ADR 0020's table is the table. A global with no row is offered to nobody | Shown for every kind of client | `tests/table.rs`, `tests/attacks.rs`, `nested-protocols.log`, `void-guest.log` |
| Redraw on change only | Shown. In the guest, ten seconds with a window on screen drew no frame, and the compositor's process did not run once | `tests/redraw.rs`, `void-guest.log` |
| A guard on what is linked | Built. It reads the binary's symbols | `tests/linked.rs` |
| Attack 1 of ADR 0020, and attacks 11 and 12 of ADR 0016 | Each has a test, and each is refused | `tests/attacks.rs`, `void-guest.log` |

## What was run

One commit of the compositor did every run: `ac3478f`. The guest check script
changed after it, and that change is in the commit that adds this record.

| Check | Where | Result |
| --- | --- | --- |
| `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all --check` in `compositor/` | Jim's tower, Linux 7.2.15, Rust 1.99.1 | 54 tests pass, with no warnings. The lint also passes with `--features nested` |
| The same three in `lab/compositor-spike/` | The same | 22 tests pass |
| `compositor/checks/nested.sh` | Nested on the tower's Wayland session | Pass |
| `lab/void/compositor_guest.py check` | The Void guest: Linux 6.18.63, runit, seatd, software rendering | 36 of 36. It also runs the 54 tests inside the Void build root |
| `lab/void/compositor_guest.py desktop-check` | A fresh Void guest | 37 of 37, with the spike |
| `lab/void/void_lab.py milestone` | The Void Base guest | 22 of 22 |

The Void guest check's binaries: `crowbar-compositor` is sha256
`b046c951c81c5706ba652888b17566081ca7d290f58fedd5ea21b765805f5112`, and
`crowbar-probe` is `37a275b109317ea9b63513d2e7f45b342f673068fdf8507f1c016330e84bcbdc`.
Both were built in the Void build root.

## Identity

The compositor has two ways in
([ADR 0016](../adr/0016-the-warden-starts-every-compartment.md)).

- **A trusted part's path socket.** The shell and the Warden each have one. The
  compositor accepts a peer only with that part's user ID, as the kernel
  reports it.
- **A compartment's listener.** The Warden registers it through
  `wp_security_context_manager_v1`, with the principal ID as the app ID and the
  compartment ID as the instance ID. Every client of that listener carries
  both.

The guest ran the compositor as `crowbar-compositor`, the shell's client as
`crowbar-shell`, the stand-in for the Warden as root, and the client of the
listener as `crowbar-app`.

| What was tried | What happened |
| --- | --- |
| `crowbar-app` asks each of the 5 sockets on the machine for a Wayland registry | None answers |
| `crowbar-app` is given both trusted sockets, with their directories opened to it on purpose | Refused twice on its user ID. The compositor logged both |
| The Warden's stand-in registers a listener for `org.crowbaros.Probe`, `compartment-1` | The compositor logs both IDs, and a client of the listener is offered the compartment's row |
| A listener with no principal ID or no compartment ID, and a second listener for a live compartment | Each accepts nothing (tests) |
| A client of a listener, and the shell, try to register a listener | Not offered to either |
| The Warden's stand-in exits | The listener accepts nothing |

The compositor started no process, and every descriptor it opened on the TTY
is close-on-exec: 33 were open, with the seat, the DRM device, and the input
devices among them.

## The protocol table

`compositor/src/policy.rs` is ADR 0020's table. Both dispatch `impl` blocks in
`src/handlers/mod.rs` cover every interface, so no global reaches a client
without passing it.

What each client saw, by `wayland-info`, in the guest:

| Client | Globals |
| --- | --- |
| A client of a compartment's listener | `wl_compositor`, `wl_subcompositor`, `wl_shm`, `wl_seat`, `wl_output`, `xdg_wm_base`, `wp_viewporter` |
| The shell | Those, `zwp_linux_dmabuf_v1`, and `zwlr_layer_shell_v1` |
| The Warden | `wp_security_context_manager_v1` |

**Attack 1 of ADR 0020.** The compositor creates no global from the "Nobody"
row, so a test that only looks for them would pass with no table at all. The
test adds one global for each of the 25 names in the row, and for two names
with no row. Each added global refuses nobody itself. Then every kind of
client asks for each one, by its name and by guessing its number. None is
offered and none is bound. A second test adds a global that does have a row,
and the client its row names binds it by the same guess, so the first test
cannot pass for lack of a way to bind.

In the guest and nested, where nothing can be added, a client of a listener
asked for all 25 and for the three globals that other clients are offered, at
every global number it was not told about. Each of the 28 was refused at each
number.

## Drawing

- **A still desktop draws nothing.** With a window on screen and nothing
  changing, the guest drew 8 frames before a ten-second wait and 8 after it.
  The compositor's process had run 1,376 times before the wait and 1,376 after.
- **A change draws again.** Moving the pointer, which the TTY backend draws,
  took the count to 18, and it stopped there.
- **A client that commits on every frame is held to the display's rate**, even
  when its commits change nothing and so no frame is drawn (test).

## What is linked

`tests/linked.rs` reads the symbols of the built binary. It fails if the
binary holds code from `reis` or Smithay's libei backend, from Smithay's
XWayland modules, from its X11 backend, or from Smithay's and
`wayland-server`'s code for a socket that any process can find. A control test
fails if the binary is stripped, so the others cannot pass by finding nothing.

The guard was checked against a build that calls into `reis`: it fails.

## Found on the way

| Finding | What was done |
| --- | --- |
| The backend that draws into a window on a host session put X11 client code in the binary: 1,422 symbols from `x11rb` alone. ADR 0020 says the compositor never holds X11 code | That backend is now behind a `nested` build feature, which Base never builds. The guard fails on X11 client code |
| A popup or subsurface that is destroyed commits nothing, so nothing drew it away | Both now queue a redraw. A test failed first |
| `pgrep -x crowbar-compositor` never matches: the kernel keeps 15 characters of a process's name. Two checks in the guest script passed without testing anything | The script finds the process by its whole name, and stops if it finds none |
| `openvt` hands the program it starts a second descriptor for the terminal, which is not close-on-exec | Recorded. The compositor did not open it. A Base service that starts the compositor through `openvt` passes it the same one |

## Read from the records, and not decided in them

1. **What the shell is offered.** The table gives layer shell to "the shell
   alone". The shell also has to draw windows, so it is offered what a
   compartment with the GPU is offered, and layer shell.
2. **How the compositor learns that a compartment's needs include the GPU.**
   The Warden says who a compartment is and nothing more. No part of Base says
   it yet, so outside the tests no compartment is offered `zwp_linux_dmabuf_v1`.
3. **Which slice creates each remaining global of the table.** The
   [compositor's README](../../compositor/README.md#what-each-client-is-offered)
   lists them. The data device waits for the clipboard rules in slice 4. The
   ADR names no slice for presentation time or cursor shape.

## Not run, and not built

- **A GPU.** The guest draws in software, and every flip waited for its
  drawing (`needs_sync=true`). The NVIDIA TTY check that ADR 0020 asks of a
  Smithay bump was not run: this change does not move the pin, and the tower's
  one GPU was in use.
- **The real shell.** The compositor has no control channel until slice 2, so
  the shell and Studio Settings were not run on it. The checks that run them
  stay with the spike.
- **A real Warden.** Every listener came from the stand-in.
- **A flip the display never reports.** An output would then wait and draw no
  more. The TTY backend ends the wait when a paused session resumes, and
  nothing else does.

## How to repeat

```sh
cd compositor
cargo test
cargo build --features nested && checks/nested.sh /usr/bin/wayland-info

cd ..
lab/void/compositor_guest.py check
lab/void/compositor_guest.py desktop-check
lab/void/void_lab.py milestone
```

The logs go to `.lab/`, outside git. On Arch, set `QEMU_MODULE_DIR` to a
directory that holds QEMU's virtio-gpu modules.

---

Work done by Claude Fable 5.2 in the Claude Code harness (through T3 Code).
