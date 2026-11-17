# The broker owns each ask, the shell draws it, and the compositor carries the yes

**Status:** accepted on November 15, 2026, by Jim Martin, at the end of the design session for [issue 55](https://github.com/crowbaros/crowbaros/issues/55).

[ADR 0014](0014-a-yes-comes-from-the-person.md) says a yes is worth something only if nothing else can produce it. The [compositor spike](../research/compositor-spike-2026-11-15.md) took the confirm chord before any client saw it, but its channel to the shell had no request IDs, no version, and no way to reconnect, and its confirm event named no ask. [ADR 0016](0016-the-warden-starts-every-compartment.md) then made the shell a service of its own that meets the compositor and the broker over path sockets. This record says how those parts talk, and how one press of the confirm gesture answers exactly one ask.

Revised after council round 5 by [ADR 0026](0026-asks-wait-and-take-one-of-three-answers.md). An ask now waits until the person opens it, lasts as long as its task, may hold a list, and may take a plain answer. Arming, the chord, the hash, and who carries the yes are as they were.

## Who does what

- **The broker owns the ask.** The gate decides that an act needs the person's answer. The broker makes the **ask**: its ID, its exact content, its kind, which answer it takes, and its expiry. It holds the counts (ADR 0014, rule 2) and the list of asks that wait.
- **The shell draws the ask.** It draws the content inside a frame that only the compositor draws. The shell is trusted for what it shows, as ADR 0014 already accepts. It carries no yes but a plain answer (ADR 0026).
- **The compositor carries the yes.** It checks that the ask is visible, on top, and steady, takes the gesture, and sends the answer straight to the broker. A bug in the shell cannot make up a yes. The one exception is a plain answer, which the shell carries, and which is accepted only for what stays inside the undo boundary (ADR 0026).

## The channels

- **Path sockets under ADR 0016's rules.** The shell, the compositor, and the broker meet over path sockets in a directory only the two ends can enter, and each listener checks the peer's user ID. The spike's socket pair is gone.
- **Length-prefixed JSON.** Each message is a length, then one JSON object. Both programs already use serde, and a log can be read.
- **A version at hello.** Each connection starts with a hello that names the protocol version. A mismatch refuses the connection and is logged.
- **Requests and events.** A request carries an ID and gets exactly one reply with that ID. An event can arrive at any time. The set of requests is fixed: there is no way to run a command or load code.
- **A bounded queue for each peer.** No trusted part blocks on another. A peer that lets its outgoing queue fill up is disconnected, and reconnects.

## One ask, from open to answer

0. **Waiting.** The broker makes the ask and tells the shell, and the bar shows how many are waiting. Nothing is shown over the person's work, and the keyboard stays where it is. The ask is opened when the person opens it, or in place when it is for what the person just asked for (ADR 0026).
1. **Open.** The broker sends the compositor `ask_open {ask_id, content_hash, kind, expires}`, and sends the shell the same ID with the content.
2. **Shown.** The shell tells the compositor which of its surfaces shows `ask_id`.
3. **Armed.** The compositor **arms** the ask when that surface is mapped, fully visible, on top, holds the keyboard, and has drawn no new frame for one second. Any new frame starts the second again, so the content cannot change under the person's finger. Ask surfaces do not animate. The compositor tells the broker each time the ask is armed or disarmed.
4. **Answered.** The gesture on an armed ask sends `confirmed {ask_id, content_hash}` to the broker. The broker checks the hash against the ask it made and uses each ask ID once. The gesture on an ask that is not armed does nothing.

- **One ask at a time.** The compositor shows and arms at most one ask, on the output that has focus. The rest wait, and the shell shows how many.
- **A list.** An ask may hold several acts of one kind. The person can strike any of them. The shell carries a strike to the broker, which makes the content hash again over what is left, and the compositor arms the ask again (ADR 0026).
- **Declining.** Escape on the ask, or a click on Decline, declines it, whether the shell or the compositor sees it. The broker records the decline. A shell that declines wrongly costs a new ask, never a false yes.
- **Expiry.** An ask lasts as long as the task or the running compartment that raised it, and `expires` is when that ends. The broker withdraws an ask past it, and the compositor disarms it.
- **The lock screen.** While the session is locked, no ask is shown or armed and the gesture does nothing. The ask waits in the broker's queue, and its second starts again after unlock. [ADR 0020](0020-the-first-compositor-is-small-on-purpose.md) gives the chord one job on the lock screen, which is to open the unlock entry. It still answers no ask there.

## Visible and on top

While an ask that takes the confirm gesture or a signature is showing, the compositor dims every other surface, draws the ask's frame, and sends keyboard and pointer input to the ask surface alone. The cursor and drag icons draw under the frame. No client can dim another client, so "on top" is something the compositor enforces, not a stacking order a client can reach. What the frame looks like is decided with the rest of the trusted UI in [issue 66](https://github.com/crowbaros/crowbaros/issues/66). [ADR 0020](0020-the-first-compositor-is-small-on-purpose.md) fixes what a frame is and where it is drawn, and leaves its look to the design work under issue 22.

## The gesture

- **The confirm gesture is Ctrl+Alt+Enter, and the keyboard alone can say yes.** The pointer can decline or dismiss an ask, never confirm one. A click can be arranged under a moving cursor; a chord is felt. Which keys make the chord can change later. That nothing in the Environment can rebind it cannot (ADR 0014, rule 5).
- **The compositor always takes the chord.** It reaches no client, whether or not an ask is showing. With no armed ask it does nothing. [ADR 0020](0020-the-first-compositor-is-small-on-purpose.md) changes one case: with no ask showing and the session unlocked, the chord locks the session.
- **One keyboard makes the chord.** The modifiers and Enter must be pressed on the same keyboard. The compositor tracks withheld keys for each keyboard, so a release with no press never reaches a client.
- **A keyboard attached after an ask opened cannot confirm it.** A USB device can pretend to be a keyboard and type the chord.

## When the shell goes away

When the shell disconnects or stalls, the compositor disarms every ask at once, and the gesture does nothing. runit restarts the shell (ADR 0016). Until it is back, the compositor draws one plain notice of its own, "System UI restarting", and no app gets the keyboard. When the shell reconnects, the broker sends every open ask again with the same ID, and its second starts again.

## Authority that lasts

An ask for authority that lasts (ADR 0014, rule 3) uses the same path with a different answer:

- **The gesture does nothing on it.** Only a signature answers it.
- **The broker asks for the touch only while the ask is armed.** It waits for the compositor's armed event.
- **What is signed is a hash of `ask_id` and `content_hash`.** A signature for one ask is refused for any other.
- **Only the broker's system user can reach the security key's device.** No compartment can.
- **The phone signs the same hash.** How it does so is the fleet's work (Milestone 6).

## Content longer than one screen

This answers the open consequence of ADR 0014.

- **An everyday ask for one act fits on one screen.** The broker writes it from fixed templates.
- **A list, and an ask for authority that lasts, can be longer.** The shell pages through it, and the compositor arms it only after the last page has been shown. Each page is a new frame, so the second starts again on every page.

## Tests

Each of these must pass, or be refused, before the shell protocol is accepted. [Issue 71](https://github.com/crowbaros/crowbaros/issues/71) runs them with the spike's in-process harness and probe client.

1. The chord on an ask that is not armed: inside the second, after a new frame, while covered, while it does not hold the keyboard. Nothing is confirmed.
2. A `confirmed` whose hash does not match, or whose ask ID was already used. The broker refuses it.
3. The chord split across two keyboards. Nothing is confirmed, and no client sees the keys.
4. A keyboard attached after the ask opened presses the chord. Nothing is confirmed.
5. The shell is killed during an ask. The ask disarms, the notice shows, no app gets the keyboard, and the ask returns with its second started again.
6. The shell stops reading. The compositor keeps running, disconnects it, and it reconnects.
7. A hello with the wrong version. The connection is refused and logged.
8. A process with the wrong user ID connects to the compositor's or the broker's socket. It is refused (ADR 0016, attack 15).
9. A client window tries to cover the ask, imitate it, or take its focus. It is dimmed and gets no input.
10. An ask that changes authority: no touch is asked for before it is armed, and its signature is refused for another ask.
11. An ask while the session is locked. It is not shown and not armed.
12. The chord with no ask showing. Nothing is confirmed, and no client sees it. The session locks (ADR 0020).
13. A malformed, oversized, or unknown message on each socket. The connection is refused, and the trusted part does not crash.

Added by ADR 0026:

14. An ask is raised while the person types in another window. Nothing is shown, the keyboard stays where it is, and the count in the bar rises.
15. An ask claims to be for what the person just asked for, while another window holds the keyboard, or after the person typed again in that conversation, or from an app's window. It waits.
16. One principal raises asks past its limit, and the harness proposes tasks past the Agent's. The principal's further asks are held in one line, and the further asks of the Agent and all its tasks are held together in one more. Nothing is refused, and dismissing a line declines every ask in it.
17. A list ask: the person strikes one item. The ask disarms until it is steady again, and a `confirmed` with the earlier hash is refused.
18. The shell reports a strike the person did not make. The struck act does not happen. The shell reports that a struck item is back. The broker refuses it.
19. The chord on an ask that takes a plain answer. Nothing is confirmed.
20. A plain answer arrives for an ask that takes the confirm gesture or a signature. The broker refuses it.
21. A key or a click reaches an ask that takes a plain answer in the moment after it appears. Nothing is answered.

## Considered Options

- **The shell owns the ask and relays the yes.** Rejected: the largest trusted program would carry every yes.
- **The compositor owns the ask and its rate limits.** Rejected: the broker already holds them for each task, and two copies can disagree.
- **The compositor draws the ask's text.** Rejected for now: it would put text layout and paging into the program that holds the seat. It is the stronger design, and the one to return to if the shell's trust proves too much.
- **The shell reports a hash of what it drew.** Rejected: nothing proves the pixels match the hash, so it adds a check that checks nothing.
- **A private Wayland protocol between shell and compositor.** Rejected: GPUI 0.2.2 gives the shell no way to bind a global of its own.
- **A schema format such as Cap'n Proto or protobuf.** Rejected for now: tighter, but it brings a toolchain, and its messages cannot be read in a log.
- **A button the pointer can click to say yes.** Rejected: it is the usual target of clickjacking.
- **Several asks on screen at once.** Rejected: one gesture would have to choose between them.
- **Apps keep the keyboard while the shell restarts.** Rejected: an app could put a window where the ask was.
- **A scrolling everyday ask.** Rejected: it teaches the person to scroll past without reading. A list is paged, and armed only after its last page.
- **A separate authenticator service for the security key.** Rejected: it keeps USB HID out of the broker, at the cost of a fifth trusted part.

## Consequences

- The spike's channel is a reference only. Issue 71 builds the protocol on the production compositor.
- The broker gains a socket to the compositor and one to the shell, and the compositor tells it when an ask is armed or disarmed.
- The shell's design for asks has no animation and fits one screen for an everyday ask about one act.
- ADR 0014's open consequence on long content is answered here.
- A compromised shell can still show one thing while the broker asked another. Asks that change authority are where that matters most, and the phone's display does not rest on the desk's shell.
