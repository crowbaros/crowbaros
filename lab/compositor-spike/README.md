# Compositor spike

**This is the [spike](https://github.com/crowbaros/crowbaros/issues/3), not the
compositor.** It was written to test
[ADR 0015](../../docs/adr/0015-our-own-compositor-on-smithay.md), and its code is
throwaway. The compositor is built in Fulcrum, at the commit
[`compositor/`](../../compositor/README.md) pins.

The spike is kept here for one reason: the Alpha desktop check still runs it
([ADR 0020](../../docs/adr/0020-the-first-compositor-is-small-on-purpose.md)).
When the compositor passes that check, this directory is deleted. Change
nothing here except to keep it building.

The [spike record](../../docs/research/compositor-spike-2026-11-15.md) says what it
proved and what it did not. The compositor's test harness, probe client,
protocol check, and Void guest check were made from this one's. The checks that run the
real shell (`checks/nested-apps.sh`, `checks/tty-apps.sh`) stay here until the
compositor has the shell's protocol
([issue 71](https://github.com/crowbaros/crowbaros/issues/71)).

What the compositor does that a borrowed one would not
([ADR 0014](../../docs/adr/0014-a-yes-comes-from-the-person.md)):

- It delivers input only as the shell directs. It offers no Tool or app a
  virtual keyboard, virtual pointer, input-method, or input-emulation interface.
- It takes the confirm gesture itself and forwards it to no client.
- It decides what is drawn above what, so system chrome cannot be covered or
  imitated.
- It gates screen capture and the clipboard per principal.

The spike does the first two, and gives layer-shell to the shell alone. It does
not draw any marking of its own, and it does not gate the clipboard.

## Layout

This is its own Cargo project with a pinned `Cargo.lock`, like the apps in
[`apps/`](../../apps/README.md). It is not a member of the root workspace, because
it pins Smithay to a git commit and needs system libraries (DRM, libinput,
libseat, udev) that CI does not install. It can depend on crates in
[`crates/`](../../crates/README.md) by path.

| Path | Contents |
| --- | --- |
| `src/policy.rs` | The one table that says which client sees which Wayland global |
| `src/input.rs` | The one function every key enters by, and the confirm chord |
| `src/control.rs` | The control channel handed to the shell, in place of the Hyprland socket |
| `src/windows.rs` | Window placement, focus, and workspaces |
| `src/handlers/` | Smithay protocol handlers. `mod.rs` routes every global through `policy.rs` |
| `src/winit.rs`, `src/tty.rs` | The two backends: a window on a host session, and a TTY |
| `src/probe.rs`, `src/bin/crowbar-probe.rs` | An ordinary Wayland client for looking at a compositor from outside |
| `tests/` | Tests that run the compositor in-process, without a display, against real Wayland clients |
| `checks/` | Scripts for the checks that need a display |

It started from Smithay's `smallvil` example, and the TTY backend is adapted
from Smithay's `anvil` example. Both are MIT; see
[`LICENSE-smithay.txt`](LICENSE-smithay.txt).

Do not copy code from niri or cosmic-comp. Both are GPL-3.0, and CrowbarOS is
MIT. Smithay and its example compositors are MIT.

## Build and test

Smithay's default features are off. That leaves out `backend_libei` (input
emulation), `xwayland`, `backend_x11`, and `backend_vulkan`.
The `reis` crate is still built, because Smithay depends on it without a
feature gate. None of its code is linked into the compositor; see the
[bump measurement](../../docs/research/compositor-smithay-bump-2026-11-15.md).

```sh
cd lab/compositor-spike
cargo test                              # 22 tests, no display needed
cargo clippy --all-targets -- -D warnings
```

## Run

```sh
cargo build

# Nested, in a window on a Wayland or X11 session:
target/debug/crowbar-compositor --shell /path/to/crowbaros-shell \
    --spawn /path/to/crowbaros-studio-settings

# The checks that need a display. They write logs to .lab/compositor/.
checks/nested.sh /usr/bin/wayland-info
checks/nested-apps.sh /path/to/crowbaros-shell /path/to/crowbaros-studio-settings
```

`--shell` starts the shell and hands it two connected sockets. `--spawn`
starts an ordinary client. `crowbar-compositor --help` lists the rest.

On a TTY the spike needs a seat from seatd. The
[Void lab](../void/README.md) builds it against Void's libraries and runs
it that way, with the shell and Studio Settings:

```sh
lab/void/compositor_guest.py desktop-check
```

On a real machine with no desktop session, `checks/tty-apps.sh` runs the
shell and Studio Settings on a TTY through DRM, over SSH, and counts their
commits; `--suspend` also suspends and resumes the machine. `--mode NAME=WxH`
picks an output's mode in place of the one its monitor prefers. The
[NVIDIA record](../../docs/research/compositor-nvidia-2026-11-15.md) says how they
were run.
