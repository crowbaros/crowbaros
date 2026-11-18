# Compositor

The CrowbarOS Wayland compositor is built in its own repository,
[Fulcrum](https://github.com/crowbaros/fulcrum), on
[Smithay](https://github.com/Smithay/smithay). It is part of Base and part of
what CrowbarOS trusts ([ADR 0027](../docs/adr/0027-the-compositor-is-built-in-its-own-repository.md)).

This directory holds one thing: [`fulcrum.json`](fulcrum.json), the full commit ID
of the Fulcrum revision that Base builds. CrowbarOS still decides what the
compositor must do, in its decision records:

| Decision | Record |
| --- | --- |
| CrowbarOS owns its compositor | [ADR 0015](../docs/adr/0015-our-own-compositor-on-smithay.md) |
| Who its clients are, and how each connects | [ADR 0016](../docs/adr/0016-the-warden-starts-every-compartment.md) |
| What desktop it is, what each client is offered, and its slices | [ADR 0020](../docs/adr/0020-the-first-compositor-is-small-on-purpose.md) |
| Which rows of that table the shell and the Warden are offered | [ADR 0023](../docs/adr/0023-the-shell-is-offered-a-compartments-protocols-and-layer-shell.md) |
| It is built beside Milestone 2 | [ADR 0022](../docs/adr/0022-the-compositor-is-built-beside-milestone-2.md) |
| It is built in its own repository, and Base pins one commit | [ADR 0027](../docs/adr/0027-the-compositor-is-built-in-its-own-repository.md) |

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
