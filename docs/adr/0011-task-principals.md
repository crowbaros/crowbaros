# The Agent coordinates; each task is its own principal, and untrusted input costs it authority

Each piece of agent work is a **task**: a fresh principal with a stated objective, effect limits, a budget, and an expiry. The Agent keeps the person's history and starts tasks, but it does not hold the authority of the tasks it has run. When a task ends, its handles end with it.

**Untrusted input is graded.**

- **Admitted** input comes from a pinned set the person admitted in advance: the Base package snapshot, and the dependency set with its documentation that build tasks may use. Reading it does not mark a context. Admission is where that risk is taken, once, by a step that can be audited.
- **Open** input is everything else written by someone other than the person or their OS: web pages, mail, messages, shared intents, files from outside. Reading it **marks** the context, and the mark follows everything the context emits (ADR 0008).

**A marked task keeps thinking and loses authority.** It keeps its model, through a request the broker builds: to the provider the person chose, with the provider's own tools and retention off. It loses every standing grant and standing rule to cross the undo boundary: to act on the person's accounts, to send data out of the fleet, or to do anything inside the fleet that cannot be undone. From then on such an act needs the person's answer, with one exception: a standing rule the person wrote for that source, carried out by a broker adapter that knows the operation ("pay this bill on this site, up to this amount each month"). The limits in that rule are then the only protection, and the rule says so. An act that depends on a result from a marked task is marked too (ADR 0008).

**Egress goes only to parties cleared for the data.** A compartment may connect only to parties cleared for everything it holds (ADR 0008). A web site is cleared for public data by default, so a compartment that has read private data cannot fetch a stranger's page, and a research task that browses holds nothing above public. The objective of a research task leaves the fleet with its first request. So the ask that starts one is a release in full: it shows the exact text that may leave, and the sites or the search service it may go to. A standing task for research has a fixed objective, and any text the Agent adds to it is shown to the person before it leaves. No principal may hold private read, open untrusted input, and a connection to a party not cleared for that data. The gate refuses the third.

**Build tasks are clean by construction.** A task that writes or changes a Tool's code reads only the intent, the checks, the Tool's own source, and admitted input. It has no web access. It sees results on synthetic inputs only: a replay of real recorded inputs reaches it as pass or fail. That verdict is computed from private, marked inputs. It is the one such result a build may read and stay clean, and it is capped at a fixed number of replays per re-derive, so a build cannot search a real input one bit at a time. Fixing a bug that needs the real input goes through the person, who turns that input into a new check. A **research task** may browse; it is marked, and its findings reach a build only by a release from the person, or by making the build unclean.

**Marks persist in a Tool's source.** Approving an unclean build does not clean it. Every later rebuild that reads that source is unclean, until the Tool is re-derived from the intent and the checks alone.

**Standing authority binds to a Tool or a standing task, never to the Agent.** A **standing task** is a recurring task the person defined by name, with a fixed objective and limits. The Agent can start it and cannot change it. A grant to a Tool binds to three things: the Tool's identity in the State, the exact needs the person saw when they granted, and a clean build. A rebuild that keeps all three keeps the grant. A rebuild that changes the needs, or that read open input, does not: the person is asked again, and until they answer the earlier build stays live.

**An ad hoc task that needs to cross is authorized once, at its start.** The person sees its objective and limits in fixed words and answers once. That answer covers the task's fetches (GET, HEAD, and its model requests, to parties cleared for everything it holds), which continue after a mark, and the acts whose exact content was shown at the start. Any other crossing act composed after the task was marked asks again with its exact content, or uses an adapter rule for that source.

**Work inside the boundary has a ceiling.** The person sets a ceiling on the limits of tasks that do not cross: how many objects, and which parts of their Data. A task proposed above the ceiling is authorized at its start, like a task that crosses.

**An Allowance is standing authority like any other.** A marked task spends from it only by the person's answer or through an adapter rule.

**The Agent proposes the limits of every task.** Those limits bound a task's mistakes. They do not bound a steered Agent: what bounds the Agent is that it holds no standing authority itself, that its context is kept clean (ADR 0008), and the ceiling above.

We chose task principals because a single long-lived Agent collects every grant it was ever given, and a durable intent such as "help me manage my finances" is too broad to judge one action against. A task gives the gate and the monitor something narrow to check: this objective, these objects, until this time.

Revised after council rounds 2, 3, and 4. The first version cut a marked context off from every crossing, including its own cloud model, and let it hand "a narrow result to a clean task". The second treated a connection to a site named in the task as safe, which let private data leave in a query string, and did not say who holds standing authority.

## Considered Options

- **One Agent principal that holds all authority, checked against durable intents (the earlier design).** Rejected: checking against broad intents either allows too much or asks too often, and one injection reaches everything the Agent can do.
- **A marked context loses every crossing (the round 1 text).** Rejected: a task that thinks with a cloud model would stop at the first web page or package it read.
- **All untrusted input is alike.** Rejected: a pinned package snapshot cannot be aimed at one person at one moment, and a live web page can.

## Consequences

- Durable intents and temporary tasks are separate things. The person can authorize a task once and let it run inside its limits.
- An admitted set can still contain hostile text. Who admits what, and how it is reviewed, decides how much that grading is worth.
- A steered Agent can start any standing task and can propose any ad hoc task. The first is bounded by that task's fixed limits, the second by the person's answer.
- The person cannot review code. When a rebuild asks again, they see the needs and the check results, and the grant's effect limits bound what the code can do.
