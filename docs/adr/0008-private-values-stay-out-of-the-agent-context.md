# Private values reach the Agent only where the person points it; labels and marks survive every handoff

We assume most people's Agent thinks with a cloud model. A model's context cannot keep private and public apart: once it has read one private value, everything it writes may carry it. The same holds for content written by strangers: once a context has read it, everything it writes may carry the stranger's instructions. So two things are tracked on whole execution contexts and on every value they emit:

- a **label**, which says how private the value is. Labels are ordered: public, personal, private. Private data also carries a category (messages, health, finance, and so on).
- a **mark**, which says the value was produced by a context that had read open **untrusted input** (ADR 0011).

A **compartment** is one execution context: a conversation, a task's model context and its workspace, or a Tool's sandbox. Once it reads a value, it is at least at that value's label and carries that value's mark, and so is everything it emits: files, messages, logs, and model calls. The one exception is a value the person moved in by hand. It is released, so the compartment takes its mark and not its label ([ADR 0025](0025-what-the-person-does-by-hand-is-the-answer.md)).

**Nothing a model does removes a label or a mark, and nothing crosses freely.** Summarizing does not clean a value, starting a new task does not, and passing through the Agent does not. Marks persist in storage too: a Tool's source and an entry in Agent memory keep the mark of whatever wrote them.

**Each conversation is a compartment of its own.** A **conversation** is one exchange between the person and the Agent.

- **It starts clean.** A conversation is **clean** while it is personal and unmarked. It starts with the entries of Agent memory that are at most personal and unmarked.
- **It changes only by the person's act.** There are two:
  - *The person puts a value in:* a paste, a drop, or a pick of one object. That is one exact value moved by hand, so it is released (ADR 0025). The conversation stays at its label and takes the value's mark. A value with no mark, such as the person's own note, leaves the conversation clean.
  - *The person points it at a source:* something that holds more than one object, such as their mail, a folder, or a Tool's Data. They pick it in the powerbox, point at it, or name it in the words they type. The shell turns each of these into a reference to the source, by fixed rules and never by a model, and the broker opens only what a reference names. The conversation rises to the source's label and takes its mark.
- **Only typed words name a source.** Words that were pasted, spoken, or written by the Agent make no reference. A reference permits a read and does not force one. Words that name no source the shell knows change nothing, and the Agent asks the person to pick one.
- **Pointing at part of a Tool to change it is a reference to the Tool's description**, not to the Data the Tool shows. It does not change the conversation.
- **The Agent cannot mark or raise its own conversation.** A read it asks for with no reference from the person behind it is refused. It uses a lookup (ADR 0011) or a plan instead.
- **A conversation that is no longer clean stays that way for as long as it lives**, with its history and any summary of it. Its frame shows its label and mark. It can read and discuss what it was given.
- **Every act of its that crosses asks**, with the exact content shown. A link it shows is one of those acts: a click on the link asks, with the full address shown. So is anything it composes for a lookup. A standing task started from it runs marked.
- **One thing goes from it to the open web with no ask: the turn the person typed last, as a lookup.** The turn goes whole and unchanged, once, and the lookup's compartment holds nothing else that the conversation holds (ADR 0011).
- **What it writes keeps its label and mark.** That holds for an entry in Agent memory, which a new conversation therefore does not load, and for an intent or a check it writes. A build from such an intent or check is not clean, so the grant asks again, with a signature (ADR 0011).
- **A clean conversation is one action away.**
- **Its provider must be cleared.** A conversation pointed at a category of private data thinks with a model provider cleared for that category. The first time, one ask opens in place and takes a signature: "let your provider hold messages". It is not asked again for that category. A typed request meets no other ask before its work starts.

**In a clean conversation, and in a standing task, the Agent works by plan over handles.** The Agent writes a **plan**: a small program whose variables are handles. A **handle** names a value without revealing it. A deterministic interpreter in the fleet runs the plan and moves the actual values between Tools, broker operations, and model calls. The Agent can draft "Hi {name}, your appointment is {date}" without seeing either value. Because the interpreter is deterministic, it tracks the label and mark of every result, which a model's context cannot. That includes handles, and **bounded results**: a choice from a list, or a number in a range, both fixed before the task read anything.

- A plan may branch on a bounded result. Everything that follows the branch carries that result's label and mark. An act that depends on a result from a marked task counts as marked (ADR 0011), so a steered choice cannot spend a standing rule.
- A clean conversation does not read a result from a marked task. The plan branches on it inside the interpreter, and the shell shows it to the person. To have the Agent read it, the person puts it into the conversation or points the conversation at it. A new request clears nothing; only a new conversation starts clean.
- What can be observed about a result is a result. The size of a value, the number of handles a task returns, and whether a step succeeded or was refused carry the label and mark of what they were computed from. They are hidden from the Agent, or charged to the budget, like any other result.
- A private result reaches a clean conversation only by a **release**, or inside a budget for that category. The budget is counted in bits across all tasks. It is zero, and no interface sets it yet. The shell can always show the person a result directly, without it passing through the Agent.
- A **release** is the person letting one exact value pass to one named destination despite its label: by answering an ask that shows the value in trusted UI, or by moving it there with their own hand (ADR 0025). The value keeps its mark.

