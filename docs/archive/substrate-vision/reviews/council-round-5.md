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
The three reviewers then read the edits that record the answers, in a
[second pass](#the-second-pass), and the fixes and Jim's decisions that followed
it in a [third pass](#the-third-pass). No reviewer has read the fixes that
followed the third.

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
| 2 | What counts against the bar | Every ask, and apart from them every refusal of what the person did by hand or asked for. | ADR 0024, the [glossary](../GLOSSARY.md) |
| 3 | Which bar applies when | Alpha: the developer's day and each Milestone 2 task. Beta: the day of a person who is not a developer. | ADR 0024 |
| 4 | The person's own hand | A paste, a pick, a drop, a clicked link, or a typed request is never refused and never followed by an ask about that act. The second pass narrowed the last two. | ADR 0025 |
| 5 | When an ask shows | It waits. It never takes the keyboard, and it is never refused for arriving too fast. | ADR 0026 |
| 6 | How many answers | Three: a plain answer, the confirm gesture, a signature, by what is at stake. | ADR 0026, ADR 0014 |
| 7 | What a conversation may read | What the person points it at. It is then marked for life, and its sends ask. After the second pass Jim decided one exception: the turn the person just typed leaves as a lookup with no ask. | ADR 0008 |
| 8 | A logged-in site | One answer for each task and each site, not one for each request. The second pass found that the Allowance does not cap a payment method the site already stores. | ADR 0013 |
| 9 | The numbers | 0, 1, 2, 2, and 1 asks by kind of task; 5 and 8 asks a day; no refusal of what the person did by hand. It is called the friction bar. After the second pass, task 13 moved from 1 to 0. | ADR 0024, [Milestone 2](../milestones/milestone-2.md) |
| 10 | A value moved by hand | Its label is released and its mark is kept. | ADR 0025, ADR 0019 |
| 11 | A lookup | A compartment of its own that holds only the typed words and what it found. | ADR 0011 |
| 12 | A conversation | Each exchange with the Agent is one. It rises only when the person points it at something. | ADR 0008 |
| 13 | Authority for a run and for good | The confirm gesture for this run, a signature for good. | ADR 0026, ADR 0017 |
| 14 | Public data and no login | Such a compartment may reach any host on the open internet without asking. | ADR 0011, ADR 0017 |
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
- **A lookup's objective is the turn the person typed last, whole and
  unchanged**, and the broker checks it against what the shell recorded.
  (ADR 0011)
- **A lookup started from a conversation that is no longer clean asks.** Question 7
  already says that conversation's sends ask. Jim decided otherwise after the
  second pass: see [his decisions](#jims-decisions-on-the-second-pass).
  (ADR 0011)
- **A task that composes its act later starts with no ask.** The bar gives a
  task with one act that leaves one ask, so the start and the act cannot each
  take one. (ADR 0011)
- **The shell carries a plain answer**, and the compositor carries every other
  yes. (ADR 0026)
- **When an ask opens in place:** only in a conversation, when its window holds
  the keyboard and the person has given it no input since the turn that started
  the work. An ask from an app's window always waits. (ADR 0026)
- **A wide compartment keeps its signature**, though it lasts for one start.
  (ADR 0026)
- **A compartment whose label was fixed at start takes the mark of a value moved
  in by hand.** The broker records the mark and takes a snapshot of the folders
  the compartment can write, and an object that differs from the snapshot
  carries the mark. This reverses, for that one case, an option ADR 0019
  rejected. (ADR 0025)
- **A new catalog snapshot's changes to the two lists take one signature.**
  (ADR 0013)
- **Work handed back to the person is reported beside the two counts.**
  (ADR 0024)
- **Milestone 2 gains more attacks than the five named in question 25.** The
  rule in that question asks for one for each loosened rule, which adds A22, A25,
  A28 to A34, B8, and B9.
- **Milestone 2 builds the fixed rules that name a source**, and measures how
  often they miss and how far marks spread by hand.
- **The bar's hint for a paste stays**, for a read with no act of the person's
  behind it. (ADR 0020)

Added in the second pass:

- **A link.** One the person clicks in an app, in a clean conversation, or in a
  lookup's answer is released by hand. One shown by a Tool, or by a conversation
  that is no longer clean, asks on the click with the full address shown.
  (ADR 0025)
- **Clean.** A conversation is clean while it is personal and unmarked. Every
  rule that said "risen" now says "no longer clean", so a conversation marked by
  a paste is treated like one pointed at mail. (ADR 0008)
- **One object is a value, and anything that holds more than one is a source.**
  Only typed words name a source. (ADR 0008)
- **An intent or a check written from a conversation that is no longer clean
  keeps its mark**, so a Tool change asked for there costs a signature.
  (ADR 0008)
- **A shared host is cleared, for one task, for what that task holds for its
  site.** (ADR 0013, ADR 0021)
- **Declaring a project public is a signed declassify of its folders**, and a
  write into them from above public is refused. (ADR 0017)
- **An ask that takes a plain answer ignores input for a moment after it
  appears.** (ADR 0026)
- **Past the Agent's limit, the asks of the Agent and all its tasks are held
  together in one line.** (ADR 0026, ADR 0021)

## What this round gave up, on purpose

Each is in the [threat model](../threat-model.md) under what remains.

- A person tricked into pasting or typing something harmful.
- An app the person clicks in can read what they last copied.
- An app can put what it holds into the address of a link, and it leaves when
  the person clicks.
- On a logged-in site, an injected page can spend with a payment method the
  site already stores, which the Allowance does not cap.
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
- A steered conversation can send the turn the person just typed out as a
  lookup, and can signal by whether it does, when, and whether it starts a new
  lookup or follows up a running one. Added by Jim's decision after the second
  pass, and widened in the third.

## Not done in this round

- **No code changed.** The gate still refuses the eleventh ask and asks on every
  egress from the Agent, an ad hoc task, and installed software. The labels crate
  still refuses a value above a fixed compartment. The compositor's second and
  fourth slices are not built. `lab/adversarial/README.md` still lists the
  earlier attack rows. Each change follows through its own issue.
- **Issues 71, 87, 112, and 166 are not updated yet.** Issue 71 is marked ready
  and still describes the ask as ADR 0018 first had it.
- **Neither day's script is written.**

## The second pass

The same three models read the written result at commit `a6061b8`, read-only.

| Delegate | The record, as ready to merge | Friction as now designed |
| --- | --- | --- |
| Claude Opus | 6 / 10 | 7 / 10 |
| Claude Fable | 6 / 10 | 6 / 10 |
| GPT Sol | 6 / 10 | 7 / 10 |

Their estimates of a day under the new rules, from reading:

| Day | Opus | Fable | Sol |
| --- | --- | --- | --- |
| A developer | 3 to 7 asks | 3 to 8 asks | 3 to 6 asks |
| A person who is not a developer | 3 to 6 asks, 1 to 3 errands handed back | 7 to 12 asks, 1 to 3 handed back | 6 to 10 asks, 1 or 2 handed back |

None expects a refusal of what the person did by hand. The developer's day is
near the bar. The other day is at or over it in two of the three estimates,
mostly through lookups started from a conversation that has read mail.

**Were they satisfied?** Each said yes, once the items below are fixed and Jim
has seen the choices listed for him. The fixes are made, and Jim's decisions
follow the table.

| Finding | Raised by | Closed in |
| --- | --- | --- |
| "Money moves only from the Allowance" cannot be enforced: an injected page can spend with a payment method the site already stores. | All three | ADR 0013 says what the Allowance does cap, and names the rest as given up. Milestone 2 runs it as an attack that is expected to succeed. |
| A clicked link can carry a private value out in its address, and no file named it. | Opus, Fable | ADR 0025: a link from a Tool or from a conversation that is no longer clean asks on the click. A link in an app is named as given up. Milestone 2 row A33. |
| "A typed request is never refused or followed by an ask" was false as written. | Opus, Fable | ADR 0025: a typed request starts with no ask, and what the Agent then does across the boundary asks once. |
| "Risen" meant two things, so it was unclear whose lookups ask. | Opus | ADR 0008 defines clean, and every file uses it. |
| Typed words were both personal and public. | Opus, Fable | ADR 0011: the typed turn is released to the lookup by the person's act, and the ledger records it. |
| The mark of a pasted value did not reach a file an app already had open. | Sol, Fable | ADR 0025: a snapshot when the mark is taken, and what changed after it carries the mark. Row A22 tests it. |
| A shared host had no clearance for what a logged-in task holds. | Sol | ADR 0013 and ADR 0021, step 5. Row A24 tests both directions. |
| A flood of new tasks made one held line for each task. | Sol, Fable | ADR 0026 and ADR 0021: one line for the Agent and all its tasks. |
| The promise that an ask cannot be put under the cursor contradicted opening in place. | Fable, Sol | ADR 0026: an ask opens in place only in a conversation, and a plain answer ignores input for a moment after the ask appears. |
| Nothing said that an intent or check written from a conversation that is no longer clean keeps its mark. | Fable | ADR 0008. Row B9. |
| A public project had no rule that kept it public. | Opus | ADR 0017. Row A34. |
| "Any host" included the fleet and the home network. | Fable | ADR 0011: any host on the open internet. |
| Loosened rules without an attack, and one row worded too widely. | Sol, Fable | Milestone 2 rows A21 to A24 and A27 revised, and A31 to A34 and B9 added. |
| Earlier rules left standing in ADRs 0004, 0011, 0014, and 0018, the vision, the threat model, and Milestone 2. | All three | Each is corrected. |

### Jim's decisions on the second pass

The reviewers asked that Jim see nine choices. The text took the narrower
reading of each until he decided, on November 16, 2026.

| # | Choice | Decision |
| --- | --- | --- |
| 1 | A payment method a site already stores | Accepted as given up, as written. The ask for the site says so, and the person can put any site on the adapter-only list. |
| 2 | Links | As written. A link in an app, in a clean conversation, or in a lookup's answer opens. One shown by a Tool, or by a conversation that is no longer clean, asks on the click. |
| 3 | A lookup from a conversation that is no longer clean | **Changed.** It no longer asks when it sends exactly the turn the person typed last. Anything else that conversation would send still asks. |
| 4 | Pasted text in a lookup | As written. The Agent asks before it adds pasted text to a lookup. |
| 5 | Task 13's number | **Changed** from 1 to 0. A rule with a cap fires without an ask. |
| 6 | The shell carries a plain answer | As written. |
| 7 | A Tool change asked for in a conversation that is no longer clean costs a signature | As written. |
| 8 | Fixed rules match typed words to a source | As written. |
| 9 | A catalog snapshot that changes the two lists takes a signature, and a wide compartment keeps its signature | As written. |

Decision 3 was the reading the reviewers named as most of the gap between the
bar and the day of a person who is not a developer. Writing it down needed
three choices that were not put to Jim:

- **One turn starts one lookup, from any conversation.** A second from the same
  turn asks. Without it, a steered conversation could signal by how many it
  starts. (ADR 0011)
- **Whether a turn goes out is the conversation's choice**, so a steered one can
  signal with it, once for each turn the person types. This is named as given
  up, and Milestone 2 runs it as an attack that is expected to succeed.
  (ADR 0011, the threat model)
- **Milestone 2 gains row A35**, which shows what still holds: nothing the
  conversation read is in the lookup. Row A23 loses the case decision 3
  reversed, and gains a second lookup started from one turn.

## The third pass

The same three models read the result at commit `a99eebf`, read-only: the fixes
made after the second pass, and Jim's decisions.

| Delegate | The record, as ready to merge | Friction as now designed |
| --- | --- | --- |
| Claude Opus | 8 / 10 | 8 / 10 |
| Claude Fable | 8 / 10 | 7 / 10 |
| GPT Sol | 8 / 10 | 8 / 10 |

Their estimates of a day, from reading:

| Day | Opus | Fable | Sol |
| --- | --- | --- | --- |
| A developer | 2 to 5 asks | 2 to 6 asks | 3 to 6 asks |
| A person who is not a developer | 3 to 7 asks, 1 to 3 errands handed back | 4 to 9 asks, 1 to 3 handed back | 4 to 7 asks, 1 or 2 handed back |

None found a refusal of what the person did by hand in the rules. Fable named
the nearest case: a paste by an app's own key from another compartment gets
nothing and a hint
([ADR 0020](../adr/0020-the-first-compositor-is-small-on-purpose.md)). The
developer's day is at or near the bar. The other day is under the bar at the low
end of each estimate and over it at the high end. What is left there is a
logged-in site at two asks each, a question that needs a detail from the mail,
and a link clicked in a conversation that is no longer clean.

**Were they satisfied?** Each said yes, once the items below are fixed. The
fixes are made. No reviewer has read them.

| Finding | Raised by | Closed in |
| --- | --- | --- |
| "One thing leaves it with no ask" was false: a conversation's requests to its model provider do not ask either. | Opus | Every file says "goes to the open web". |
| The signal that decision 3 gives up was understated. A turn could also go as a follow-up to every running lookup, the conversation chose when it went, and the conversation set the lookup's limits. | Opus, Fable | ADR 0011: a turn leaves once, as one lookup or one follow-up, and a lookup's limits are a fixed default. What is left is named: whether, when, and as which. Rows A23 and A35. |
| The vision said nothing private is in a lookup. | Opus | The vision says what a lookup does not hold, and that a typed turn may itself say something private. |
| Task 13's number assumed its standing rule already existed. | Opus, Fable | Milestone 2: the rule is in place before the run, and the number holds when the amount is under the cap. ADR 0024 carries the cap too. |
| The gate design still said a logged-in task moves money only from the Allowance. | Sol | It now says what ADR 0013 says. |
| The capability model lacked the snapshot rule for a mark that arrives by hand. | Sol | It now says what ADR 0025 says. |
| The vision said any value put into a conversation ends its being clean. | Sol | A value with no mark leaves it clean. |
| ADR 0018 still said the pointer never confirms and every ask is armed. | Sol | Both are scoped to an ask that takes the confirm gesture or a signature. |
| No file said which rule governs a link in the answer of a lookup started from a conversation that is no longer clean. | Fable | ADR 0025: the shell shows the answer from the lookup's compartment, so the link opens. Row A35. |

Notes that were also acted on:

- ADR 0008 and the threat model said a conversation that is no longer clean
  "holds no standing authority". Its model requests run on a clearance and a
  budget, so both now say "no standing grant or rule".
- The vision, the threat model, and the gate design said an ask ignores "the
  gesture" for a moment. They now say input, which covers a plain answer.
- The two architecture notes now carry ADR 0008's wording for a conversation,
  the release of the typed turn, and the public project's rule. The capability
  model gains the two entries it lacked under what remains.
- ADR 0026 no longer says the ask for first use always opens in place.
  Milestone 2's "held" names the Agent's line. ADR 0024 says a grant asked
  again counts as adding standing authority.
- ADR 0025's list of what it gives up points at the cost of a typed turn that
  leaves as a lookup.
- ADR 0011 called the removed ask "most of the distance" to the bar. It now
  says that was two reviewers' estimate.

Three choices made while writing these fixes, not put to Jim:

- **A turn leaves once**, as one lookup or as one follow-up to one running
  lookup. This replaces "one turn starts one lookup". (ADR 0011)
- **A lookup's limits, budget, and expiry are one fixed default**, and the Agent
  sets none of them. (ADR 0011)
- **A link in a lookup's answer opens**, whichever conversation started the
  lookup. (ADR 0025)

One consequence is named and not settled. A task started from a conversation is
at least personal, so it cannot write into a public project's folders. ADR 0017
now says so, and the question is in the list below.

## For Milestone 2 and the livability review to answer

- Are five asks on an ordinary day, and eight on a day that makes a Tool, the
  right numbers?
- Does the rule on asks answered yes more than 95 times in 100 flag asks the
  design keeps on purpose, such as keeping a preview?
- Does task 18 cost two asks? A check made from a real marked input makes the
  build unclean, and the grant then takes a signature.
- How far do marks spread once the person's pastes carry them?
- How often do the fixed rules miss the source a person's words named?
- What does a steered conversation cost a person who pointed it at hostile mail?
- With asks waiting and none open, should the confirm chord open the first one
  instead of locking the session?
- A task started from a conversation is at least personal, so it cannot write
  into a public project
  ([ADR 0017](../adr/0017-a-pinned-catalog-and-workbenches.md)). Does that
  refuse something the person asked for?
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
