# Council round 5: friction

**Date:** November 16, 2026. **Reviewed:** `main` at commit `d6a01a4`.

Rounds 1 to 4 asked whether the design is secure. This round asked a different
question. Jim Martin asked the council for honest feedback on whether CrowbarOS
is on the right track, given that it must not overwhelm the person with asks or
refusals, and told the reviewers to keep an open mind because the changes are
cheapest now. Three reviewers, the same three models, read the repository and
changed nothing.

| Delegate | Direction | Friction as designed |
| --- | --- | --- |
| Claude Opus | 7 / 10 | 3 / 10 |
| Claude Fable | 7 / 10 | 3 / 10 |
| GPT Sol | 8 / 10 | 4 / 10 |

Each walked a day of ordinary work through the rules as written. These are
estimates from reading, not measurements.

| Day | Opus | Fable | Sol |
| --- | --- | --- | --- |
| A developer | 10 to 25 interruptions, mostly refusals | 10 to 25 answers, 10 to 30 refusals | 5 approvals, 1 refusal |
| A person who is not a developer | 25 to 45, with web errands | 20 to 40 | At least 17, plus browser asks it did not count |

Sol's developer day is lower mainly because it did not count pastes from the
browser into the editor, which the labels code refuses.

**Were they satisfied?** No. All three said the direction is right and that the
friction as designed is not livable. All three said the core should stay: the
deterministic gate with no model on the trust path, broker-owned egress and the
vault, task principals with no standing authority for the Agent, authority
outside the State, fork, preview, promote, the powerbox, workbenches, and the
Allowance. What they would change is a handful of rules added in the rounds that
looked only at security.

The findings were then settled in a design session of thirty questions with Jim.
No reviewer has read the edits that record the answers.

## Findings and how each was closed

| Finding | Raised by | Closed in |
| --- | --- | --- |
| Friction has no pass mark, and the vision says security wins every conflict. | All three | [ADR 0024](../adr/0024-the-friction-bar-ranks-with-the-attack-list.md): the friction bar ranks with the attack list, with numbers set before Milestone 2 runs. Where no design passes both, the capability gives way. |
| Every request on a logged-in site that can change state asks. | All three | [ADR 0013](../adr/0013-agents-never-drive-the-persons-browser.md): one answer for each task and each site. Banks, mail providers, and identity providers are adapter-only. |
| The person's own paste, pick, or drag is refused. | All three | [ADR 0025](../adr/0025-what-the-person-does-by-hand-is-the-answer.md): what the person does by hand is the answer. A value moved by hand is released and keeps its mark. [ADR 0019](../adr/0019-the-origin-table-fails-closed.md) and [ADR 0020](../adr/0020-the-first-compositor-is-small-on-purpose.md) follow. |
| The Agent cannot read a marked result, so each follow-up is a new task it directs blind. | All three | [ADR 0008](../adr/0008-private-values-stay-out-of-the-agent-context.md): each conversation is a compartment, and one the person points at a source may read it. |
| Every ad hoc task asks at its start, lookups included. | Sol, Fable | [ADR 0011](../adr/0011-task-principals.md): a lookup holds only the typed words and asks nothing. A task asks once for an act, never twice. |
| A passkey or a security key for a small widening, such as one new host. | Opus, Fable | [ADR 0026](../adr/0026-asks-wait-and-take-one-of-three-answers.md): the confirm gesture for authority that lasts this run, a signature for authority that lasts for good. [ADR 0017](../adr/0017-a-pinned-catalog-and-workbenches.md) follows. |
| The full chord for every answer, previews and checks included. | Opus, Fable | ADR 0026: a plain answer for what stays inside the undo boundary. [ADR 0001](../adr/0001-intent-is-the-source.md) and [ADR 0007](../adr/0007-fork-verify-promote.md): the checks and the preview take one answer together. |
| The eleventh ask in an hour is refused, not queued, and an ask takes the keyboard when it appears. | Sol, Opus | ADR 0026: an ask waits, and past the limits it is held in one line. [ADR 0018](../adr/0018-one-gesture-answers-one-ask.md) and [ADR 0021](../adr/0021-the-gate-decides-from-metadata-and-counts-from-the-ledger.md) follow. |
| A rule with a limit that depends on a value, such as "up to $150", always asks. | Fable, Sol | ADR 0021: a rule with a cap fires without an ask when the amount is under the cap. |
| As built, the gate asks on every egress from the Agent, an ad hoc task, and installed software, though ADR 0017 says everyday use never asks. | Found while checking the reviews | ADR 0011 and ADR 0021: a model request to a cleared provider, and installed software using what its approval showed, do not ask. |

