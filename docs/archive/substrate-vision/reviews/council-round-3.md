# Council round 3: the round 2 rules, attacked

**Date:** November 14, 2026. **Reviewed:** branch `agent-first`, the working tree
on top of commit `b4581f2`, after the [round 2](council-round-2.md) decisions were
written in.

Three fresh reviewers, the same three models as before, read the branch from one
brief with no opinion supplied. Jim Martin then answered twelve questions drawn
from their reports. This page records the scores, what they found, what was
decided, and what is left to a build.

| Delegate | Round 1 (vision / M1) | Round 2 (vision / M1) | Round 3 (vision / M1) |
| --- | --- | --- | --- |
| Claude Opus | 6 / 5 | 7 / 5 | 7 / 6 |
| Claude Fable | 6 / 5 | 7 / 6 | 7 / 6 |
| GPT Sol | 7 / 6 | 7 / 6 | 7 / 6 |

The round 3 Milestone 1 scores were given before the code fixes listed below.

**Were they satisfied?**

- **Fable:** yes, start building, once two rules are decided, Milestone 2 gains a
  set of ordinary tasks and a measurement of how far marks spread, and the
  overclaims are corrected.
- **Opus:** not yet, but no full review round is needed. Settle four rules in
  writing first, because they decide how the gate and the interpreter work.
- **Sol:** not as written. Fix the listed items narrowly, name Milestone 2's
  attacks, then build.

All three said more writing would not settle what is left.

## What they found wrong

| Finding | Raised by |
| --- | --- |
| Bounded results "cross freely": a task can return a private file one byte at a time, and a steered choice or handle is acted on by an unmarked Agent. | All three |
| "A site named in the task is not open egress" lets private data out in a query string, and allowing a site does not say which acts on the account are allowed. | All three |
| Strings written by strangers reach the Agent unmarked: file names, mail subjects, error text, Tool labels, and anything the person pastes. A voice from a video counts as the person asking. | Opus, Fable |
| A mark can be cleaned through storage: a Tool's own source, and Agent memory. | Fable, Sol |
| A mark on a native sandbox cannot be changed when it opens a file, because the gate does not see the read. | Opus, Sol |
| A build task that sees the output of a replay of real inputs is not clean. | Opus |
| A token obtained with a vault secret was returned to the Tool as an ordinary value. | Sol |
| Nothing said how a task comes to hold a standing grant, and limits the Agent proposes do not bound a steered Agent. | Fable, Opus |
| The threat model left out the provider and harness as the author of every plan and build, credited a stranger's own checks as a defence against a hostile Tool, and did not list wrong labels or a flood of asks. | All three |
| ADR 0014's "no principal can synthesize input" is false on Linux without closing shared X11, the accessibility bus, created input devices, and agent-reachable owner terminals. | Opus |
| Undoing one Tool can break another that depends on it. | Sol |
| Contradictions: a read counted as a crossing; "only the person can widen limits" beside "the Agent proposes limits"; the gate note's flowchart against its text; ADR 0014 against ADR 0013. | Opus, Fable |
| Nothing measures whether the rules leave ordinary work possible. | Fable, Opus, Sol |

## Decisions

Jim answered yes to all twelve.

