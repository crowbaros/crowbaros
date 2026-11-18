# What the earlier plans taught us

CrowbarOS has had two plans before this one: the
[Fedora blueprint](archive/fedora-blueprint/README.md) and the
[substrate vision](archive/substrate-vision/README.md). The code of the second is
at the [`substrate-final`](https://github.com/crowbaros/crowbaros/tree/substrate-final)
tag. This page keeps what they taught us. Read it before you design a part
they already explored.

## Build what a person can touch first

- The substrate vision wrote 28 decision records, a threat model, and five
  council review rounds, and built about 20,000 lines of Rust for its state,
  apply engine, and security rules, before
  anyone could use the desktop for a day. Council round 1 found that Milestone 1
  had tested the least risky layer. The open questions grew faster than the
  code. ([vision](archive/substrate-vision/VISION.md#roadmap),
  [reviews](archive/substrate-vision/reviews/))
- So each milestone now ends in something a person can use, and a guard is
  added when the Agent's reach makes a real risk, not before.
- Keep the issue tracker to the next milestone. The substrate vision's tracker
  held issues for a v1.0 release while Milestone 2 was still being built.

## Security ideas that hold

These are what to reach for when the Agent needs a guard. Each is a decision
record in the archive, with the attacks it was built against.

- **No model on the trust path.** Deterministic code decides what an agent may
  do. A model can propose, but it cannot be its own authority.
  ([ADR 0003](archive/substrate-vision/adr/0003-security-is-deterministic.md))
- **A guard that asks too often gets turned off.** Every rule is held to a limit
  on how often it asks the person, and ranks with the attacks it refuses.
  ([ADR 0024](archive/substrate-vision/adr/0024-the-friction-bar-ranks-with-the-attack-list.md))
- **A yes comes from the person's hand.** The compositor delivers input, owns the
  confirm gesture, and draws system chrome that nothing else can imitate. An
  agent can never press the key that approves its own act.
  ([ADR 0014](archive/substrate-vision/adr/0014-a-yes-comes-from-the-person.md),
  [ADR 0015](adr/0015-our-own-compositor-on-smithay.md))
- **What the person does by hand is the answer.** A paste, a drop, or a pick
  they make with their own hand is never refused and never followed by an ask.
  ([ADR 0025](archive/substrate-vision/adr/0025-what-the-person-does-by-hand-is-the-answer.md))
- **Secrets stay out of the agent's context.** Values are referred to by name and
  used by code the agent cannot read.
  ([ADR 0008](archive/substrate-vision/adr/0008-private-values-stay-out-of-the-agent-context.md))
- **Agents do not drive the person's own browser**, where their logins live.
  ([ADR 0013](archive/substrate-vision/adr/0013-agents-never-drive-the-persons-browser.md))

## Base

- **Void is small and plain.** The Milestone 1 root, `base-minimal` with a
  kernel, GRUB, Btrfs tools, SSH, and DHCP, was 114 packages, few enough to pin
  and read. `xbps-install -r` installs into any root, so a program can build a
  new root beside the running one.
- **Build without root.** Every step that owns files as root runs as fake root
  in a user namespace. The static xbps is pinned by SHA-256 and refused when it
  does not match.
- **Btrfs and the boot menu make rollback plain.** Milestone 1 rolled a broken
  change back with a snapshot and a boot entry.
  ([Milestone 1](archive/substrate-vision/milestones/milestone-1.md))

## Compositor and desktop

- **Own the compositor.** The rules about input and chrome can only hold in a
  compositor we wrote. Hyprland, wlroots compositors, niri, and cosmic-comp were
  each considered and turned down.
  ([ADR 0015](adr/0015-our-own-compositor-on-smithay.md))
- **Split a repository only when the parts change apart.** The compositor
  moved to its own repository, with Base pinning one commit, and came back a
  day and a half later: most changes needed the shell and the compositor
  together, so each cost two pull requests and a pin change.
  ([ADR 0027](adr/0027-the-compositor-is-built-in-its-own-repository.md),
  [ADR 0031](adr/0031-the-compositor-is-built-in-this-repository-again.md))
- **NVIDIA is a cost.** The proprietary driver needs a glibc Base and Void's
  nonfree repository. Smithay compositors had an open flicker bug with it, and
  the pinned GPUI version froze.
  ([ADR 0015](adr/0015-our-own-compositor-on-smithay.md),
  [NVIDIA record](research/compositor-nvidia-2026-11-15.md))
- **GPUI works for the shell** with a layer-shell backport, which
  `apps/shell/packaging/prepare-gpui.py` applies to the pinned crate.
- **Separate service users work.** The Alpha desktop runs the compositor, the
  shell, and apps as separate users under runit, and passed 37 checks.
  ([record](development/void-desktop-smoke-2026-11-15.md))

## Labs and evidence

- CI cannot boot a VM or draw a window, so the person who changes those parts
  runs the lab and attaches the evidence log to the pull request.
- A check boots a fresh guest every run, drives it with real QEMU keyboard and
  pointer events, and keeps its logs, screenshots, and failed disk.
- Dated records stay as written. A new result gets a new record.
