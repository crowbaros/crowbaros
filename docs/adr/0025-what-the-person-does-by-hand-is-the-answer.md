# What the person does by hand is the answer

**Status:** accepted on November 16, 2026, by Jim Martin, in the design session after [council round 5](../reviews/council-round-5.md).

The powerbox already works this way: the person's pick of a file is the grant of a handle to it. Everywhere else, the person's own act was treated like an agent's. A paste into a workbench could be refused ([ADR 0019](0019-the-origin-table-fails-closed.md)). A paste from a menu got nothing, and a drag between two apps was left out ([ADR 0020](0020-the-first-compositor-is-small-on-purpose.md)). A question the person typed started a task that asked before it ran ([ADR 0011](0011-task-principals.md)). Council round 5 counted these as refusals and asks that a person meets for doing ordinary things. This record makes the powerbox's rule the general one.

[ADR 0022](0022-the-compositor-is-built-beside-milestone-2.md) says a change to what the compositor tells the broker about a paste gets a record of its own. This is that record.

## The rule

- **What the person does with their own hand is the answer.** A paste, a drop, a pick in the powerbox, and a link they click in an app are never refused, and are never followed by an ask about that same act.
- **A request the person types starts with no ask.** It starts a lookup, or points a conversation at a source, as it stands. What the Agent then does across the boundary to carry the request out is the Agent's act: it asks once, or is refused, by the rules for acts ([ADR 0011](0011-task-principals.md)).
- **Deterministic parts know the hand.** The compositor sees the key press, the click, and the drag. The shell sees the pick and the words typed into it. No model decides that the person did something. An act an agent makes, or one a client claims with no input behind it, is not the person's hand.
- **One ask can come before a typed request's work starts.** The first time a conversation is pointed at a category of private data that its model provider is not cleared for, one ask opens in place and takes a signature. It is asked once for each category ([ADR 0008](0008-private-values-stay-out-of-the-agent-context.md)).

## A value moved by hand is released, and keeps its mark

A label says where a value may go, and that is the person's to decide. A mark says a stranger wrote the value, and moving it does not make it the person's.

- **The move is a release.** The value passes to the compartment the person moved it into, whatever its label. That compartment's label does not rise, and nothing is refused.
- **The mark is kept.** The destination carries the value's mark from then on, and so does everything it writes afterwards.
- **A compartment whose label was fixed at start takes the mark too.** ADR 0019 refused a value above such a compartment's start label and mark, and rejected letting it rise, because the gate cannot see a native process write inside its view. A value moved by hand has its label released, so only the mark has to rise. The broker records the mark on the compartment before it delivers the value.
- **The rule does not depend on seeing a write.** Installed software and workbenches write live ([ADR 0017](0017-a-pinned-catalog-and-workbenches.md)), and a file may already be open. So when the compartment takes the mark, the broker takes a snapshot of every folder that compartment can write. From then on, an object in those folders that differs from the snapshot carries the mark whenever another compartment is given it. What counts is that the object changed after the value arrived, not how it was written. The compartment cannot alter the snapshot. An object that did not change keeps what it had.
- **The ledger records each one:** the source, the destination, and which act of the person's moved it.

Four cases:

| The person | What happens |
| --- | --- |
| Copies a one-time code from mail and pastes it into a form in their browser | It goes to that site. Nothing rises, and nothing is refused later. |
| Pastes text from a web page into their notes | It lands. What the notes app saves afterwards carries the mark, so an Agent task that later reads the note is marked, and its sends ask. |
| Pastes a stack trace from a web page into a workbench terminal | A workbench already carries the mark of the open web, so nothing changes, and its standing rules still fire. |
| Imports photos from a camera card | They are personal and marked as from outside, not private in the `unknown` category. |

## What counts as the person's hand

- **A paste.** Four acts count, and [ADR 0020](0020-the-first-compositor-is-small-on-purpose.md) fixes each:
  1. a paste key in the destination's window;
  2. a middle click in the destination's window, for the primary selection;
  3. a click in the window that holds the keyboard, followed at once by that window reading the clipboard, which is what Paste in a right-click menu does;
  4. a drop: a drag that starts in one compartment and is released over a window of another.