## The one fact that was looked up

Whether ADR 0013's rule could survive depended on how many requests an ordinary
logged-in flow makes that are not a GET or a HEAD. A research task gathered
public evidence. It found roughly 20 to 60 such requests in a flow of a few
minutes, of which 1 to 3 are acts the person meant. The rest are telemetry,
fraud signals, and reads sent by POST. Its confidence in the number is low: it is
an inference from one shop's load test and vendors' telemetry documents, and
nobody publishes a measurement. Its confidence in the direction is high. It found
no product or paper that asks per request. Its sources were not checked again
for this record.

## Decisions

Jim answered yes to every question but one.

| # | Question | Decision | Recorded in |
| --- | --- | --- | --- |
| 1 | When security and low friction cannot both pass | The capability gives way. Security is not loosened and the bar is not raised. | ADR 0024 |
| 2 | What counts against the bar | Every ask, and apart from them every refusal of what the person did by hand or asked for. | ADR 0024, the [glossary](../../GLOSSARY.md) |
| 3 | Which bar applies when | Alpha: the developer's day and each Milestone 2 task. Beta: the day of a person who is not a developer. | ADR 0024 |
| 4 | The person's own hand | A paste, a pick, a drop, a clicked link, or a typed request is never refused and never followed by an ask about that act. | ADR 0025 |
| 5 | When an ask shows | It waits. It never takes the keyboard, and it is never refused for arriving too fast. | ADR 0026 |
| 6 | How many answers | Three: a plain answer, the confirm gesture, a signature, by what is at stake. | ADR 0026, ADR 0014 |
| 7 | What a conversation may read | What the person points it at. It is then marked for life, and its sends ask. | ADR 0008 |
| 8 | A logged-in site | One answer for each task and each site, not one for each request. | ADR 0013 |
| 9 | The numbers | 0, 1, 2, 2, and 1 asks by kind of task; 5 and 8 asks a day; no refusal of what the person did by hand. It is called the friction bar. | ADR 0024, [Milestone 2](../milestones/milestone-2.md) |
| 10 | A value moved by hand | Its label is released and its mark is kept. | ADR 0025, ADR 0019 |
| 11 | A lookup | A compartment of its own that holds only the typed words and what it found. | ADR 0011 |
| 12 | A conversation | Each exchange with the Agent is one. It rises only when the person points it at something. | ADR 0008 |
| 13 | Authority for a run and for good | The confirm gesture for this run, a signature for good. | ADR 0026, ADR 0017 |
| 14 | Public data and no login | Such a compartment may reach any host without asking. | ADR 0011, ADR 0017 |
| 15 | The two lists | The catalog keeps shared hosts and adapter-only sites, changed only by a signature. | ADR 0013 |
| 16 | The signal for a waiting ask | A count in the bar and nothing else. An ask lasts as long as its task. | ADR 0026 |
| 17 | The outbox | Its delay is for acts under a standing rule. An act the person answered leaves at once. | [ADR 0003](../adr/0003-security-is-deterministic.md) |
| 18 | A rule with a cap | It fires without an ask under the cap. The adapter checks the amount. | ADR 0021 |
| 19 | A value put in, and a source pointed at | The first is released. The second raises the conversation, and the provider is cleared once for each category, by signature. | ADR 0008 |
| 20 | Words | **Lookup**, **plain answer**, **signature**. | The glossary |
| 21 | Standing authority on day one | Asked for at first use. A list the person put together takes one signature. At most five signatures on the first day. | ADR 0026, ADR 0024 |
| 22 | What never asks | A model request to a cleared provider within the budget, and installed software using what its approval showed. | ADR 0011, ADR 0021 |
| 23 | A list | One ask may hold several acts of one kind. | ADR 0026, ADR 0018 |
| 24 | Checks and preview | Confirmed together, in one plain answer. | ADR 0001, ADR 0007 |
| 25 | Attacks | Every rule that was loosened gets a named attack in Milestone 2 before more is built. | Milestone 2 |
| 26 | The hand acts the compositor left out | Drag and drop between compartments, the middle click, and Paste in a right-click menu come in. | ADR 0025, ADR 0020 |
| 27 | The vision's paragraph | "Security and low friction both have to pass" replaces "security comes first". | The [vision](../VISION.md) |
| 28 | How it lands | One pull request for the documents. Code follows through issues. | This record |
| 29 | Whether Jason Alexander reads it before it merges | No. Jim merges it. | — |
| 30 | Left out on purpose | See below. | This record |

