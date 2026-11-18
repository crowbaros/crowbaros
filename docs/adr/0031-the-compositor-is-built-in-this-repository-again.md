# The compositor is built in this repository again

**Status:** accepted on November 18, 2026, by Jim Martin and Jason Alexander. It replaces [ADR 0027](0027-the-compositor-is-built-in-its-own-repository.md).

[ADR 0027](0027-the-compositor-is-built-in-its-own-repository.md) moved the compositor to its own repository, Fulcrum, and had Base build one commit pinned in `compositor/fulcrum.json`. In the day and a half after, the pin changed nine times. Four of those changes were shell features that needed the compositor too: the launcher (#203), the lock screen (#204), the confirm chord (#206), and password unlock (#207). Each needed two pull requests, two reviews, two CI runs, and a pin change kept in step. The [vision](../VISION.md) ties the compositor still closer to the shell and the Agent: it owns the summon gesture, draws the Agent's surface, and tells the person's own input apart. Most Agent work will change both.

## The decision

- **The compositor's code comes back to [`compositor/`](../../compositor/README.md), with its history.** `git subtree add` imported Fulcrum at `15322b2e7dd4c31725ded0b320dd747b4352d8b3`, its `main` when work there paused, with all 64 commits and their authors and dates. The pull request that adds this record is merged with a merge commit, not squashed, so those commits stay on `main`.
- **Fulcrum stays the compositor's name.** It is the name of the program, the way Mutter or KWin is. The binary is still `crowbar-compositor`, with the same sockets and flags.
- **A compositor change reaches Base when it merges, and not before it passes the Void guest check.** Every pull request that changes how the compositor serves clients runs `lab/void/compositor_guest.py check` and attaches its evidence log, the bar a pin change had under ADR 0027. Reverting the pull request puts Base back on the previous compositor.
- **`compositor/` stays its own Cargo project, with its own CI workflow.** [`compositor.yml`](../../.github/workflows/compositor.yml) runs Fulcrum's format, lint, and test commands, and only when the compositor's code changes. ADR 0028's rules on dependencies are unchanged.
- **The pin goes.** `compositor/fulcrum.json`, `lab/void/test_fulcrum_pin.py`, the lab's fetch and export, and `FULCRUM_REPOSITORY` and `FULCRUM_REVISION` are removed. The lab builds `compositor/` the way it builds the apps, and the evidence log names this repository's commit, and says when the build had changes that were not committed.
- **`crowbaros/fulcrum` is archived, not deleted.** It becomes read-only, and its README points here. Its pull requests stay readable at their links.

## Considered Options

- **Keep ADR 0027.** Rejected: most changes span both repositories, and the Agent's work will span them more. Nothing outside CrowbarOS uses Fulcrum yet, so the split costs more than it gives.
- **A Git submodule.** Rejected for the reasons ADR 0027 gave: every checkout must fetch it, and a working copy can hold changes nobody committed.
- **Import a squashed snapshot.** Rejected: Fulcrum's history explains why its code is the way it is, and a subtree import keeps it at no cost after the import.
- **A folder named `fulcrum/`.** Rejected: the repository's folders are named for what they hold, as `base/`, `apps/`, and `lab/` are, and the links and checks already name `compositor/`.

## Consequences

- One pull request can change the compositor, the shell, and the lab together, and be reviewed and reverted as one.
- The Void guest check is the gate. CI cannot run it, so the person who changes the compositor runs it, as [`AGENTS.md`](../../AGENTS.md) says.
- `compositor/AGENTS.md` keeps the rules that hold only for the compositor: its dependencies, and how its protocol code is generated.
- Fulcrum's dated records in `compositor/docs/` stay as written, links included. Their links into Fulcrum's repository still work, because it is archived and not deleted.
- `git log -- compositor` lists only commits made in this repository. Fulcrum's commits are reached through the import's merge commit: `git log <merge>^2`.
- Fulcrum can move out again with `git subtree split` if someone outside CrowbarOS needs it on its own, or when its interface to the shell and the Agent rarely changes.
