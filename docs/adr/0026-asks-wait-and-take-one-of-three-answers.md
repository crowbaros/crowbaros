# An ask waits for the person, and takes one of three answers

**Status:** accepted on November 16, 2026, by Jim Martin, in the design session after [council round 5](../reviews/council-round-5.md).

[ADR 0014](0014-a-yes-comes-from-the-person.md) gave the person two ways to say yes: the confirm gesture for everyday asks, and a hardware root for a change to authority. [ADR 0018](0018-one-gesture-answers-one-ask.md) showed an ask the moment it was raised, dimmed everything else, gave it the keyboard, and let it expire. [ADR 0021](0021-the-gate-decides-from-metadata-and-counts-from-the-ledger.md) refused the eleventh ask in an hour. Council round 5 found four costs in that. Keeping a preview took the chord. A host needed for one afternoon took a passkey. An ask from a background task took the keyboard from whatever the person was typing. Honest work was refused for asking too fast.

This record changes when an ask is shown, how long it lasts, and what answers it. How the compositor arms an ask, takes the gesture, and carries the yes to the broker stays as ADR 0018 says. [ADR 0022](0022-the-compositor-is-built-beside-milestone-2.md) says a change to the ask gets a record of its own. This is that record.

## An ask waits

- **An ask does not open by itself.** The broker makes it, and the task that raised it pauses. For a Tool, installed software, or a workbench, only the operation that asked waits. The shell's bar shows how many asks are waiting. There is no sound, no popup, and nothing over the person's work, and the keyboard stays where it is.
- **A waiting task says so in its conversation.**
- **The person opens an ask when they choose.** From then on it is shown, armed, and answered as ADR 0018 says.
- **One ask opens in place:** the one for what the person just asked for, in the conversation they are in. It opens by itself when that conversation's window holds the keyboard, and the person has given the conversation no input since the turn that started the work. The shell draws the conversation, so it knows both. The same holds in the shell's own screens, such as the catalog, where the person's pick leads straight to the ask. An ask from an app's window or a workbench always waits: no trusted part can tell which input there led to it.
- **An ask lasts as long as the task or the running compartment that raised it.** It has no shorter expiry.
- **Nothing is refused for asking too often.** ADR 0021's limits stay, as counts. Past one principal's limit, its further asks are held in one line in the list of waiting asks. Past the Agent's limit, the further asks of the Agent and all its tasks are held together in one line, however many tasks raised them. The person can open a line, or dismiss it whole, which declines every ask in it.

## Three answers

What answers an ask follows from two facts the gate already has: whether the effect crosses the undo boundary, and whether the authority lasts.

| Answer | What it is | What it answers |
| --- | --- | --- |
| A **plain answer** | Any ordinary input in trusted UI: a click or a key | What stays inside the undo boundary: keeping a preview whose needs did not change, which also freezes its checks ([ADR 0001](0001-intent-is-the-source.md)), and an undo |
| The **confirm gesture** | The secure key press ([ADR 0018](0018-one-gesture-answers-one-ask.md)) | Each crossing: something leaves the fleet, money moves, or the effect cannot be undone. A release by an ask is one, because a value that has passed cannot be taken back, and so is letting an act leave the outbox before its delay ends. Also authority that lasts only for this run: the start of a task above the ceiling, a task's answer for one logged-in site ([ADR 0013](0013-agents-never-drive-the-persons-browser.md)), and a host added to a workbench until it stops ([ADR 0017](0017-a-pinned-catalog-and-workbenches.md)) |
| A **signature** | A passkey, a security key, or the phone, over the exact content shown | Authority that lasts for good: everything in ADR 0014's rule 3 |

- **How long the authority lasts decides, not how small it seems.** A host added to a workbench's profile for good takes a signature, however minor the host. Nothing has to judge what "small" means.
- **A wide compartment keeps its signature**, though it lasts for one start ([ADR 0016](0016-the-warden-starts-every-compartment.md)). It is given everything the person has.
- **The shell carries a plain answer.** It is input to a surface the shell draws, so the shell tells the broker. The compositor does not arm such an ask or dim the screen for it, and the confirm gesture does nothing on it. The compositor still carries every other yes.
- **An ask that takes a plain answer ignores input for a moment after it appears**, as an armed ask does. A key or a click already on its way when the ask opens answers nothing.