## Choices made while writing, not put to Jim

Writing the answers down needed these. Each is the narrower reading.

- **How typed words name a source.** The shell matches them to one of the
  person's sources by fixed rules, never by a model, and the broker opens only
  what the reference names. A miss costs a pick. (ADR 0008)
- **A lookup's objective is one whole turn the person typed, unchanged**, and the
  broker checks it against what the shell recorded. (ADR 0011)
- **A lookup started from a conversation that has risen asks.** Question 7
  already says that conversation's sends ask. (ADR 0011)
- **A task that composes its act later starts with no ask.** The bar gives a
  task with one act that leaves one ask, so the start and the act cannot each
  take one. (ADR 0011)
- **The shell carries a plain answer**, and the compositor carries every other
  yes. (ADR 0026)
- **When an ask opens in place:** the window that holds the keyboard belongs to
  the conversation or compartment the ask came from, and the person's last input
  to it was the request. (ADR 0026)
- **A wide compartment keeps its signature**, though it lasts for one start.
  (ADR 0026)
- **A compartment whose label was fixed at start takes the mark of a value moved
  in by hand.** The broker records the mark before it delivers the value. This
  reverses, for that one case, an option ADR 0019 rejected. (ADR 0025)
- **A new catalog snapshot's changes to the two lists take one signature.**
  (ADR 0013)
- **Work handed back to the person is reported beside the two counts.**
  (ADR 0024)
- **Milestone 2 gains more attacks than the five named in question 25.** The
  rule in that question asks for one for each loosened rule, which adds A22, A25,
  A28, A29, A30, and B8.
- **Milestone 2 builds the fixed rules that name a source**, and measures how
  often they miss and how far marks spread by hand.
- **The bar's hint for a paste stays**, for a read with no act of the person's
  behind it. (ADR 0020)

## What this round gave up, on purpose

Each is in the [threat model](../threat-model.md) under what remains.

- A person tricked into pasting or typing something harmful.
- An app the person clicks in can read what they last copied.
- A private value the person pastes into an app can go wherever that app can
  send it.
- While a task runs on a logged-in site, an injected page can make other changes
  on that one account. What the task holds for the site can reach a shared host.
- A real key the person typed or pasted into a file in a public project can be
  sent anywhere by an injected coding agent.
- A task under a rule with a cap can tell whether its act went through.
- A conversation the person pointed at hostile text may be steered in what it
  says and proposes.
- A plain answer can be made up by a bug in the shell, and a click can be
  arranged under a moving cursor, for what can be undone.
- The person no longer sees an ask the moment it is raised.

## Not done in this round

- **No code changed.** The gate still refuses the eleventh ask and asks on every
  egress from the Agent, an ad hoc task, and installed software. The labels crate
  still refuses a value above a fixed compartment. The compositor's second and
  fourth slices are not built. `lab/adversarial/README.md` still lists the
  earlier attack rows. Each change follows through its own issue.
- **Issues 71, 87, 112, and 166 are not updated yet.** Issue 71 is marked ready
  and still describes the ask as ADR 0018 first had it.
- **Neither day's script is written.**

## For Milestone 2 and the livability review to answer

- Are five asks on an ordinary day, and eight on a day that makes a Tool, the
  right numbers?
- How far do marks spread once the person's pastes carry them?
- How often do the fixed rules miss the source a person's words named?
- What does a steered conversation cost a person who pointed it at hostile mail?
- With asks waiting and none open, should the confirm chord open the first one
  instead of locking the session?
- Do the ask, the paste message, or the chord's table change again
  ([ADR 0022](../adr/0022-the-compositor-is-built-beside-milestone-2.md))?

## Left out on purpose

- **Packaged standing tasks from the catalog**, such as "add events from my mail
  to my calendar": Beta.
- **Counting requests on real sites with real accounts.** It would tune the
  shared list before Milestone 5, and it blocks nothing now.
- **An interface for the budget on private results reaching the Agent.** The
  budget stays at zero.
- **A setting for the ceiling on tasks that do not cross.** There is one
  default.
- **How a conversation's frame, the bar's count, and a list ask look.** That is
  Jason's design work.
