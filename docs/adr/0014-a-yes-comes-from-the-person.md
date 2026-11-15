# A "yes" comes from the person: trusted input, a confirm gesture, and a hardware root

Every crossing the gate does not already allow ends in the person's answer. That answer is only worth something if nothing else can produce it.

1. **Only the shell delivers input.** No principal can synthesize keyboard, pointer, or touch input to a surface the shell draws, and nothing but the shell can read input meant for another surface. Agents operate Tools through the UI description (ADR 0004), never through input events. On Linux, Base has to close every route by which one process produces or reads another's input. The known ones: an X11 server shared between apps (each app gets its own XWayland, or none); the accessibility bus; virtual keyboard, virtual pointer, and input-method protocols; the remote desktop portal and its input emulation; creating or reading input devices; tracing the shell; pushing characters into a terminal; and owner authority in any terminal an agent can reach. A route that is not closed is a hole in this decision.
2. **Everyday answers use a confirm gesture.** An ask is drawn by the shell in system chrome. The person answers with a secure key press that the shell handles itself and never delivers to any Tool or app. An ask ignores the gesture for a moment after it appears, so one ask cannot be swapped under a key press. Asks are rate-limited per task and across all the Agent's tasks together, and the ask that starts a task counts, so neither one task nor a stream of new tasks can flood the person until they stop reading.
3. **A change to authority needs a hardware root.** A new standing grant, rule, or standing task, wider limits on any of them, a higher ceiling for tasks that do not cross, a higher clearance, a declassify, a refilled Allowance, and owner authority each need a passkey, a security key, or approval on the phone. The approval is a signature over the exact content shown, so approving one thing cannot be replayed as another.
4. **Voice never counts as confirmation.** A voice can ask for something. It cannot say yes to an ask, because a speaker can produce it. Words that arrive by voice are marked until the person makes the confirm gesture (ADR 0008), so nothing crosses for them and nothing they say is kept as the person's own.
5. **The trusted UI and the confirm gesture are part of Base.** They cannot be themed, rebound, or replaced by anything in the Environment.

## Considered Options

- **Any input in system UI counts as the person.** Rejected: an agent or a native app that can produce input could answer its own ask.
- **A hardware root for every answer.** Rejected: touching a key for every ask teaches the person to touch it without reading.
- **Let the person theme the ask.** Rejected: an ask that can look like anything can be imitated.

## Consequences

- An agent cannot drive a native app that has no UI description. Whether to offer that through an accessibility interface the shell controls is open.
- A machine with no authenticator of its own changes authority through the phone or a security key.
- All of this rests on Base. A compromised kernel or shell can forge an everyday answer. Phone approval does not rest on the desk's kernel, which is why the person can require it for the changes that matter most.
- The shell needs a way to show exact content that is too long for one screen, without training the person to skip it.
