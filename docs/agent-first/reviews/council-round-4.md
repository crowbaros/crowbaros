# Council round 4: confirmation

**Date:** November 14, 2026. **Reviewed:** branch `agent-first`, the working tree
on top of commit `b4581f2`, after the [round 3](council-round-3.md) decisions were
written in.

A narrow round. Three fresh reviewers, the same three models, were asked only
whether the round 3 findings had been settled correctly, whether the edits had
introduced contradictions, whether the Milestone 2 scope tests what it should,
and whether the round 3 code fixes hold.

| Delegate | Round 1 (vision / M1) | Round 2 (vision / M1) | Round 3 (vision / M1) | Round 4 (vision / M1) |
| --- | --- | --- | --- | --- |
| Claude Opus | 6 / 5 | 7 / 5 | 7 / 6 | 7 / 6 |
| Claude Fable | 6 / 5 | 7 / 6 | 7 / 6 | 8 / 7 |
| GPT Sol | 7 / 6 | 7 / 6 | 7 / 6 | 8 / 7 |

**Were they satisfied?** All three said broad review should stop and that no
fifth round is needed. Each named a short list of blockers to write in first:
Sol three, Fable three, Opus five. They overlap, and all are closed below. No
reviewer has read the closing edits.

## Blockers and how each was closed

| Blocker | Raised by | Closed in |
| --- | --- | --- |
| What can be observed about a result still crossed freely: the size of a value, the number of handles, whether a step failed, and a replay's pass or fail. A private byte could leave as a file size. | All three | [ADR 0008](../../adr/0008-private-values-stay-out-of-the-agent-context.md): these are results and keep their label and mark. [ADR 0011](../../adr/0011-task-principals.md): the replay verdict is capped per re-derive. |
| The Agent's mark lasted for "the request", which was defined nowhere. A steered choice read in one request could be used in the next, and the conversation history kept no mark. | All three | ADR 0008: the Agent does not read a marked result. A context that does is marked for as long as it lives, with its history and summaries. A standing task started from it runs marked. |
| The answer given at the start of a task still covered acts composed after the task read an injection. | Fable | ADR 0011: the answer covers fetches and acts whose exact content was shown. Anything composed after a mark asks again. |
| "Starting a research task releases its objective" was not a release: no exact text, no named destination. The Agent's personal context could leave through a standing research task. | Opus | ADR 0011: the ask shows the exact text and where it may go. A standing research task has a fixed objective. |
| Nothing limited a task that crosses nothing: "rewrite every file" asked no one. | Opus | ADR 0011: the person sets a ceiling on tasks that do not cross; above it the task is authorized at its start. |
| The Milestone 2 scope could not be evaluated: "refused" was undefined where the outcome is an ask, three attacks named nothing to observe, one attack was allowed by its own rule, some tasks needed things the milestone excludes, and the round 3 record said it covered real sites. | All three | [Milestone 2](../milestone-2.md) rewritten: fixture services and stand-ins, a definition of "refused", an observable for every attack, tasks labelled by what they need, the origin table as the first deliverable. The round 3 record is corrected. |

## Corrections made

- **Code.** The round 3 bind-mount fix could unmount the host's own `/dev`
  submounts if `--make-rslave` failed on a host with shared mounts. Opus and
  Fable each reproduced it in a mount namespace. A bind that is still shared is
  now left mounted and reported. 57 unit tests pass.
- **Session cookies** stay in the broker; a task browser context cannot read
  them. A page fetched under a login is private to that site. (ADR 0013)
- **Input routes.** ADR 0014 listed four Linux routes for producing input. It now
  says Base must close every route and lists the known ones, including virtual
  input protocols and the remote desktop portal.
- **Ask rate limits** apply per task and across all the Agent's tasks.
- **Voice.** Words that arrive by voice are marked until the confirm gesture.
- **The hardware root** is needed for changes to standing authority, not for the
  limits of one ad hoc task.
- **The Allowance** is standing authority: a marked task spends from it only
  through an adapter rule or the person's answer.
- **Credentials obtained without a vault secret** (a device flow, a sign-up, a
  reset link) also go to the vault. (ADR 0006)
- **A native sandbox's view is a snapshot**, so a file that arrives later is not
  in it. (ADR 0004)
- **"Never above personal"** for the Agent's context is now "personal by
  default": a private value released into it is released to its provider too.
- **Wording:** the glossary gains "bounded result" and "adapter"; the pitch no
  longer says the provider is not trusted; the round 3 record counts five code
  fixes and one document fix.

## Decisions

The first four were choices made while closing the blockers. Jim Martin was then
asked about each, and about ending the review. He answered yes to all five.

| # | Question | Decision | Recorded in |
| --- | --- | --- | --- |
| 1 | What the Agent reads | The Agent never reads a result from a marked task. The shell shows it to the person, and the plan branches on it inside the interpreter. | [ADR 0008](../../adr/0008-private-values-stay-out-of-the-agent-context.md) |
| 2 | Work that crosses nothing | The person sets a ceiling on tasks that do not cross. Above it, the task is authorized at its start. | [ADR 0011](../../adr/0011-task-principals.md) |
| 3 | What a start answer covers | It does not cover what the task composes after it is marked. That act asks again with the exact content. | ADR 0011 |
| 4 | Voice | Words by voice are marked until the confirm gesture. | ADR 0008, [ADR 0014](../../adr/0014-a-yes-comes-from-the-person.md) |
| 5 | A fifth round | None. Review stops here, and Milestone 2 is next. | [Milestone 2](../milestone-2.md) |

One choice was not put to Jim: session cookies stay in the broker
([ADR 0013](../../adr/0013-agents-never-drive-the-persons-browser.md)).

## For Milestone 2 to answer

- Can the Agent do useful work when it cannot change its plan after seeing what
  a marked task found?
- Can a research task work when its sites cannot be named in advance?
- How far do marks spread once Tools run, not only at rest?
- How many asks does booking a slot cost: a release and a request, or one?
- How much can a backdoor inside a Tool's granted needs reach?

## For later

- A managed path may hold a character that reorders a line on screen.
- `crowbar ledger` prints package-manager error text as it is.
- The person's browser profile directory must never fall inside a task's view.
- The pitch page loads its diagram library from a CDN.
