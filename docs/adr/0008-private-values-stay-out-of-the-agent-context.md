# Private values stay out of the Agent's context; labels and marks survive every handoff

We assume most people's Agent thinks with a cloud model. A model's context cannot keep private and public apart: once it has read one private value, everything it writes may carry it. The same holds for content written by strangers: once a context has read it, everything it writes may carry the stranger's instructions. So two things are tracked on whole execution contexts and on every value they emit:

- a **label**, which says how private the value is. Labels are ordered: public, personal, private. Private data also carries a category (messages, health, finance, and so on).
- a **mark**, which says the value was produced by a context that had read open **untrusted input** (ADR 0011).

A **compartment** is one execution context: a task's model context and its workspace, or a Tool's sandbox. Once it reads a value, it is at least at that value's label and carries that value's mark, and so is everything it emits: files, messages, logs, and model calls.

**Nothing a model does removes a label or a mark, and nothing crosses freely.** Summarizing does not clean a value, starting a new task does not, and passing through the Agent does not. Marks persist in storage too: a Tool's source and an entry in Agent memory keep the mark of whatever wrote them.

**The Agent works by plan over handles.** The Agent writes a **plan**: a small program whose variables are handles. A **handle** names a value without revealing it. A deterministic interpreter in the fleet runs the plan and moves the actual values between Tools, broker operations, and model calls. The Agent can draft "Hi {name}, your appointment is {date}" without seeing either value. Because the interpreter is deterministic, it tracks the label and mark of every result, which a model's context cannot. That includes handles, and **bounded results**: a choice from a list, or a number in a range, both fixed before the task read anything.

- A plan may branch on a bounded result. Everything that follows the branch carries that result's label and mark. An act that depends on a result from a marked task counts as marked (ADR 0011), so a steered choice cannot spend a standing rule.
- The Agent does not read a result from a marked task. The plan branches on it inside the interpreter, and the shell shows it to the person. If a marked result does enter the Agent's context, that context is marked for as long as it lives, together with its history and any summary of it. A new request clears nothing; only a fresh context with no marked input starts clean. A standing task started from a marked context runs marked.
- What can be observed about a result is a result. The size of a value, the number of handles a task returns, and whether a step succeeded or was refused carry the label and mark of what they were computed from. They are hidden from the Agent, or charged to the budget, like any other result.
- A private result reaches the Agent's context only by a **release**, or inside a budget the person set for that category. The budget is counted in bits across all tasks, and its default is zero. The shell can always show the person a result directly, without it passing through the Agent.
- A **release** is the person seeing the exact value in trusted UI and letting it pass to one named destination.

**Only the person's own words are the person's words.** What the person types reaches the Agent as theirs. What arrives by voice is marked until the person makes the confirm gesture, because a speaker can produce it (ADR 0014). Content the person pastes or drops arrives as a handle with its origin's label and mark, not as text in the Agent's context. Every other string that was not typed by the person or produced by a fixed template is data with its origin's label and mark: a file name, a mail subject, a stranger's Tool label, error text from a server, a ledger detail. The Agent sees what the OS states about a handle (its kind, label, mark, and id, and its size only where that does not depend on a private or marked value) and names the person typed. Synthetic samples are made by deterministic code from the schema, never by a model from real Data.

The Agent's own context holds handles, schemas, synthetic samples, results it is allowed to read, and the person's own words. It is personal by default: what the person types counts as personal by rule. A private value released into it is released to the Agent's model provider as well, and the ask says so.

**Every outside party has a clearance.** A **clearance** is the highest label a machine or an outside party may hold, and for private data it names the categories. The default for a cloud model provider is personal, and the person can raise it for one category at a time. The default for a web site is public. A site may also receive back what it supplied, and what the person released to it. Content fetched under a login is private to that site. A compartment may connect only to parties cleared for everything it holds. The broker builds every model request itself, to the provider the person chose, with the provider's own tools and retention switched off. A task may read a value and think with a model only when that model's provider or machine is cleared for the value.

Revised after council rounds 2, 3, and 4. The first version returned "a narrow, typed result" from a private compartment, which could still contain the private value, and handed summaries to a "clean task", which carried any injection with them. The second let handles and bounded results "cross freely": a task could then return a private file one byte at a time, and a steered choice could be acted on by an unmarked Agent.

## Considered Options

- **Design for a local model and route private data to it (the earlier design).** Rejected as the default: for most people the model that is good enough to build software is in the cloud, so the Agent would be either blind to the person's data or declassifying constantly.
- **Track labels per value through the Agent's context.** Rejected: it cannot be enforced inside a model, and promising it would be false. The plan interpreter tracks per value because it is not a model.
- **Typed results cross compartments freely (the round 1 text).** Rejected: a typed string is still a string.
- **Bounded results cross freely (the round 2 text).** Rejected: many small results add up to the whole value, and the choice itself is what an injection steers.
- **No cloud model ever reads private data.** Rejected: a person with no model in the fleet would get no help with their own mail. The person decides, per category, with the provider named.
- **Treat the fleet as one flat perimeter.** Rejected: a rented machine and a stolen laptop would get the same trust as the desk.

## Consequences

- The plan language and its interpreter are part of the trusted base. Milestone 2 prototypes them. Prior art: the dual-model pattern and capability-tracking interpreters for agent plans.
- The Agent cannot read the name of a file a stranger named. It works on the handle, and the shell shows the name to the person.
- Tracking what an act depends on is the interpreter's job. A model's context cannot do it, so the Agent's context is all or nothing: it is clean, or it is marked for as long as it lives.
- The Agent cannot change its plan after seeing what a marked task found. Whether ordinary work survives that is for Milestone 2 to measure.
- Switching off a provider's tools and retention is a setting and a contract, not something the OS can verify. A provider's clearance is trust in that provider.
- A machine whose disk is not encrypted has no clearance for private data.
- With no model cleared for a category, values in it are processed only by Tools.
- For software the broker cannot see inside, label checks are per principal and per destination (ADR 0004).