| # | Question | Decision | Recorded in |
| --- | --- | --- | --- |
| 1 | Bounded results | Nothing crosses freely. The interpreter keeps the label and mark on every result. An act that depends on a marked result is marked. A private result reaches the Agent by a release or inside a budget, zero by default. | [ADR 0008](../adr/0008-private-values-stay-out-of-the-agent-context.md) |
| 2 | Sites | A site is an outside party with a clearance, public by default. It may receive back what it supplied and what the person released to it. | ADR 0008, [ADR 0011](../adr/0011-task-principals.md) |
| 3 | Acting on a logged-in site | Allowing a site approves no act. Every request that can change state waits for the person. Unasked acts go only through an adapter that knows the operation. | [ADR 0013](../adr/0013-agents-never-drive-the-persons-browser.md) |
| 4 | The person's words | Only what the person types or says. A paste arrives as a handle. A request by voice needs the confirm gesture before anything crosses. | ADR 0008, [ADR 0014](../adr/0014-a-yes-comes-from-the-person.md) |
| 5 | Strings from outside | Every string the person did not type and a template did not produce is data with its origin's label and mark. | ADR 0008 |
| 6 | Marks in storage | Marks persist on Tool source and Agent memory. A native sandbox's mark is fixed at start. | ADR 0008, ADR 0011, [ADR 0004](../adr/0004-capability-safe-tools.md) |
| 7 | Builds and real inputs | A build task sees synthetic results only. A replay reaches it as pass or fail. | ADR 0011 |
| 8 | Who holds standing authority | A Tool or a standing task, never the Agent. An ad hoc task that crosses is authorized once at its start. | ADR 0011 |
| 9 | Secrets obtained with secrets | The adapter stores them in the vault and returns a handle. | [ADR 0006](../adr/0006-broker-owns-all-egress.md) |
| 10 | Hardening the "yes" | Close the four Linux mechanisms; a short delay before an ask accepts the gesture; asks rate-limited per task. | ADR 0014 |
| 11 | Milestone 2 scope | Named attacks, a set of ordinary tasks, a measurement of mark creep, and the flow rules built first. | [Milestone 2](../milestones/milestone-2.md) |
| 12 | What agents want from the gate | A dry run, refusals that name the rule and limit, idempotency keys, and reading a handle's label and mark. | [ADR 0003](../adr/0003-security-is-deterministic.md) |

Choices made while writing these up, not put to Jim as questions:

- **Undo is a change like any other.** It re-runs the checks of dependent Tools
  and the person confirms it.
- **A research task's objective is released when the task is authorized.** The
  Agent's context is at most personal and a web site is cleared for public, so
  without this no search could ever be sent.
- **Inside the Agent's context, "depends on" is the request.** Once the Agent has
  read a result from a marked task, every later act in that request counts as
  marked. The interpreter tracks dependence exactly; a model's context cannot.
- **"Standing task"** is the name for a recurring task the person defined.
- The round 2 choice of "a standing rule for a named source" is narrowed: it now
  needs an adapter that knows the operation.

## Milestone 1 code

Five code defects and one error in the state note, all reproduced by the
reviewers and all fixed. Each code fix has a unit test that fails without it. The
suite went from 50 to 56 tests. A fresh VM run
passed 22 of 22 checks; it shows the main loop still works and does not exercise
the fixes.

| Defect | Raised by |
| --- | --- |
| A declared file path containing `--id gen-3 ` made a failed apply report success and record the wrong default. | All three |
| A non-ASCII file path caused a panic after changes were written. | Sol, Fable |
| A newline in a file path printed a forged line in the plan. | Fable |
| A FIFO at `/etc/hostname` hung `plan`, and would hang an apply holding the lock. | Opus, Fable |
| A bind mount was never unmounted if the next setup step failed. | Sol, Opus |
| The state note said rollback reads the menu when it relies on a flag. | Fable |

The bind-mount fix introduced a hazard: if `--make-rslave` failed on a host whose
mounts are shared, the new cleanup could unmount the host's own `/dev` submounts.
[Round 4](council-round-4.md) reproduced it and it is fixed: such a bind is left
mounted and reported.

## Left to a build

All three put these under "only building can settle". Milestone 2's scope names
the first four. The last needs real sites and is left to Milestone 5.

- Whether an Agent that works through handles, and cannot read a stranger's file
  name, can still finish ordinary work.
- How far marks spread over real Data.
- How many asks and releases a day of ordinary tasks costs.
- How often a bug fix needs a real input, and whether the re-ask becomes a
  rubber stamp.
- Whether a logged-in task browser context survives real sites.

## Ideas not taken up

- Running a promoted Tool in shadow and comparing it with rehearsal, to catch a
  Tool that behaves well only in rehearsal. (Opus)
- Planted values in Data to detect a leak. (Opus)
- Anchoring fleet authority in a security key or the phone, not the desk. (Opus)
- Renewing leases from the phone. (Fable)
- A lasting agent profile per site, apart from the person's. (Fable)
- "Why is this here?" as a query. (Sol, from round 1)
