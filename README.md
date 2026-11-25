# CrowbarOS

**Quiet surface. Sharp mind.**

CrowbarOS is a small operating system on Void Linux with an agent built into it.
A person installs a bare desktop: our own compositor, a shell, a handful of
default apps, and the Agent. The Agent is part of the OS, one gesture away from
anywhere, and out of the way until the person asks.

## Start here

1. [Vision](docs/VISION.md): the plan of record.
2. [Glossary](GLOSSARY.md): the words this project uses, and the ones it avoids.
3. [Decision records](docs/adr/).
4. [Design system](DESIGN.md) and [product brief](PRODUCT.md): how CrowbarOS
   looks and what its interfaces must do.

## Status

CrowbarOS started again on November 17, 2026
([ADR 0030](docs/adr/0030-crowbaros-starts-again-with-an-agent-at-the-persons-side.md)).
The substrate vision before it is archived, and its code is at the
[`substrate-final`](https://github.com/crowbaros/crowbaros/tree/substrate-final) tag.

| Area | State |
| --- | --- |
| Base | Being rebuilt as plain Void with the desktop on it, built without root, booted under UEFI, in [`base/`](base/) |
| Compositor | Our own ([ADR 0015](docs/adr/0015-our-own-compositor-on-smithay.md)), named Fulcrum, built in [`compositor/`](compositor/) ([ADR 0031](docs/adr/0031-the-compositor-is-built-in-this-repository-again.md)), written from the ground up with Smithay as its reference ([ADR 0028](docs/adr/0028-the-compositor-is-written-from-the-ground-up.md)). The desktop runs on it |
| Shell, Files, and Studio Settings | The desktop on Base, with separate runit services and saved appearance, booted under UEFI by the desktop check. [Earlier lab record](docs/development/void-desktop-smoke-2026-11-15.md), [apps](apps/) |
| Brand and design system | Approved. [Brand kit](docs/branding/), [DESIGN.md](DESIGN.md) |
| The Agent | Super+Space brings up the Agent's surface, a drawer the [shell](apps/shell/README.md) draws, and Fulcrum serves desktop context on its agent socket. [Lever](lever/README.md) (`crowbar-lever`) runs from boot as the person, speaks ACP v1 to a harness, keeps the record of every thread in SQLite, and serves [`crowbar-agent`](lever/client/README.md) on its person socket ([ADR 0036](docs/adr/0036-lever-drives-the-harnesses-a-person-has-and-crowbaros-builds-none.md)); no real harness is bundled in Base yet. See the [vision](docs/VISION.md#the-agent) |

## Repository map

| Path | Contents |
| --- | --- |
| [`base/`](base/) | Plain Void Linux with the desktop on it, built without root, that boots under UEFI in a VM or [on a real machine](base/HARDWARE.md) |
| [`apps/`](apps/) | The GPUI interfaces: desktop shell, Files, Studio Settings, and the GPUI probe |
| [`libs/`](libs/) | Shared libraries: [`crowbar-theme`](libs/crowbar-theme/README.md) (pure Rust palettes, WCAG contrast, store, config renderers) and [`crowbar-ui`](libs/crowbar-ui/README.md) (GPUI 0.2.2 bundled fonts, `UiPalette`, `ThemeWatcher`, and `DESIGN.md` primitives) |
| [`compositor/`](compositor/) | Fulcrum, the Wayland compositor: its own Cargo project, with its own CI workflow |
| [`lever/`](lever/) | [Lever](lever/README.md) (`crowbar-lever`), the Rust service that runs from boot as the person, speaks ACP v1 to a harness, and keeps every thread's record ([ADR 0036](docs/adr/0036-lever-drives-the-harnesses-a-person-has-and-crowbaros-builds-none.md)); its scripted ACP stub harness (`crowbar-stub-harness`); and [`lever/client/`](lever/client/README.md) (`crowbar-agent`) |
| [`lab/`](lab/) | The Void lab: the compositor check and the desktop check, both on Base's image |
| [`docs/VISION.md`](docs/VISION.md) | The vision, the plan of record |
| [`docs/adr/`](docs/adr/) | Decision records |
| [`docs/research/`](docs/research/) | Prior-art research and compositor records |
| [`docs/branding/`](docs/branding/) | The approved brand kit and the rounds that led to it |
| [`docs/development/`](docs/development/) | Current Void build and run guidance, and dated validation records |
| [`docs/archive/`](docs/archive/) | The Fedora blueprint and the substrate vision. Not current. Do not build on them |
| [`DESIGN.md`](DESIGN.md), [`PRODUCT.md`](PRODUCT.md), [`.impeccable/`](.impeccable/) | The design system, the product brief, and the design review evidence |

## Build and test

```sh
python3 -m unittest discover -s base -p "test_*.py"
python3 -m unittest discover -s lab/void -p "test_*.py"
python3 -m unittest discover -s .github/workflows -p "test_*.py"   # which jobs CI runs for a change
base/void_base.py check        # build Base with the desktop, boot it, and check it
lab/void/compositor_check.py   # the compositor, on a copy of Base's image booted under UEFI
lab/void/desktop_check.py      # the desktop and Base boot checks, on a copy of Base's image booted under UEFI
```

The compositor's own build and test commands are in its
[README](compositor/README.md#build-and-test). The GPUI apps are separate Cargo projects with their own pinned
dependencies. See [`apps/`](apps/README.md) for how to build and run
them.

## Working in this repository

- [`AGENTS.md`](AGENTS.md) is the standard for how we and our agents work:
  pull requests, merging, and checks.
- Use the terms in the [glossary](GLOSSARY.md). When a term is missing or wrong,
  change the glossary first.
- A decision that is hard to reverse gets a [decision record](docs/adr/).
- [`docs/archive/`](docs/archive/) is history. Where it disagrees with the
  vision, the vision is right.
- CrowbarOS is [MIT](LICENSE). Third-party parts keep their own terms: the GPUI
  patches and the adapted GPUI text input in `apps/shell` are
  Apache-2.0, and the bundled fonts are OFL-1.1.
