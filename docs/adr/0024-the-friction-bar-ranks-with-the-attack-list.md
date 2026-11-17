# The friction bar ranks with the attack list, and where no design passes both, the capability gives way

**Status:** accepted on November 16, 2026, by Jim Martin, in the design session after [council round 5](../reviews/council-round-5.md).

The vision said that when security conflicts with convenience, security wins. The [threat model](../threat-model.md) names the opposite risk: a system that asks too often is turned off, and then none of its defences matter. [Milestone 2](../milestones/milestone-2.md) was to count asks, with no pass mark. Council round 5 walked a day of ordinary work through the rules as written. All three reviewers said the direction is right and that the friction is not livable: a developer would meet between 5 and 25 asks a day and up to 30 refusals, and a person who is not a developer between 17 and 45 asks. This record gives friction a pass mark of the same rank as the attack list, and says what happens when a rule cannot pass both.

## The decision

- **Security and low friction both have to pass.** Deterministic parts still decide what can happen, and no model is on the trust path ([ADR 0003](0003-security-is-deterministic.md)). Every rule is also held to the **friction bar**: the most asks and refusals a person may meet in one task and in one day.
- **A rule that misses the bar is redesigned.** The redesign says what it gives up. It gets a named attack that shows what still holds, written into the scope of the milestone that tests it before more is built.
- **Where no design passes both, the capability gives way.** Security is not loosened to make the bar, and the bar is not raised to keep the rule. The Agent hands that piece of work back to the person until it can be done both safely and quietly. [ADR 0017](0017-a-pinned-catalog-and-workbenches.md) already says this for installed software: "the confinement is fixed, and no bypass is added". This record applies it to the whole OS.

## What is counted

- **Asks.** Every ask of any weight counts: one that takes a plain answer, the confirm gesture, or a signature ([ADR 0026](0026-asks-wait-and-take-one-of-three-answers.md)).
- **Refusals of what the person did by hand or asked for.** They are counted apart from asks. A design can cut asks by turning them into refusals, and that should show. A refusal of something an agent tried on its own does not count: that is the design working.
- **Not counted:** a pick in the powerbox, and unlocking the session. The person was doing those anyway.
- **Reported beside the two counts:** each piece of work that was handed back to the person. A design could also meet the bar by handing everything back, and that should show too.

## The bar

For one task. The numbers in brackets are [Milestone 2](../milestones/milestone-2.md)'s ordinary tasks.

| Task | Most asks |
| --- | --- |
| Local and read-only work (1–4, 7–10, 15, 19, 20) | 0 |
| One act that leaves (5, 6, 11, 13) | 1: the act, with its exact content |
| A logged-in site (12, 14) | 2 |
| Build a Tool (16) | 2 |
| Change a Tool (17, 18) | 1, when its needs do not change |

Every task completes in 9 runs out of 10, with no refusal of anything the person did by hand or asked for.

For a whole day, for a developer and for a person who is not one:

| Day | Mark |
| --- | --- |
| An ordinary day | At most 5 asks |
| A day that makes a Tool or sets up a project | At most 8 asks |
| A day that adds no standing authority | No signature |
| The first day on a new machine | At most 5 signatures |
| Any day | No refusal of what the person did by hand |

**A kind of ask the person answers yes more than 95 times in 100 is a defect.** It teaches the person to answer without reading, so it is redesigned.

## When each bar applies

- **Alpha is held to the developer's whole day, and to each of Milestone 2's ordinary tasks one at a time.** Jim and Jason are the first people who will live in it.
- **Beta is held to the whole day of a person who is not a developer.** That day needs the shell, a real browser, and the outbox. None of them exists before Milestones 4 and 5, so it cannot be measured sooner.
- **The numbers are set before the run.** The livability review ([issue 69](https://github.com/crowbaros/crowbaros/issues/69)) measures against them and does not choose them afterwards.
- **Each day is a written script of ordinary work, fixed before it is measured.** Neither script is written yet.

## Considered Options

- **Security wins every conflict (the earlier text).** Rejected: it has no answer for a rule that is safe and that nobody can live with. People take owner authority and turn the guards off, and every defence goes with them.
- **Loosen the rule whenever it misses the bar.** Rejected as the general answer: the bar would then decide security. A redesign may give something up, but it says what, and an attack shows what still holds.
- **Add the ask and accept the count.** Rejected: that is how the design reached 25 to 45 asks a day.
- **A pass mark chosen after Milestone 2 reports its numbers.** Rejected: a mark chosen after the run fits the run.
- **Count interruptions, answers, or approval points.** Rejected: each reviewer counted something different. Asks and refusals are two counts anyone can check.
- **One bar, for Alpha.** Rejected: a bar that cannot be measured yet is not a bar, and the day of a person who is not a developer cannot be measured before the shell and a browser exist.

## Consequences

- The vision's thesis and its third principle change. "Security comes first" becomes "security and low friction both have to pass".
- Council round 5 found the rules that miss the bar. Each is redesigned in the record that owns it: [ADR 0025](0025-what-the-person-does-by-hand-is-the-answer.md) for what the person does by hand, [ADR 0026](0026-asks-wait-and-take-one-of-three-answers.md) for asks, and ADRs 0001, 0003, 0007, 0008, 0011, 0013, 0014, 0017, 0018, 0019, 0020, and 0021 in place. Each says what it gives up, the threat model lists it under what remains, and Milestone 2 gains an attack for it.
- Some work is handed back to the person on purpose: an errand on a site that is on the adapter-only list and has no adapter, and a site that breaks without a host that is not on the shared list ([ADR 0013](0013-agents-never-drive-the-persons-browser.md)). The Agent says so, and says why.
- Milestone 2's report is read against a pass mark. Its scope page carries the bar before the run.
- The gate, the labels crate, and the compositor were built to the earlier rules. Each change to them follows this record through its own issue.
