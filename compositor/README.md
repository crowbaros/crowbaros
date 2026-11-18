# Compositor

The CrowbarOS Wayland compositor is built in its own repository,
[Fulcrum](https://github.com/crowbaros/fulcrum), and written there from the
ground up, with [Smithay](https://github.com/Smithay/smithay) as its reference
([ADR 0028](../docs/adr/0028-the-compositor-is-written-from-the-ground-up.md)).
It is part of Base and part of what CrowbarOS trusts
([ADR 0027](../docs/adr/0027-the-compositor-is-built-in-its-own-repository.md)).
The pinned commit is Fulcrum's own compositor, written from the ground up. It
passed the Smithay-based baseline's 57 tests and the Void guest's checks, and
took the baseline's place (ADR 0028). The baseline is at Fulcrum's
`smithay-baseline` tag.

This directory holds one thing: [`fulcrum.json`](fulcrum.json), the full commit ID
of the Fulcrum revision that Base builds. CrowbarOS still decides what the
compositor must do, in its decision records:

| Decision | Record |
| --- | --- |
| CrowbarOS owns its compositor | [ADR 0015](../docs/adr/0015-our-own-compositor-on-smithay.md) |
| It is built in its own repository, and Base pins one commit | [ADR 0027](../docs/adr/0027-the-compositor-is-built-in-its-own-repository.md) |
| It is written from the ground up, with Smithay as its reference | [ADR 0028](../docs/adr/0028-the-compositor-is-written-from-the-ground-up.md) |
| The substrate vision ends, and these three stand | [ADR 0030](../docs/adr/0030-crowbaros-starts-again-with-an-agent-at-the-persons-side.md) |

Who the compositor's clients are, what each is offered, and its slices were
decided in ADRs [0016](../docs/archive/substrate-vision/adr/0016-the-warden-starts-every-compartment.md),
[0020](../docs/archive/substrate-vision/adr/0020-the-first-compositor-is-small-on-purpose.md),
[0022](../docs/archive/substrate-vision/adr/0022-the-compositor-is-built-beside-milestone-2.md),
[0023](../docs/archive/substrate-vision/adr/0023-the-shell-is-offered-a-compartments-protocols-and-layer-shell.md),
and [0029](../docs/archive/substrate-vision/adr/0029-each-compartment-has-a-budget-in-the-compositor.md),
now archived. They bind until a new record says what the compositor offers the
shell and the Agent.

Fulcrum's README says what is built, which slice is next, and how to build and
test it. Its [plan](https://github.com/crowbaros/fulcrum/blob/main/docs/PLAN.md)
says where each milestone stands, and what waits on CrowbarOS:

- decision records on what Fulcrum built as proposals: the shell's control
  messages for the launcher, attention, and lock; the lock screen's namespace;
  the unlock authority, which the Alpha session's password helper uses; and
  the layout of displays on the control channel;
- a record of what replaces the broker, for asks and their arming, transfers
  between compartments, and the record of each unlock;
- records on capture by the shell and on input devices' settings;
- runs on the RTX 4090, Intel, and Surface machines, which Fulcrum's
  [hardware guide](https://github.com/crowbaros/fulcrum/blob/main/docs/hardware-runs.md)
  describes.

Fulcrum's [picking up the work](https://github.com/crowbaros/fulcrum/blob/main/docs/picking-up.md)
says where the compositor's work paused, how to run its checks from here, how a
change reaches this pin, and how to start each next piece. Through commit 30fadd8 the compositor's code was here. Its history moved
with it, so `git log` in Fulcrum shows every commit that touched this
directory.

The [spike](https://github.com/crowbaros/crowbaros/tree/substrate-final/lab/compositor-spike) that tested ADR 0015 was never the compositor. It was
deleted once the Alpha desktop check passed on the compositor the pin names,
and it is at the `substrate-final` tag.

## The pin

The Void lab builds the pinned commit. It fetches the commit into
`.lab/fulcrum.git`, writes the commit's tree to `.lab/fulcrum/<commit>/`, and
builds and tests it in the Void build root:

```sh
lab/void/compositor_guest.py check
```

A Fulcrum change that is not pinned yet is checked the same way, with the clone
and commit named. The evidence log says that the build is not the pinned one:

```sh
FULCRUM_REPOSITORY=../fulcrum FULCRUM_REVISION=<full commit ID> \
    lab/void/compositor_guest.py check
```

A change to the pin is a pull request of its own. It names the Fulcrum commits
it takes in, and attaches the guest check's evidence log at the new commit.
Reverting that pull request puts Base back on the previous compositor.
