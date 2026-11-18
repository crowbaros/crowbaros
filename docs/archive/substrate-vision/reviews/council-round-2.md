# Council round 2: the revised design and the Milestone 1 fixes

**Date:** November 14, 2026. **Reviewed:** branch `agent-first`, the working tree
on top of commit `b4581f2`, after the [round 1](council-round-1.md) decisions were
written in.

Three fresh reviewers, the same three models as round 1, read the revised branch
from one brief with no opinion supplied. Jim Martin then answered fourteen
questions drawn from their reports. This page records the scores, what they
found, what was decided, and what is still open.

| Delegate | Vision, round 1 | Vision, round 2 | Milestone 1, round 1 | Milestone 1, round 2 |
| --- | --- | --- | --- | --- |
| Claude Opus | 6/10 | 7/10 | 5/10 | 5/10 |
| Claude Fable | 6/10 | 7/10 | 5/10 | 6/10 |
| GPT Sol | 7/10 | 7/10 | 6/10 | 6/10 |

The round 2 Milestone 1 scores were given before the code fixes listed below.

## What all three would keep

- The gate that allows and the monitor that can only pause.
- Authority stored outside the State.
- Fork, verify, promote.
- Task principals.
- The pinned build as part of the source.
- The word "rehearsal".

## What they found wrong in the revised design

| Finding | Raised by |
| --- | --- |
| Labels and the effect of untrusted input did not survive handoffs. A "narrow, typed result" could hold the private value; a summary handed to a "clean task" carried the injection; the Agent read every task's results; a task that had read an injected page could write code into a Tool that held grants. | All three |
| The untrusted-input rule, read literally, cut a task off from its own cloud model after one web page or package. | All three |
| Provisional edits let the code of a Tool with grants be rewritten and go live unconfirmed. | All three |
| Undo meant two things: promoting the previous generation, and an undo that keeps later edits. | All three |
| The browser holds sessions, reads every page, and has open egress, which the design forbids. | All three |
| "The gate checks every use" is false for native code writing through an open file descriptor. | Sol, Fable |
| Nothing said what a grant binds to when a Tool is rebuilt. | Sol, Fable |
| Every crossing ends in the person's answer, and nothing stopped an agent from producing input. | Opus, Fable |
| A check that always passes could carry honest-looking wording, and nothing tied a test result to the build that was promoted. | Sol, Fable |
| A fork "with egress off" cannot be true for a task that thinks with a cloud model. | Raised in the reports |
| There was no threat model. | All three |

## Decisions

Jim answered yes to all fourteen.

| # | Question | Decision | Recorded in |
| --- | --- | --- | --- |
| 1 | Labels across handoffs | Labels and marks survive every handoff. Only handles and bounded results cross freely; anything else needs a release by the person. | [ADR 0008](../adr/0008-private-values-stay-out-of-the-agent-context.md) |
| 2 | What untrusted input costs | Untrusted input is graded (admitted or open). A marked task keeps its model through a broker-built request and loses its standing authority to act or send. | [ADR 0011](../adr/0011-task-principals.md) |
| 3 | Code from a marked task | Build tasks are clean by construction. A grant binds to the Tool's identity, the needs the person saw, and a clean build. | ADR 0011 |
| 4 | Private data and a cloud model | A model provider is a party with a clearance per label. The default is personal. | ADR 0008 |
| 5 | Plan over handles | Adopted as the main way private values stay out of the Agent's context. Prototyped in Milestone 2. | ADR 0008 |
| 6 | Provisional edits | Dropped. Every change is previewed, and one confirmation promotes it. | [ADR 0007](../adr/0007-fork-verify-promote.md) |
| 7 | Unit of undo and promote | The Tool. Change sets, automatic re-run of checks, undo of one change set. Rolling back the whole Environment is a recovery action. | ADR 0007 |
| 8 | Tasks that change no software | A task has a workspace. Data writes are staged and committed together. A fork is only for changes to installed behaviour. | ADR 0007 |
| 9 | What native code is promised | Only what Linux can enforce. A label fixed at start; limits on count and rate hold on broker operations and at commit. | [ADR 0004](../adr/0004-capability-safe-tools.md) |
| 10 | The browser | Agents never drive the person's browser profile. A task gets its own context per task and site, with approved fill. | [ADR 0013](../adr/0013-agents-never-drive-the-persons-browser.md) |
| 11 | What makes a "yes" the person's | Only the shell delivers input; a confirm gesture for everyday asks; a hardware root for authority changes; voice never confirms; trusted UI in Base, not themeable. | [ADR 0014](../adr/0014-a-yes-comes-from-the-person.md) |
| 12 | Checks as data | A check is a scenario in a fixed format, worded by templates. A Base runner binds the result to the build. | [ADR 0001](../adr/0001-intent-is-the-source.md) |
| 13 | Order of work | The adversarial slice (M2) first, on stock Linux. The Base rework (M1b) after it. | [Vision](../VISION.md), Roadmap |
| 14 | Three security defaults | A stranger's Tool runs only as WASM, with its author's reasons shown as claims. One lead machine holds authority and orders promotes. No private Data on an unencrypted disk. | ADR 0004, [ADR 0010](../adr/0010-share-the-pinned-build.md), [ADR 0012](../adr/0012-authority-outside-the-state.md), ADR 0008 |

