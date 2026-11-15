# What a bump of the Smithay pin costs

**Result (November 15, 2026): a bump across 198 days and 245 commits of
Smithay's `master` broke the compositor in three places, and the fix was 31
lines in 4 files.** No logic in the protocol table or the confirm chord
changed. The measurement is a proxy: it ported the compositor backwards,
because no newer Smithay exists to bump to.

The [design brief](compositor-design-brief.md) says to measure one bump before
deciding how often the pin moves. This is that measurement.
[ADR 0020](../adr/0020-the-first-compositor-is-small-on-purpose.md) holds the
policy that rests on it.

## There is no forward bump to measure

| | Commit | Date |
| --- | --- | --- |
| The pin | `fd8e3f50f9556aa6c0485989d1d49ff2c5eefae1` | November 14, 2026 |
| Head of `master`, by `git ls-remote` | The same commit | November 14, 2026 |

The pin is the head of `master` and is one day old. So the measurement went
the other way.

## The dispatch traits are 198 days old

The brief says the traits the protocol table rests on were on `master` "only
days before the spike". That is not so.

- `Dispatch2`, `GlobalDispatch2`, and `delegate_dispatch2!` first appear in
  Smithay commit `0d14cd65`, which landed on `master` on April 30, 2026.
- That is 198 days and 245 commits before the pin.
- `src/wayland/dispatch2.rs` has had no commit since.
- No release contains the traits. The last release is 0.7.0, of June 24, 2025:
  507 days and 528 commits before the pin.

## What a 198-day bump costs

The compositor was pointed at `0d14cd65`, the commit where the traits first
appear, and ported until it built. That is the same span as a bump from the
day the traits landed to the pin.

It failed with 5 compile errors from 3 causes.

| Where | Smithay commit | Fix | Area |
| --- | --- | --- | --- |
| `src/input.rs`, `tests/common/mod.rs` | `80ddb66d`, August 20, 2026: `InputTime` replaces `u32` and `u64` timestamps | Change the type of the timestamp | The input path, including the signature of `handle_key`. No logic changed |
| `src/handlers/mod.rs:57` | `a12d486b`, August 10, 2026: `new_constraint` gained a default body. The commit for `cursor_position_hint` was not found | Two empty methods | Plumbing |
| `src/winit.rs:30` | `f949fa48`, June 11, 2026: winit 0.30 to 0.31 renamed `with_inner_size` | One rename | Plumbing |

- **Size:** 4 source files, 19 lines added and 12 removed, and the two `rev`
  lines in `Cargo.toml`.
- **Security code:** `policy.rs`, the two dispatch `impl` blocks, and the
  logic of the confirm chord needed no change.
- **Tests:** all 22 passed against the older Smithay after the port.
- **Changelog:** of the unreleased "Breaking changes" in that span, only
  `InputTime` reached the compositor. The others are in code it does not use:
  virtual keyboard, `input_intercept`, X11 and XWayland, session lock, text
  input, and `xdg-decoration`.
- **Time:** the port took about a minute. A cold `cargo test` takes 24 seconds
  on the machine used.

## How fast `master` moves

Over the 198 days: 245 commits on 76 days, 25 to 65 commits a month, and a
longest quiet gap of 13 days.

## What is built and what is linked

- **The `reis` crate is in the build at the pin.** Since Smithay `7878e6ed`
  (June 16, 2026) it is a dependency with no feature gate, re-exported as
  `smithay::reexports::reis`. `default-features = false` does not remove it:
  `cargo tree -i reis` lists it.
- **Smithay's libei code is still left out.** `backend::libei` is behind the
  `backend_libei` feature, and the linked `crowbar-compositor` binary has no
  `reis` symbols. So the README's claim holds for the code and not for the
  crate.
- **`x11rb` 0.14.0 is also in the build**, through winit's `x11` feature,
  which Smithay sets. It does not come from `backend_x11` or `xwayland`.
- **The lockfile across the span:** 245 crates then, 227 now. winit moved from
  0.30.13 to 0.31.0-beta.4, `x11rb` from 0.13.2 to 0.14.0, `glam` replaced
  `cgmath`, and `reis` with `enumflags2` was added. `wayland-server`, `drm`,
  `gbm`, `input`, `libseat`, and `udev` did not move.

So a list of crates no longer shows whether input emulation is in the
compositor. ADR 0020 adds a test on what the binary links.

## Checks

| | Tests | `cargo clippy --all-targets -- -D warnings` | `cargo fmt --check` |
| --- | --- | --- | --- |
| At the pin, before | 22 passed | Clean | Clean |
| At the pin, after the port was reverted | 22 passed | Clean | Clean |

The 22 tests are 1 unit test, 7 for the confirm chord, 8 for the control
channel, 4 for globals, and 2 for layers. The toolchain was rustc 1.99.1.

## Not run

- **A forward bump with real breakage.** None exists. The 198-day figure is
  from a port backwards.
- **Clippy on the port.** It was checked with `cargo check` and `cargo test`
  only.
- **Anything that needs a display or a VM:** `checks/nested.sh`,
  `checks/nested-apps.sh`, `checks/tty-apps.sh`, and
  `lab/void/compositor_guest.py check`. The port never ran on a display, on a
  TTY, or against Void's libraries.

Nothing was committed. The port was made in a separate worktree at `88536b2`
and reverted.

---

Measured by Claude Fable 5.2 in the Claude Code harness (through T3 Code).
