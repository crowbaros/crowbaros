# The Environment changes by fork, verify, promote, one Tool at a time

Nothing is applied to the live Environment and undone later. A task that changes installed behaviour (a Tool, a service, the desktop) works in a **fork**: a copy-on-write copy of the Environment. The task builds there, and the Base runner proves the checks in rehearsal (ADR 0001). The person sees the result as a **preview**: the forked Tool running in the fork, drawn by the shell and marked as a preview, with no effects outside the fork. One plain answer in trusted UI ([ADR 0026](0026-asks-wait-and-take-one-of-three-answers.md)) **promotes** exactly the build that was previewed, and freezes the checks shown beside it (ADR 0001). A promote that brings new needs, or a build that is not clean, also takes a signature for the grant (ADR 0011, ADR 0014). Nothing unconfirmed goes live.

**A preview stays open across changes.** The person can ask for one change after another, and each rebuild replaces what the preview shows. One plain answer at the end promotes the last build. Nothing before that answer was live.

The unit of change is the Tool, not the whole Environment. Everything an intent produces in the Environment counts as a Tool here: a service, an automation, and each part of the desktop are changed, promoted, and undone the same way. A promote carries a **change set**: the Tools it changes, and the build of each that the task started from. It succeeds when none of those Tools has changed since. Two tasks that worked on unrelated Tools both promote; the runner re-runs the checks of every Tool the combined result could affect, and the person is asked only when two change sets touch the same Tool or a check fails. Each promote makes a new Environment generation.

**Undo** reverts one change set: each Tool in it returns to its earlier pinned build, as a new generation, and later change sets to other Tools stay. An undo is a change like any other. The runner re-runs the checks of every Tool it could affect, and the person confirms it with a plain answer. If a later change set touched the same Tool, or a Tool that depends on this one would fail its checks, the undo is refused and both are shown. **Rolling back** the whole Environment to an earlier generation remains as a recovery action.

A task that changes no installed behaviour gets no fork. Every task has a **workspace**: a private scratch area and a view of the Data it was given. Its writes to Data are staged in the workspace and committed together when the task finishes, inside its effect limits, as one entry in the Data history. Sorting photos is a task with a workspace, not an Environment change.

**Installed software and workbenches write live.** An editor or a dev server cannot wait for a commit at the end, because other programs read its files as it works. So installed software and workbenches ([ADR 0017](0017-a-pinned-catalog-and-workbenches.md)) write straight into the folders they were granted. Btrfs snapshots of each granted folder, and the Data undo window, protect the person. Tasks still stage, wide or not: the rule depends on the kind of compartment, not on how much it was given.

**An update that asks for nothing new is promoted under a standing rule.** The person can set a standing rule once: an update to installed software is promoted without asking when it asks for no new access and its checks pass in a fork. An update that asks for more waits for the person's confirmation. Added by ADR 0017.

A fork has no egress of its own. The task working in it reaches its model and its dependencies only through the broker (ADR 0011).

Revised after council rounds 2, 3, and 5. The first version promoted and undid the whole Environment, gave every task a fork, said forks ran with "egress off", and allowed provisional edits that went live unconfirmed. Until round 5 a promote took the confirm gesture, after a separate confirmation of the checks.

## Considered Options

- **Apply live, undo later (the earlier design).** Rejected: it depends on the person noticing, and it has no answer for two agents changing the Environment at the same time.
- **Promote and undo the whole Environment.** Rejected: two tasks on unrelated Tools would conflict on every promote, and undoing one change would remove every later one.
- **Provisional edits: an edit that adds no authority goes live unconfirmed and expires.** Rejected: an edit that adds no authority can still rewrite the code of a Tool that already holds grants. A preview costs the person one confirmation and removes the case.

## Consequences

- The live Environment has one line of generations. Forks branch from it and return as change sets.
- The engine must know which Tools a change could affect, so it knows which checks to run again.
- The Base engine's apply lock only stops two applies from running at once. It does not refuse an apply built from a generation that is no longer the default. A promote must.
- A preview is bound to the content hash of the build. A promote of anything else is refused.
- A promote can be undone, so a plain answer is enough for it. A Tool that holds standing rules can act under them before an undo, which is why the answer is accepted only for a clean build whose needs did not change (ADR 0011, ADR 0026).
- Ports, devices, and model time are leased to a task by name.