Choices made while writing these up, not put to Jim as questions:

- **A standing rule for a named source.** A marked task loses its standing rules.
  Without an exception, no recurring act on a web site could ever run unasked. So
  a rule the person wrote for that source by name still applies, and its limits
  are then the only protection.
- **The words.** "Mark" for what a context gets from open untrusted input,
  "release" for the person letting one value pass, "change set", "workspace",
  "preview", "lead machine", "confirm gesture". "Quarantined compartment" is no
  longer used.
- **Label order.** Public, personal, private, with a category on private data.
- **The Agent's own context is never above personal**, whatever its provider is
  cleared for. A task's context can be.
- **A [threat model](../threat-model.md) page** was added.

[Round 3](council-round-3.md) changed two of these: handles and bounded results
no longer cross freely, and the standing rule for a named source now needs an
adapter that knows the operation.

## Milestone 1 code

Nine defects in the round 1 fixes and the code around them. All nine reproduced
and are fixed, each with a unit test. The suite went from 31 to 50 tests. A fresh
VM run passed 22 of 22 checks; it shows the main loop still works and does not
exercise the fixes.

| Defect | Raised by |
| --- | --- |
| A failed directory sync after the boot menu rename still deleted the new generation. | Sol, Fable |
| Nothing flushed the new generation to disk before the boot menu named it. | Opus, Fable |
| Boot files were checked against the host root, and a boot path with a newline could add boot menu entries. | All three |
| The host's `/dev`, `/proc`, and `/sys` were mounted through a symlink if the generation had one in their place. | Fable |
| A mount device starting with `#` became an fstab comment, and a mount target could be `/boot`. | Sol, Fable |
| A declared file named like the engine's temporary files could be deleted silently. | Fable |
| The ledger reader silently repaired invalid bytes. | Sol |
| `plan` and `drift` read a managed path through a final symlink with no type check. | Opus |
| A file with mode 0600 was briefly 0644. | Fable |

Left for Milestone 1b, with the round 1 list, because that code is being replaced:

- The lock stops two applies from running at once but does not refuse an apply
  built from a generation that is no longer the default, so a second apply before
  a reboot drops the first one's changes.
- Two managed paths can name the same file.
- A declared file can land in another section's directory through a symlink.
- A mount target is not checked again after symlinks are resolved.
- `crowbar ledger` exits 0 when the ledger has damaged lines.
- A ledger detail can span more than one line.

## Where the reviewers disagreed

- **Provisional edits.** Sol and Fable would have kept them for edits confined to
  the UI description. Opus would drop them for a preview and one confirmation.
  Decided for Opus's form (question 6).
- **Order of work.** Fable and Sol would run the adversarial slice first, on
  stock Linux. Opus would settle the rules for labels and marks before designing
  the slice. The rules were decided in this round and the slice comes first
  (question 13).

## Not decided

These are in the vision's open questions:

- Who sets a task's effect limits, and what stops the Agent proposing limits that
  are too wide.
- How long the person's answer to an outbox item lasts.
- Who admits the dependency set that build tasks read, and how it is reviewed.
- How much a plan over handles can express.
- How many previews, releases, and asks a person will read in a day.
- Whether agents may drive native apps that have no UI description.
- Losing the lead machine.
- Security updates for a pinned Base.
