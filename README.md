# CrowbarOS

**Quiet surface. Sharp mind.**

CrowbarOS is a small operating system on Void Linux with an agent built into it.
A person installs a bare desktop: our own compositor, a shell, a handful of
default apps, and the Agent. The Agent is part of the OS, one gesture away from
anywhere, and out of the way until the person asks.

## Start here

1. [Vision](docs/VISION.md): the plan of record. It is a draft for both of us
   to review.
2. [Lessons](docs/LESSONS.md): what the earlier plans taught us.
3. [Glossary](GLOSSARY.md): the words this project uses, and the ones it avoids.
4. [Decision records](docs/adr/).
5. [Design system](DESIGN.md) and [product brief](PRODUCT.md): how CrowbarOS
   looks and what its interfaces must do.

## Status

CrowbarOS started again on November 17, 2026
([ADR 0029](docs/adr/0029-crowbaros-starts-again-with-an-agent-at-the-persons-side.md)).
The substrate vision before it is archived, and its code is at the
[`substrate-final`](https://github.com/crowbaros/crowbaros/tree/substrate-final) tag.

| Area | State |
| --- | --- |
| Base | Being rebuilt as plain Void, built without root, booted under UEFI, on the `new-vision` branch |
| Compositor | Our own ([ADR 0015](docs/adr/0015-our-own-compositor-on-smithay.md)), built in Fulcrum at the commit Base pins ([ADR 0027](docs/adr/0027-the-compositor-is-built-in-its-own-repository.md)), and being rewritten there from the ground up with Smithay as its reference ([ADR 0028](docs/adr/0028-the-compositor-is-written-from-the-ground-up.md)). The Alpha desktop still runs the [spike](lab/compositor-spike/README.md) |
| Shell and Studio Settings | An Alpha session on Void with separate runit services, saved appearance, and 37 passing desktop checks. [Record](docs/development/void-desktop-smoke-2026-11-15.md), [apps](apps/) |
| Brand and design system | Approved. [Brand kit](docs/branding/), [DESIGN.md](DESIGN.md) |
| The Agent | Not started. See the [vision](docs/VISION.md#the-agent) |

## Repository map

| Path | Contents |
| --- | --- |
| [`apps/`](apps/) | The GPUI interfaces: desktop shell, Studio Settings, and the GPUI probe |
| [`compositor/`](compositor/) | The Fulcrum commit that Base builds as its Wayland compositor, and where the code lives |
| [`lab/`](lab/) | The Void lab for the compositor and the Alpha desktop, and the compositor spike that desktop still runs |
| [`docs/VISION.md`](docs/VISION.md), [`docs/LESSONS.md`](docs/LESSONS.md) | The vision, and what the earlier plans taught us |
| [`docs/adr/`](docs/adr/) | Decision records |
| [`docs/research/`](docs/research/) | Prior-art research and compositor records |
| [`docs/branding/`](docs/branding/) | The approved brand kit and the rounds that led to it |
| [`docs/development/`](docs/development/) | Current Void build and run guidance, and dated validation records |
| [`docs/archive/`](docs/archive/) | The Fedora blueprint and the substrate vision. Not current. Do not build on them |
| [`DESIGN.md`](DESIGN.md), [`PRODUCT.md`](PRODUCT.md), [`.impeccable/`](.impeccable/) | The design system, the product brief, and the design review evidence |

## Build and test

```sh
python3 -m unittest discover -s lab/void -p "test_*.py"
lab/void/compositor_guest.py check          # the compositor in a Void VM
lab/void/compositor_guest.py desktop-check  # the Alpha desktop in a fresh Void VM
```

The GPUI apps and the compositor spike are separate Cargo projects with their
own pinned dependencies. See [`apps/`](apps/README.md) for how to build and run
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
  layer-shell backport and the adapted GPUI text input in `apps/shell` are
  Apache-2.0, and the bundled fonts are OFL-1.1.
