# Intents are the source; the Crowbar State is a lockfile

The durable thing a person owns is what they asked for, not the packages and code that came out of it. So a curated set of **intents** (the person's words plus checks they confirmed) is the source of truth. The **Crowbar State** is compiled from it and pins the result exactly, and **generations** are builds of the State. The agent merges each new request into the intents as a diff the person can see. Compiling happens only when an intent changes or the person asks to re-derive. A better model never rewrites the OS silently: it can only offer a re-derive as a diff that must pass the existing checks.

## Considered Options

- **The State is the truth, prompts are ledger metadata.** Rejected: the OS can't be re-derived with a better model or on different hardware, and sharing means copying a machine instead of an idea.
- **Intents only, regenerated every time.** Rejected: not reproducible; the system would drift with every model change.

## Consequences

- An OS definition can be shared as intents (portable, re-derivable) or as intents plus the State (an exact copy).
- The approver has a durable intent to check actions against.
- Writing an intent is a design conversation: the agent asks questions and proposes checks before it builds.
