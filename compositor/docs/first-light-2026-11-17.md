# Fulcrum shows windows on a virtual terminal

**Date:** November 17, 2026. **Plan milestone:** [2 · First light](PLAN.md#implementation-milestones).

Milestone 2 put Fulcrum's own compositor on a display. It runs on a virtual
terminal of CrowbarOS's Void guest, with the seat from seatd, the display
through the kernel's mode setting, and input through libinput. It shows the
shell's layers and the compartments' windows, gives the keyboard to what the
person clicks, and draws only when something changed. Void's own `foot` and
`wayland-info` run on it, as clients of a compartment's listener.

| Pull request | What it built |
| --- | --- |
| [#7](https://github.com/crowbaros/fulcrum/pull/7) | xdg-shell windows and popups, and redraw on change |
| [#8](https://github.com/crowbaros/fulcrum/pull/8) | Layer shell for the shell's bar, dock, launcher, and wallpaper |
| [#9](https://github.com/crowbaros/fulcrum/pull/9) | The seat: the keyboard through libxkbcommon, and the pointer |
| [#10](https://github.com/crowbaros/fulcrum/pull/10) | The TTY backend: Fulcrum's own seatd client, KMS through the kernel's ioctls with dumb buffers, libinput, and netlink hotplug |
| [#11](https://github.com/crowbaros/fulcrum/pull/11) | The data device, which moves nothing yet, and `wp_viewporter` |
| [#12](https://github.com/crowbaros/fulcrum/pull/12) | The nested backend, and outputs that change size |
| [#13](https://github.com/crowbaros/fulcrum/pull/13) | Budgets that fit `foot`'s pools, and a budget for the pixels a client makes the compositor copy |
| This one | This record |

## The exit check

| Plan's check | Result |
| --- | --- |
| All 57 baseline tests ported and passing | Passing. 45 keep their names. The other 12 are merged or say what Fulcrum guards instead; the table below lists each |
| The Void guest check passes against the new binary | **37 of 37**, at `248a22a`, the head of #13, whose tree `main` holds at `b388fb5`. The baseline's 36 checks, with the changes below, and one for `foot`. [The log](assets/first-light-2026-11-17/guest-check.log) and [`foot` on the first output](assets/first-light-2026-11-17/foot.png) |
| Void's own `foot` and `wayland-info` run on it | Both, on a compartment's listener. `wayland-info` lists the compartment's row of ADR 0020's table, and the guest check reads it. `foot` opens a window and keeps running, which is now a check of its own |

`cargo test --locked` passes 179 tests, and so does the nested build
(`--features nested`), which adds two and leaves out the two that hold only
without it. `Cargo.lock` holds 14 crates, none from the Smithay organization.

### The baseline tests that changed name

| Baseline test | Here |
| --- | --- |
| `main.rs`: `no_flag_starts_a_client_or_opens_a_socket`, `the_only_backend_is_a_tty` | `main.rs`: `no_flag_starts_a_client_opens_a_socket_or_picks_another_backend`, and `a_build_without_the_nested_feature_has_no_nested_backend` |
| `connections.rs`: `a_window_belongs_to_the_compartment_whose_listener_its_client_came_through`, `what_a_client_calls_itself_does_not_change_who_it_is` | `windows.rs`: `a_window_belongs_to_its_clients_compartment_whatever_it_calls_itself`, with `connections.rs`: `a_client_belongs_to_the_compartment_whose_listener_it_came_through` |
| `exhaustion.rs`: `a_compartment_that_uses_up_the_descriptors_does_not_end_the_compositor` | `exhaustion.rs`: `a_compartment_that_opens_connections_without_end_is_stopped_at_its_budget`. Under ADR 0029 a compartment never uses the descriptors up |
| `linked.rs`: `the_guard_sees_what_the_binary_does_hold`, `the_binary_holds_no_libei_code`, `the_binary_holds_no_xwayland_code`, `the_binary_holds_no_x11_backend_code`, `the_binary_holds_no_x11_client_code`, `the_binary_holds_no_code_that_opens_a_public_socket`, `a_symbol_is_from_a_module_only_when_the_module_starts_a_path_in_it` | `linked.rs`: `the_binary_links_only_the_libraries_adr_0028_allows`, `the_binary_holds_no_smithay_or_wayland_rs_code`, and `the_binary_never_looks_for_where_a_public_socket_would_go`. The baseline had to read Smithay's symbols to tell its X11, XWayland, and libei code from the rest. Fulcrum has none of that code to tell apart, so the guard checks what the binary links and that no symbol of those crates is in it, and each test first checks that it read the binary |

Each pull request ran the check at its head, with the check as it now
stands:

| Pull request | Result | What failed |
| --- | --- | --- |
| #10 | 34 of 37 | The lists without the data device, and `foot` |
| #11 | 36 of 37 | `foot`, refused its pool of 512 MiB |
| #12 | 36 of 37 | The same |
| #13 | 37 of 37 | Nothing |

### What the guest check changed

The check is CrowbarOS's `lab/void/compositor_check.py`. These changes go with
the pull request that moves CrowbarOS's pin:

- The seat comes from seatd through Fulcrum's own client, so the check looks
  for `session opened through seatd`, and the log level is `FULCRUM_LOG`.
- A compartment's list gains `wl_data_device_manager`, which the table always
  offered and Fulcrum now creates.
- `zwp_linux_dmabuf_v1` is created for no one until Milestone 3, so the
  shell's list no longer has it. A compartment's guess at it is still
  refused.
- The probe that stands in for the Warden says the connection was refused, in
  its own words.
- New: `foot` opens a window on the compartment's listener and keeps running.

## What changed from the plan

- **The GLES renderer, GBM, and damage tracking moved to Milestone 3.** First
  light draws with the CPU renderer into dumb buffers, which every display
  the guest and the lab machines have can show. Milestone 3 needs EGL and
  GBM for linux-dmabuf anyway, and its exit check is the GPUI shell on real
  GPUs.
- **The data device and `wp_viewporter` came early.** `foot` will not start
  without the data device, and asks for a viewport as it starts. The data
  device moves nothing until slice 4 (Milestone 6): a selection is offered to
  no other client, and a drag is cancelled as it starts.
- **ADR 0029's shared-memory budget grew, as it allows.** `foot` reserves a
  pool of 512 MiB for each window and leaves it sparse. ADR 0029 had one pool
  at most 256 MiB and a client's pools at most 512 MiB, and said no Alpha app
  comes near; `foot` does. A pool costs address space until it is written
  to, so one pool may now be 1 GiB and a client's pools 8 GiB. The memory the
  compositor holds for a client is the copies of its committed buffers, and
  those now have the budget of 512 MiB on their own: one buffer can be
  committed on many surfaces, which the pool budget never bounded.

## Found on the way

- **The guard on shared memory failed in release builds.** The `SIGBUS`
  handler marked the fault in a thread-local the optimizer did not know could
  change, so a client that truncated its file read as zeros instead of an
  error. The Void build root caught it, because it tests in release. The flag
  is now an atomic, read after a compiler fence.
- **The stand-in for the Warden gave up after ten seconds.** It waited with
  the probe's patience, then exited and took the compartment's listener with
  it. It now holds the listener until the compositor goes away, as the
  Warden does.

## Not done

- **Hardware.** Only the Void guest's virtio-gpu has run it. The RTX 4090,
  Intel, and Surface machines come with Milestones 3 and 7.
- **The nested backend on a real session.** Its tests run Fulcrum nested in
  Fulcrum; it has not opened a window on another compositor.
- **Cursor images.** The pointer is a white square until Milestone 3.
- **Copy and paste.** Nothing moves between clients until Milestone 6.
