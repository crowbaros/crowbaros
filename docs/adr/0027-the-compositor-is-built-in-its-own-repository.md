# The compositor is built in its own repository, and Base pins one commit

**Status:** proposed on November 17, 2026, by Jason Alexander. It takes effect when Jim Martin agrees, and the pull request that carries it merges then.

[ADR 0015](0015-our-own-compositor-on-smithay.md) put the compositor in `compositor/`, as a Cargo project of its own outside the root workspace. [ADR 0020](0020-the-first-compositor-is-small-on-purpose.md) started it clean there, and slice 1 is built. Five slices remain, and then the work after a first release: input methods, window sharing, more layouts, and more hardware. This record moves the code to a repository of its own, named Fulcrum. Everything else in ADRs 0015, 0016, 0020, 0022, and 0023 stands.

## The decision

- **The compositor's code moves to Fulcrum, with its history.** `git subtree split` of `compositor/` at commit 30fadd8 kept the seven commits that touched it, with their authors and dates. The tree of the last one is that directory, byte for byte. Fulcrum tags it `import/crowbaros-30fadd8`.
- **CrowbarOS keeps the decisions.** Who the compositor's clients are, what each is offered, what it draws, and how a person confirms stay in this repository's records and attack lists. A change to any of them gets its record here before the code changes in Fulcrum. The slice issues stay in this tracker, and Fulcrum's pull requests link to them.
- **Base builds one Fulcrum commit.** [`compositor/fulcrum.json`](../../compositor/fulcrum.json) names the repository and a full commit ID. A branch or a tag is not a pin, because it can move.
- **The lab builds the commit, not a working copy.** `lab/void/compositor_guest.py` fetches the commit into `.lab/fulcrum.git` and writes its tree to `.lab/fulcrum/<commit>/` with `git archive`, and the Void build root builds and tests that tree offline. `FULCRUM_REPOSITORY` and `FULCRUM_REVISION` point the lab at a Fulcrum change that is not pinned yet. The evidence log names both repositories' commits, and says when the Fulcrum one is not the pinned one.
- **A change to the pin is a pull request of its own.** It names the Fulcrum commits it takes in and attaches the guest check's evidence log at the new commit. Reverting it puts Base back on the previous compositor.
- **ADR 0022's rule carries over.** Every Fulcrum pull request that changes the compositor runs the Void guest check against its commit. So does every bump of the Smithay pin, which keeps [ADR 0020's rules](0020-the-first-compositor-is-small-on-purpose.md#the-smithay-pin).
- **Nothing a client or a service sees changes.** The binary is still `crowbar-compositor`, with the same sockets and flags. A rename is a change of its own, in both repositories.

## Considered Options

- **Keep the compositor in `compositor/`.** One pull request could change a decision and the code together. Rejected: a compositor change would reach Base when it merges, not when Base takes it, and Fulcrum's plan has the compositor released on its own, with its own CI and a published record of what each release supports.
- **A Git submodule.** It pins a commit too. Rejected: every checkout and CI job must fetch it, and the offline Void build would read a working copy that can hold changes nobody committed. The lab's export builds only what the commit holds.
- **A copy kept in step by a script.** Rejected: two copies drift, and neither says which is the source.

## Consequences

- There are two repositories. Work that needs both lands in Fulcrum first, then a pin change here.
- This repository's CI no longer builds the compositor; Fulcrum's CI runs the same commands. `lab/void/test_fulcrum_pin.py` checks here that the pin names a full commit, and that the lab builds a commit's tree and nothing from a working copy.
- Fulcrum must be published at the pin's repository before anyone without a local clone can build the compositor. Until then `FULCRUM_REPOSITORY` names that clone.
- `compositor/README.md` stays, so every link to it still lands somewhere that says where the code went. Dated records that name a file under `compositor/` stay as written. That file is in Fulcrum at the import tag.
- ADR 0015's line about where the compositor lives points here.