## One ask can hold a list

- **An ask may hold several acts of one kind.** Three drafted replies arrive as one ask. Each is shown in full, the person can strike any of them, and one answer covers the rest.
- **A list that is longer than one screen is paged**, and the compositor arms it only after the last page has been shown, as ADR 0018 already does for a long ask.
- **A strike is carried by the shell, like a decline.** The broker makes the content hash again over what is left, and the ask is armed again. A shell that strikes wrongly costs a new ask, never a false yes.
- **A list for a signature holds only what the person put together:** the apps they chose to install on a new machine, or the hosts they added to a workbench during the week. It is shown in full and takes one signature.

## Standing authority is asked for at first use

- **Nothing is asked up front on a new machine.** The first time something needs standing authority, one ask opens in place, at the moment the person can see what it is for.
- **The first day is held to five signatures** ([ADR 0024](0024-the-friction-bar-ranks-with-the-attack-list.md)).

## What this gives up

- **The person no longer sees an ask the moment it is raised.** A task that needs an answer waits until they look.
- **A plain answer has two fewer defences than the confirm gesture.** A click can be arranged under a moving cursor, and a bug in the shell can make a plain answer up. Both are accepted only for what the system holds a tested undo for, and the ledger records each one.
- **A promote can be undone, but a Tool that holds standing rules can act under them before it is.** A plain answer is accepted for a promote because its needs did not change and its build was clean ([ADR 0011](0011-task-principals.md)). The ask for it opens by itself only in the conversation where the person just asked for the change, and it ignores input for a moment after it appears. Anywhere else it waits until the person opens it.
- **Each item in a list gets less of the person's attention than an ask of its own would.**

## Considered Options

- **An ask opens when it is raised (ADR 0018's rule).** Rejected: a background task could take the keyboard while the person types in their editor.
- **A sound or a popup for a waiting ask.** Rejected: it interrupts as much as the ask did.
- **A short expiry.** Rejected: "answer when you have a moment" would become a failed task.
- **Refuse past the rate (ADR 0021's rule).** Rejected: honest work was refused for how fast it asked, and a refusal counts against the bar as an ask does.
- **Two answers (ADR 0014's rule).** Rejected: the chord for "keep this preview?" teaches the person to press it without reading. ADR 0014 rejected a hardware root for every answer on the same argument.
- **A plain answer for a small crossing.** Rejected: a crossing cannot be undone, so a click arranged under the cursor would cost something real.
- **The confirm gesture for a small standing change.** Rejected: "small" needs a judge, and how long authority lasts is a fact.
- **A separate ask for each act.** Rejected: three replies would cost three asks, and the bar gives one.
- **The compositor carries the plain answer too.** Rejected for now: it would have to know which control on the shell's surface was pressed, which puts the shell's layout into the program that holds the seat.

## Consequences

- [ADR 0014](0014-a-yes-comes-from-the-person.md), [ADR 0018](0018-one-gesture-answers-one-ask.md), and [ADR 0021](0021-the-gate-decides-from-metadata-and-counts-from-the-ledger.md) are revised to match.
- The compositor's second slice ([issue 71](https://github.com/crowbaros/crowbaros/issues/71)) keeps arming, the chord, and the hash. What changes: an ask is shown only when the person opens it or it opens in place, a list ask is armed again after a strike, and an ask that takes a plain answer is never armed. Its tests change with it, and are run again.
- The chord's table in [ADR 0020](0020-the-first-compositor-is-small-on-purpose.md) is unchanged. With asks waiting and none open, the chord still locks the session. Whether it should open the first waiting ask instead is a question for the livability review.
- The shell gains work: the count in the bar, the list of waiting asks with a held line, a list ask with strikes, and asks that take a plain answer. How they look is design work under [issue 22](https://github.com/crowbaros/crowbaros/issues/22).
- The gate holds an ask that is over a limit and no longer refuses it. That change follows through its own issue.
- Milestone 2's attack on flooding changes: the flood is held in one line, and nothing is refused.
