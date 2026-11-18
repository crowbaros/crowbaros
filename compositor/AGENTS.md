# Fulcrum

Fulcrum is the CrowbarOS Wayland compositor, in `compositor/`. The
[repository's standard](../AGENTS.md) holds here: its branches, pull requests,
and checks. This file adds what holds only for the compositor.

## Ground rules

- [The plan](docs/PLAN.md) orders the work. The [README](README.md) says what is
  built and where it lives. [Picking up the work](docs/picking-up.md) says where
  the work paused and how to start the next piece; keep it current when the
  work pauses.
- CrowbarOS decides what the compositor must do: who its clients are, what each
  is offered, what it draws, and how a person confirms. A change to any of those
  needs a decision record in [`docs/adr/`](../docs/adr/) first, in the same
  pull request as the code or before it.
- Write with the terms in the [glossary](../GLOSSARY.md).
- Dated records in [`docs/`](docs/) stay as written. New results get a new
  record.
- Keep the binary named `crowbar-compositor` until CrowbarOS's services change
  in a pull request of their own.

## Dependencies

These hold for every change ([the plan](docs/PLAN.md)):

- **No crate from the Smithay organization**: not `smithay`, the `wayland-rs`
  crates, `drm`, `gbm`, `input`, `calloop`, or `libseat`. A test fails if one
  appears in `Cargo.lock`.
- **The only C libraries are Mesa's EGL, GLES, and GBM, libinput, and
  libxkbcommon.** Adding another needs a CrowbarOS decision record.
- **`unsafe` code lives in `sys/` alone.** Every other crate forbids it.
- **Study Smithay for design ideas and write the implementation here.** Otto
  is MIT and its code may be adapted with attribution. Use niri, COSMIC,
  Pinnacle, and Jay as design references.
- A new crate of any kind needs a reason in the pull request that adds it.

## Pull requests

- The title's scope is `compositor`, as for every compositor change in this
  repository.
- One pull request does one thing. A change that needs the shell, the lab, or
  Base too changes them in the same pull request.

## Checks

| Run | When |
| --- | --- |
| The commands under the README's [Build and test](README.md#build-and-test) | Every change to code. CI runs them too |
| `cargo run -p fulcrum-codegen` | A change to `protocols/`. Commit what it writes |
| `lab/void/compositor_guest.py check`, from the repository's root | Every pull request that changes how the compositor serves clients ([ADR 0031](../docs/adr/0031-the-compositor-is-built-in-this-repository-again.md)). Attach the evidence log it writes |

A bug fix comes with a test that fails without the fix. Offering a protocol
global to a client takes a row in the policy table, a test of who is offered
it, and the matching list in `lab/void/compositor_check.py`.

## Reaching Base

A compositor change reaches Base when its pull request merges
([ADR 0031](../docs/adr/0031-the-compositor-is-built-in-this-repository-again.md)).
Until November 18, 2026, Fulcrum was its own repository and Base built a pinned
commit ([ADR 0027](../docs/adr/0027-the-compositor-is-built-in-its-own-repository.md)).
That repository, `crowbaros/fulcrum`, is archived; its pull requests stay
readable there.
