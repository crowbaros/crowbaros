# CrowbarOS starts again, as a small desktop with an agent at the person's side

**Status:** accepted on November 18, 2026, by Jim Martin and Jason Alexander. Proposed on November 17 by Jim Martin. [ADR 0031](0031-the-compositor-is-built-in-this-repository-again.md) later replaced ADR 0027, one of the compositor records it keeps. [ADR 0032](0032-fulcrum-serves-the-shell-and-the-persons-apps.md) amends this record: it ends the archived records' hold on the compositor for the shell and the person's apps, leaving what the compositor offers the Agent to Milestone 3.

The [substrate vision](../archive/substrate-vision/README.md) made CrowbarOS a lean base that each person shaped into their own OS with Tools their agent built, under a deterministic security model. It produced 28 decision records, five council review rounds, a threat model, and about 20,000 lines of Rust for its state, apply engine, and security rules, but no desktop a person could use for a day. This record ends it and starts the [new vision](../VISION.md).

## The decision

- **CrowbarOS is a small desktop on Void with the Agent built in.** A person installs Base, our compositor, the shell, a handful of default apps, and the Agent. The Agent runs from boot, the person can summon it from anywhere, and it stays out of the way until they do.
- **The substrate vision is archived.** Its vision, glossary, threat model, architecture notes, milestones, and reviews move to [`docs/archive/substrate-vision/`](../archive/substrate-vision/), with ADRs 0001 to 0014, 0016 to 0026, and 0029. They are history. A part of them comes back only through a new record.
- **The compositor decisions stand:** [ADR 0015](0015-our-own-compositor-on-smithay.md), [ADR 0027](0027-the-compositor-is-built-in-its-own-repository.md), and [ADR 0028](0028-the-compositor-is-written-from-the-ground-up.md). Fulcrum's target is unchanged: the new compositor replaces the Smithay baseline when it passes the baseline's tests and the Void guest's checks. The requirements those records take from archived ones, such as who the clients are and what each is offered (ADRs 0016, 0020, 0022, 0023, and 0029), bind only until a new record says what the compositor offers the shell and the Agent.
- **The code of the substrate vision is removed:** the Rust workspace in `crates/`, the adversarial lab, the Base generations lab, and the Warden's tests. It is at the [`substrate-final`](https://github.com/crowbaros/crowbaros/tree/substrate-final) tag.
- **What carries over stays where it is:** the compositor pin, the compositor spike that the Alpha desktop runs, the GPUI shell, Studio Settings, the GPUI probe, the graphical Void lab, the brand, and the design system.
- **What the earlier plans taught us is kept in LESSONS.md.** It was later removed, because most of it held for the earlier plans and not this one.
- **ADR numbers continue from 0030.** Archived records keep their numbers, so a reference to one still means one record. This record was first merged as a second 0029, beside ADR 0029 on compositor budgets for compartments, which merged first and is archived with the rest.

## Considered Options

- **Keep building the substrate vision.** Rejected: the next milestone was more security machinery, and a person still could not use the desktop.
- **Merge the `new-vision` branch as it is.** It adds a new Base beside all of the substrate vision's code, so main would mix two plans. Rejected. The branch is rebased onto this reset and merged as its own pull request.
- **A new repository, or an orphan branch.** Either is a clean slate. Rejected: each loses the history, the issue numbers, and the links that dated records hold. A tag keeps all of it.
- **Delete the substrate vision's documents.** Rejected: they hold the reasoning behind the compositor decisions and the security ideas we expect to need again.

## Consequences

- CI no longer builds a Rust workspace at the repository root or runs the Warden's tests. It checks the lab scripts, the compositor spike, and the apps.
- `lab/void/void_lab.py` keeps only the helpers the graphical guest shares. `void_lab.py milestone` and `warden_check.py` are gone.
- The substrate vision's open issues close as `wontfix`, with a comment pointing at the tag. Issues for the new milestones are written against the new vision.
- `PRODUCT.md`, `DESIGN.md`, and `apps/README.md` still describe Tools, the powerbox, and the catalog. They link to the archived vision until Jason Alexander restates them.
- Pull request scopes change to match the new parts. [AGENTS.md](../../AGENTS.md) lists them.
