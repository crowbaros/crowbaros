# A "yes" comes from the person: trusted input, a confirm gesture, and a hardware root

Every crossing the gate does not already allow ends in the person's answer. That answer is only worth something if nothing else can produce it.

There are three answers, and what is at stake decides which one an ask takes ([ADR 0026](0026-asks-wait-and-take-one-of-three-answers.md)): a plain answer for what stays inside the undo boundary, the confirm gesture for what crosses and for authority that lasts only for this run, and a signature for authority that lasts for good.

1. **Only the shell delivers input.** No principal can synthesize keyboard, pointer, or touch input to a surface the shell draws, and nothing but the shell can read input meant for another surface. Agents operate Tools through the UI description (ADR 0004), never through input events. On Linux, Base has to close every route by which one process produces or reads another's input. The known ones: an X11 server shared between apps (each app gets its own XWayland, or none); the accessibility bus; virtual keyboard, virtual pointer, and input-method protocols; the remote desktop portal and its input emulation; creating or reading input devices; tracing the shell; pushing characters into a terminal; and owner authority in any terminal an agent can reach. A route that is not closed is a hole in this decision.
2. **An answer to anything that crosses uses the confirm gesture.** An ask is drawn by the shell in system chrome. The person answers with a secure key press that the shell handles itself and never delivers to any Tool or app. An ask ignores the gesture for a moment after it appears, so one ask cannot be swapped under a key press. An ask waits until the person opens it, and never takes the keyboard from their work. Asks are counted per task and across all the Agent's tasks together, and the ask that starts a task counts. Past the limits, further asks are held in one line the person can dismiss whole, so neither one task nor a stream of new tasks can flood the person until they stop reading (ADR 0026). What stays inside the undo boundary, such as keeping a preview, takes a plain answer instead: any ordinary input in trusted UI.
3. **Authority that lasts needs a hardware root.** A new standing grant, rule, or standing task, wider limits on any of them, a higher ceiling for tasks that do not cross, a higher clearance, a declassify, a refilled Allowance, and owner authority each need a passkey, a security key, or approval on the phone. The approval is a signature over the exact content shown, so approving one thing cannot be replayed as another. Authority that lasts only for this run, such as a host added to a workbench until it stops, takes the confirm gesture. Standing authority is asked for the first time something needs it, not up front, and a list the person put together takes one signature (ADR 0026).
4. **Voice never counts as confirmation.** A voice can ask for something. It cannot say yes to an ask, because a speaker can produce it. Words that arrive by voice are marked until the person makes the confirm gesture (ADR 0008), so nothing crosses for them and nothing they say is kept as the person's own.
5. **The trusted UI and the confirm gesture are part of Base.** They cannot be themed, rebound, or replaced by anything in the Environment.

## Considered Options

- **Any input in system UI counts as the person.** Rejected for anything that crosses or that lasts: an agent or a native app that can produce input could answer its own ask. ADR 0026 accepts it for what stays inside the undo boundary, where rule 1 is the defence and an undo is the remedy.
- **A hardware root for every answer.** Rejected: touching a key for every ask teaches the person to touch it without reading.
- **Let the person theme the ask.** Rejected: an ask that can look like anything can be imitated.

## Consequences

- An agent cannot drive a native app that has no UI description. Whether to offer that through an accessibility interface the shell controls is open.
- A machine with no authenticator of its own changes authority through the phone or a security key.
- Revised after council round 5. Until then there were two answers, every ask took at least the confirm gesture, an ask opened the moment it was raised, and a small change to standing authority for one afternoon took a hardware root.
- All of this rests on Base. A compromised kernel or shell can forge an everyday answer. Phone approval does not rest on the desk's kernel, which is why the person can require it for the changes that matter most.
- The shell needs a way to show exact content that is too long for one screen, without training the person to skip it. [ADR 0018](0018-one-gesture-answers-one-ask.md) answers this: an everyday ask for one act fits one screen, and a longer one (a list, or an ask for authority that lasts) is paged and armed only after its last page.
- Where this record says the shell handles the confirm gesture, [ADR 0018](0018-one-gesture-answers-one-ask.md) divides the work: the compositor takes the gesture and sends the yes to the broker, and the shell draws the ask.
