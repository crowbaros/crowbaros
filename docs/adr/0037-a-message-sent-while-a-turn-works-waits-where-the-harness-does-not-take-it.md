# A message sent while a turn works waits where the harness does not take it

**Status:** proposed on November 26, 2026, by Jim Martin. It amends [ADR 0036](0036-lever-drives-the-harnesses-a-person-has-and-crowbaros-builds-none.md).

[ADR 0036](0036-lever-drives-the-harnesses-a-person-has-and-crowbaros-builds-none.md) has an adapter say whether its harness can steer a turn, and has Lever offer the person that and no more, standing in for nothing a harness lacks. The Milestone 3 spec ([#371](https://github.com/crowbaros/crowbaros/issues/371)) followed it: a message sent while a turn works is steered into the turn where the harness can, and refused where it cannot or does not take it.

Refusing it means the person keeps the message, watches for the turn to end, and sends it again. Each client must say why, and say it differently for each harness. A message the harness declined, as when the turn ended just as it came, is lost unless the person notices.

These are the facts as found on November 26, 2026:

- **OpenCode 2.4.1 steers with an extension of its own.** Its `initialize` answer says `_meta.steering.supported`, and a `_session/steering` request is answered `injected` while a prompt runs and `promptRequired` when none does. Its documents do not describe it.
- **Other harnesses steer in other ways, or not at all.** Codex has `turn/steer`, Claude Code takes a message mid-turn through its streaming input, and some harnesses that speak the Agent Client Protocol have nothing.
- **An agent workbench that drives these harnesses queues what a harness cannot take.** A message a provider declines, or sent to a provider that cannot steer, waits and goes as its own turn when the turn ends.

## The decision

- **A message sent while a turn works is steered into it where the harness can, and otherwise waits.** It waits when the harness declines it, when the harness cannot steer, when the harness ends before it says, when a subagent of the thread asks, and when the turn's prompt has yet to go. A message for a thread with messages waiting waits behind them.
- **What waits goes one message at a time, each as its own turn**, in the order sent, when the turn before it ends done or failed. It goes with the desktop context it was sent with, in the thread's mode, Model, and effort as they are when it goes.
- **A Stop drops what waits behind the turn it stops.** Nothing runs after the person said stop. What a Stop dropped stays in the record as not sent.
- **What waits is in Lever's record and outlasts a restart.** Behind a turn that was interrupted, it waits for the person, and is not sent on its own: their next message goes behind it, and all go.
- **The person can take back a message that waits**, from the drawer and from `crowbar-agent`.
- **Waiting is Lever's, not the harness's.** A message that waits is sent as an ordinary message, a new turn, through the same adapter contract. It is not presented as steering, and Lever still steers only a harness that says it can.

## Considered Options

- **Refuse what the harness does not take**, as ADR 0036 and #371 have it. Rejected: the person does Lever's waiting for it, and a declined message can be lost.
- **Queue only what a harness that can steer declined**, and refuse a message to a harness that cannot steer. Rejected: clients would behave differently for each harness, which Lever's protocol exists to hide.
- **Join everything that waits into one turn.** Fewer turns. Rejected: each message is the person's own request, and the record keeps them apart.
- **Keep what waits after a Stop.** Rejected: a Stop means nothing more runs until the person says.
- **Keep what waits in memory only.** Simpler. Rejected: a restart of Lever would lose a message the person was told would go.

## Consequences

- ADR 0036's "never stands in for something a harness lacks" no longer covers waiting: Lever holds a message until the turn ends, for every harness. It still covers what a harness does within a turn.
- Lever's protocol gains a `queued` reply and event and an `unqueue` request. A message is refused while a turn works only when it would change the thread's mode, Model, or effort, or when the person has asked to stop the turn.
- A message the harness took into a turn is shown where the harness said it took it, which can be a little before the step at which the Model first sees it.
