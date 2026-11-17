# CrowbarOS

**Quiet surface. Sharp mind.**

CrowbarOS is an operating system that ships a small, stable base, which each
person turns into their own OS by stating intents to an agent. Changes to a
person's tools are checked in a private copy before they go live, and can be
kept, undone, and shared. Security does not depend on a model being right, and
every rule is held to a limit on how often it asks the person.

## Start here

1. [Vision](docs/VISION.md): the plan of record. The one-page
   [pitch](docs/pitch.html) is the short version.
2. [Glossary](GLOSSARY.md): the words this project uses, and the ones it avoids.
3. [Decision records](docs/adr/) and the [threat model](docs/threat-model.md).
4. [Design system](DESIGN.md) and [product brief](PRODUCT.md): how CrowbarOS
   looks and what its interfaces must do.

## Status

| Area | State |
| --- | --- |
| Milestone 1: Crowbar State, apply engine, Base generations, boot-menu rollback | Done. [Record](docs/milestones/milestone-1.md) |
| Milestone 2: the security rules, built to be attacked | Next. [Scope](docs/milestones/milestone-2.md) |
| Brand and design system | Approved. [Brand kit](docs/branding/), [DESIGN.md](DESIGN.md) |
| GPUI shell and Studio Settings | Integrated Void Alpha session with separate runit services, saved appearance, and 37 passing desktop checks. [Record](docs/development/void-desktop-smoke-2026-11-15.md), [apps](apps/) |
| Compositor | Our own, on Smithay ([ADR 0015](docs/adr/0015-our-own-compositor-on-smithay.md)). The first of six slices is built: client identity, the protocol table, and drawing only on change ([ADR 0020](docs/adr/0020-the-first-compositor-is-small-on-purpose.md), [record](docs/research/compositor-slice-1-2026-11-15.md)). The Alpha desktop still runs the spike that tested the decision. [Spike record](docs/research/compositor-spike-2026-11-15.md); [NVIDIA runs](docs/research/compositor-nvidia-2026-11-15.md) |

The full roadmap is in the [vision](docs/VISION.md#roadmap).

## Repository map

| Path | Contents |
| --- | --- |
| [`base/`](base/) | The rebuild's starting point: plain Void Linux, built without root, that boots under UEFI in a VM |
| [`crates/`](crates/) | The Rust workspace: the Crowbar State, the apply engine, and the skeleton crates for Milestone 2 |
| [`apps/`](apps/) | The GPUI interfaces: desktop shell, Studio Settings, and the GPUI probe |
| [`compositor/`](compositor/) | The CrowbarOS Wayland compositor, on Smithay |
| [`lab/`](lab/) | Void Base and desktop labs, the compositor spike that the Alpha desktop still runs, and adversarial scaffolding; Fedora tooling is archived |
| [`docs/VISION.md`](docs/VISION.md) | The vision, the stack, and the roadmap |
| [`docs/adr/`](docs/adr/) | Decision records |
| [`docs/architecture/`](docs/architecture/) | Design notes: state and apply engine, capability model, gate and monitor |
| [`docs/milestones/`](docs/milestones/) | Milestone scopes, write-ups, and evidence |
| [`docs/research/`](docs/research/), [`docs/reviews/`](docs/reviews/) | Prior-art research and the council review rounds |
| [`docs/branding/`](docs/branding/) | The approved brand kit and the rounds that led to it |
| [`docs/development/`](docs/development/) | Current Void build/run guidance and unchanged historical validation records |
| [`docs/archive/`](docs/archive/) | Superseded material. Not current. Do not build on it |
| [`DESIGN.md`](DESIGN.md), [`PRODUCT.md`](PRODUCT.md), [`.impeccable/`](.impeccable/) | The design system, the product brief, and the design review evidence |

## Build and test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
base/void_base.py check            # build the Void base, boot it, and check it
lab/void/void_lab.py milestone     # the full Milestone 1 loop in a Void Linux VM
```

The GPUI apps are separate Cargo projects with their own pinned dependencies.
See [`apps/`](apps/README.md) for how to build and run them.

## Working in this repository

- [`AGENTS.md`](AGENTS.md) is the standard for how we and our agents work:
  pull requests, merging, and checks.
- Use the terms in the [glossary](GLOSSARY.md). When a term is missing or wrong,
  change the glossary first.
- A decision that is hard to reverse gets a [decision record](docs/adr/).
- [`docs/archive/`](docs/archive/) describes the earlier Fedora plan. Where it
  disagrees with the vision, the vision is right.
- CrowbarOS is [MIT](LICENSE). Third-party parts keep their own terms: the GPUI
  layer-shell backport and the adapted GPUI text input in `apps/shell` are
  Apache-2.0, and the bundled fonts are OFL-1.1.
