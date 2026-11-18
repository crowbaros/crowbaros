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
| The substrate vision ends, and these three stand | [ADR 0029](../docs/adr/0029-crowbaros-starts-again-with-an-agent-at-the-persons-side.md) |

Who the compositor's clients are, what each is offered, and its slices were
decided in ADRs [0016](../docs/archive/substrate-vision/adr/0016-the-warden-starts-every-compartment.md),
[0020](../docs/archive/substrate-vision/adr/0020-the-first-compositor-is-small-on-purpose.md),
[0022](../docs/archive/substrate-vision/adr/0022-the-compositor-is-built-beside-milestone-2.md),
and [0023](../docs/archive/substrate-vision/adr/0023-the-shell-is-offered-a-compartments-protocols-and-layer-shell.md),
now archived. They bind until a new record says what the compositor offers the
shell and the Agent.

Fulcrum's README says what is built, which slice is next, and how to build and
test it. Through commit 30fadd8 the compositor's code was here. Its history moved
with it, so `git log` in Fulcrum shows every commit that touched this
directory.

The [spike](../lab/compositor-spike/README.md) that tested ADR 0015 is not the
compositor. It stays in `lab/compositor-spike/` because the Alpha desktop check
still runs it, and it is deleted when the compositor passes that check.

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