- **A read with none of these behind it gets nothing:** when a window gains the keyboard, on a timer, or from a window that does not hold the keyboard.
- **A pick** in the powerbox, as before.
- **A link the person clicks in an app's window** opens in their browser. The address is a value moved by hand. The same holds for a link in a clean conversation and in a lookup's answer. The shell shows a lookup's answer from the lookup's compartment, so a link in it opens whichever conversation started the lookup.
- **A link shown by a Tool, or by a conversation that is no longer clean, is that compartment's act.** Whatever may be steering it composed the address. A click on it asks, with the full address shown, unless a standing rule covers it ([ADR 0008](0008-private-values-stay-out-of-the-agent-context.md)).
- **A request the person types.** A lookup started from the turn they just typed leaves with those words, exactly as typed, and with no ask, whether or not the conversation is clean ([ADR 0011](0011-task-principals.md)). A conversation they point at a source may read it (ADR 0008).

## What this gives up

- **A person tricked into pasting or typing something harmful.** The threat model already lists a person persuaded to release something as not defended.
- **An app the person clicks in can read what they last copied.** A window that does not hold the keyboard still cannot, and a window that holds it cannot without a click or a key. Other Linux desktops let a window read the clipboard as soon as it has the keyboard.
- **A private value the person pastes into an app is at that app's label from then on.** It can go wherever that app can send it.
- **A turn the person types can leave as a lookup, though they meant it for the conversation.** [ADR 0011](0011-task-principals.md) names what that costs, in a clean conversation and in one that is not.
- **An app can put what it holds into the address of a link, and it leaves when the person clicks.** The person sees the link's words, not its address. A clean conversation can do the same with what it holds, which is at most personal.

## Considered Options

- **Refuse a paste above the destination's label and mark (ADR 0019's rule).** Rejected: the person did it by hand and met a refusal. A stack trace copied from a web page could not be pasted into a workbench terminal.
- **Ask on each such paste.** Rejected: a paste is too common an act, which is why ADR 0020 rejected a powerbox for it. The ask would be answered yes nearly every time.
- **Raise the destination to the value's label.** Rejected: a one-time code from mail would make the browser private to messages, and its next request to any other site would be refused.
- **Drop the mark as well.** Rejected: a stranger's text would reach a task as if the person had written it, which is the attack Milestone 2 is built to refuse. Keeping the mark costs an ask later, never a refusal now.
- **Keep the hint for a paste from a menu.** Rejected: it is how most people who are not developers paste, and a hint every time fails them.
- **An app declares at install that it reads the clipboard on a click.** Rejected for now: it adds a line to every approval screen that few people could judge.

## Consequences

- [ADR 0019](0019-the-origin-table-fails-closed.md) and [ADR 0020](0020-the-first-compositor-is-small-on-purpose.md) are revised. A paste is never refused, and the compositor's fourth slice ([issue 166](https://github.com/crowbaros/crowbaros/issues/166)) gains the primary selection and drag and drop between compartments.
- Those two are built before the Alpha bar is measured ([ADR 0024](0024-the-friction-bar-ranks-with-the-attack-list.md)), because the bar allows no refusal of what the person did by hand.
- The compositor tells the broker which of the four acts each transfer followed, as well as its source and destination. The attacks of the fourth slice change with it, and are run again.
- `crates/crowbar-labels` refused a value above a fixed compartment. It changed through [issue 180](https://github.com/crowbaros/crowbaros/issues/180): a compartment takes a value moved in by hand through an operation of its own, and every other value is read as before.
- Milestone 2 gains attacks: a clipboard read with no hand behind it, a marked paste into a file an app already had open, and a link shown by a conversation that is no longer clean.
- A mark now spreads by hand as well as by tasks. Milestone 2's measurement of how far marks spread counts it.
