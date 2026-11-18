# Fulcrum

Fulcrum is the CrowbarOS Wayland compositor. CrowbarOS's two builders, Jim
Martin and Jason Alexander, work on it with coding agents, under the same
standard as the [CrowbarOS repository](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/AGENTS.md).
This file covers what differs here.

## Ground rules

- [The plan](docs/PLAN.md) orders the work. The [README](README.md) says what is
  built and where it lives.
- CrowbarOS decides what the compositor must do: who its clients are, what each
  is offered, what it draws, and how a person confirms. A change to any of those
  needs a decision record in CrowbarOS's `docs/adr/` first. Link to it from the
  pull request here.
- Write with the terms in CrowbarOS's [glossary](https://github.com/crowbaros/crowbaros/blob/30fadd8e88fbeb9991eb340f5dee27998b02c9fe/GLOSSARY.md).
- Dated records in [`docs/`](docs/) stay as written. New results get a new
  record.
- Keep the binary named `crowbar-compositor` until CrowbarOS's services change
  in a pull request of their own.

## Branches and pull requests

- `main` changes only through a pull request from a short-lived branch, squash
  merged. Commit locally as you go. Push, open a pull request, or merge only when
  the person you work for asks.
- Write the pull request title as a conventional commit that states the outcome
  in plain language. The scope is the area: `identity`, `policy`, `input`,
  `windows`, `redraw`, `backend`, `probe`, `checks`, `ci`, `docs`.
- One pull request does one thing, with its code, tests, and docs.

## Checks

| Run | When |
| --- | --- |
| The commands under the README's [Build and test](README.md#build-and-test) | Every change to code. CI runs them too |
| `checks/nested.sh` | A change to the nested backend, or to what a client is offered |
| CrowbarOS's `lab/void/compositor_guest.py check`, pointed at the commit as the README's [Run](README.md#run) section says | Every pull request that changes the compositor (ADR 0022). Attach the evidence log it writes |

A bug fix comes with a test that fails without the fix. A new protocol global
gets a row in `src/policy.rs`, a test in `tests/table.rs`, and the matching list
in CrowbarOS's guest check.

## Releasing to CrowbarOS

CrowbarOS builds the full commit ID pinned in its `compositor/fulcrum.json`.
Changing the compositor in Base is a CrowbarOS pull request that changes that
pin, with a guest check run at the new revision.
