# Fulcrum

Fulcrum is the CrowbarOS Wayland compositor. CrowbarOS's two builders, Jim
Martin and Jason Alexander, work on it with coding agents, under the same
standard as the [CrowbarOS repository](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/AGENTS.md).
This file covers what differs here.

## Ground rules

- [The plan](docs/PLAN.md) orders the work. The [README](README.md) says what is
  built and where it lives. [Picking up the work](docs/picking-up.md) says where
  the work paused and how to start the next piece; keep it current when the
  work pauses.
- CrowbarOS decides what the compositor must do: who its clients are, what each
  is offered, what it draws, and how a person confirms. A change to any of those
  needs a decision record in CrowbarOS's `docs/adr/` first. Link to it from the
  pull request here.
- Write with the terms in CrowbarOS's [glossary](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/GLOSSARY.md).
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
- **Smithay and Otto are MIT.** Their code may be adapted, with attribution in
  `LICENSE-smithay.txt` or a file like it. niri, COSMIC, Pinnacle, and Jay are
  GPL-3.0: read them for ideas, and copy nothing.
- A new crate of any kind needs a reason in the pull request that adds it.

## Branches and pull requests

- `main` changes only through a pull request from a short-lived branch, squash
  merged. Commit locally as you go. Push, open a pull request, or merge only when
  the person you work for asks.
- Write the pull request title as a conventional commit that states the outcome
  in plain language. The scope is the area: `wire`, `codegen`, `sys`,
  `identity`, `policy`, `input`, `windows`, `redraw`, `render`, `backend`,
  `probe`, `checks`, `ci`, `docs`.
- One pull request does one thing, with its code, tests, and docs.

## Checks

| Run | When |
| --- | --- |
| The commands under the README's [Build and test](README.md#build-and-test) | Every change to code. CI runs them too |
| `cargo run -p fulcrum-codegen` | A change to `protocols/`. Commit what it writes |
| CrowbarOS's `lab/void/compositor_guest.py check`, with `FULCRUM_REPOSITORY` and `FULCRUM_REVISION` set to the commit | Every pull request that changes how the compositor serves clients (ADR 0022), once it serves them (Milestone 2). Attach the evidence log it writes |

A bug fix comes with a test that fails without the fix. Offering a protocol
global to a client takes a row in the policy table, a test of who is offered
it, and the matching list in CrowbarOS's guest check.

## Releasing to CrowbarOS

CrowbarOS builds the full commit ID pinned in its `compositor/fulcrum.json`.
Changing the compositor in Base is a CrowbarOS pull request that changes that
pin, with a guest check run at the new revision.