**Only the person's own words are the person's words.** What the person types reaches the Agent as theirs. What arrives by voice is marked until the person makes the confirm gesture, because a speaker can produce it (ADR 0014). What the person pastes or drops into a conversation enters it, and the conversation carries its mark from then on. Every other string that was not typed by the person or produced by a fixed template is data with its origin's label and mark: a file name, a mail subject, a stranger's Tool label, error text from a server, a ledger detail. The Agent sees what the OS states about a handle (its kind, label, mark, and id, and its size only where that does not depend on a private or marked value) and names the person typed. Synthetic samples are made by deterministic code from the schema, never by a model from real Data.

A clean conversation holds handles, schemas, synthetic samples, results it is allowed to read, and the person's own words. It is personal: what the person types counts as personal by rule. A value put into a conversation goes to that conversation's model provider as well. When an ask releases the value, the ask says so.

**Every outside party has a clearance.** A **clearance** is the highest label a machine or an outside party may hold, and for private data it names the categories. The default for a cloud model provider is personal, and the person can raise it for one category at a time. The default for a web site is public. A site may also receive back what it supplied, and what the person released to it. Content fetched under a login is private to that site. A compartment may connect only to parties cleared for everything it holds. The broker builds every model request itself, to the provider the person chose, with the provider's own tools and retention switched off. A task may read a value and think with a model only when that model's provider or machine is cleared for the value.

Revised after council rounds 2, 3, 4, and 5. The first version returned "a narrow, typed result" from a private compartment, which could still contain the private value, and handed summaries to a "clean task", which carried any injection with them. The second let handles and bounded results "cross freely": a task could then return a private file one byte at a time, and a steered choice could be acted on by an unmarked Agent. Until round 5 the Agent had one context, which never read a marked result. A person who asked for help with a mail got an Agent that could not read it, and each "make it firmer" became another task it directed blind.

## Considered Options

- **Design for a local model and route private data to it (the earlier design).** Rejected as the default: for most people the model that is good enough to build software is in the cloud, so the Agent would be either blind to the person's data or declassifying constantly.
- **Track labels per value through the Agent's context.** Rejected: it cannot be enforced inside a model, and promising it would be false. The plan interpreter tracks per value because it is not a model.
- **Typed results cross compartments freely (the round 1 text).** Rejected: a typed string is still a string.
- **Bounded results cross freely (the round 2 text).** Rejected: many small results add up to the whole value, and the choice itself is what an injection steers.
- **No cloud model ever reads private data.** Rejected: a person with no model in the fleet would get no help with their own mail. The person decides, per category, with the provider named.
- **Treat the fleet as one flat perimeter.** Rejected: a rented machine and a stolen laptop would get the same trust as the desk.
- **The Agent never reads what a marked task found, in any conversation (the round 4 text).** Rejected: the Agent could not read the thing the person asked it to help with, and the work it directed blind cost an ask at every step.
- **One Agent context for every exchange, which may rise.** Rejected: one mail would mark everything the person did with the Agent from then on.
- **The Agent may read what it chooses and take the mark.** Rejected: a conversation would change state behind the person's back, and one hostile page could turn any conversation into a marked one.
- **A model decides which source the person's words name.** Rejected: it puts a model on the trust path. Fixed rules in the shell will miss some requests, and the cost of a miss is one pick.

## Consequences

- The plan language and its interpreter are part of the trusted base. Milestone 2 prototypes them. Prior art: the dual-model pattern and capability-tracking interpreters for agent plans.
- The Agent cannot read the name of a file a stranger named. It works on the handle, and the shell shows the name to the person.
- Tracking what an act depends on is the interpreter's job. A model's context cannot do it, so a conversation is all or nothing: it is clean, or it is marked for as long as it lives.
- In a clean conversation, the Agent cannot change its plan after seeing what a marked task found. In a conversation the person pointed at the source, it can. Milestone 2 measures both.
- The Agent in a conversation that is no longer clean may be steered by what it read. What bounds it: it holds no standing grant or rule, every act of its that crosses asks with the exact content, it reaches only parties cleared for what it holds, and what it writes keeps its mark. Its model requests go to its cleared provider with no ask (ADR 0011). The one thing it can send to the open web with no ask is the turn the person just typed, as a lookup, exactly as typed.
- The person has to understand what a conversation's frame says about its label and mark. How the frame says it is design work.
- The fixed rules can err both ways. A miss costs the person a pick. A match the person did not mean lets the Agent open that source, and in a conversation that is no longer clean a steered Agent can take it: the conversation then holds that source too, and its provider must be cleared for it. Milestone 2 counts both kinds of error.
- A change to a Tool that the person asks for in a conversation that is no longer clean costs a signature, because the build is not clean.
- Switching off a provider's tools and retention is a setting and a contract, not something the OS can verify. A provider's clearance is trust in that provider.
- A machine whose disk is not encrypted has no clearance for private data.
- With no model cleared for a category, values in it are processed only by Tools.
- For software the broker cannot see inside, label checks are per principal and per destination (ADR 0004).
