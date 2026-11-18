# The Agent coordinates; each task is its own principal, and untrusted input costs it authority

Each piece of agent work is a **task**: a fresh principal with a stated objective, effect limits, a budget, and an expiry. The Agent keeps the person's history and starts tasks, but it does not hold the authority of the tasks it has run. When a task ends, its handles end with it.

**Untrusted input is graded.**

- **Admitted** input comes from a pinned set the person admitted in advance: the Base package snapshot, and the dependency set with its documentation that build tasks may use. Reading it does not mark a context. Admission is where that risk is taken, once, by a step that can be audited.
- **Open** input is everything else written by someone other than the person or their OS: web pages, mail, messages, shared intents, files from outside. Reading it **marks** the context, and the mark follows everything the context emits (ADR 0008).

**A marked task keeps thinking and loses authority.** It keeps its model, through a request the broker builds: to the provider the person chose, with the provider's own tools and retention off. It loses every standing grant and standing rule to cross the undo boundary: to act on the person's accounts, to send data out of the fleet, or to do anything inside the fleet that cannot be undone. From then on such an act needs the person's answer, with one exception: a standing rule the person wrote for that source, carried out by a broker adapter that knows the operation ("pay this bill on this site, up to this amount each month"). The limits in that rule are then the only protection, and the rule says so. An act that depends on a result from a marked task is marked too (ADR 0008).

**Egress goes only to parties cleared for the data.** A compartment may connect only to parties cleared for everything it holds (ADR 0008). A web site is cleared for public data by default, so a compartment that has read private data cannot fetch a stranger's page. No principal may hold private read, open untrusted input, and a connection to a party not cleared for that data. The gate refuses the third.

**A lookup holds only what the person typed and what it found.** A **lookup** is a task that answers a question from the open web. It runs in a compartment of its own, which holds the words the person typed, exactly as typed, and the pages it reads. It cannot see Agent memory or the person's Data. What the person types is personal by rule (ADR 0008), and typing it as a question for the web is the person's own act, so the turn is released to the lookup, which holds it as public ([ADR 0025](0025-what-the-person-does-by-hand-is-the-answer.md)). The ledger records the release. So a lookup is public, and it starts, keeps searching, and takes the person's follow-ups with no ask. The broker checks its objective, and each follow-up, against what the shell recorded: it is the turn the person typed last in that conversation, whole and unchanged. A turn leaves once: it starts one lookup, or it is one follow-up to one running lookup, and a second use asks. A lookup runs on one fixed default for its limits, its budget, and its expiry, and the Agent sets none of them. Any text the Agent adds to those words, pasted text included, is shown to the person before it leaves, as a release in full: the exact text, and that it goes to the open web. A lookup started from a conversation that is no longer clean follows the same rule. It holds the turn the person typed last and nothing else that the conversation holds, so it starts with no ask. Anything else that conversation would send as a lookup is one of its acts that cross, and asks (ADR 0008): a turn with a word changed, an earlier turn, text the Agent added, or a follow-up the person did not type. A standing task that looks things up has a fixed objective.

**A compartment that holds only public data and no login may reach any host on the open internet without asking.** Machines in the fleet and addresses on the home network are not among them: a command to a device there cannot be taken back, so it crosses like any other (ADR 0003). That covers a lookup, a task browser context that is not logged in ([ADR 0013](0013-agents-never-drive-the-persons-browser.md)), and a workbench for a project the person declared public when they made it ([ADR 0017](0017-a-pinned-catalog-and-workbenches.md)). Nothing private is inside to leak. Secrets stay in the vault, and the broker fills them in only for the hosts they are tied to (ADR 0006).

**Build tasks are clean by construction.** A task that writes or changes a Tool's code reads only the intent, the checks, the Tool's own source, and admitted input. It has no web access. It sees results on synthetic inputs only: a replay of real recorded inputs reaches it as pass or fail. That verdict is computed from private, marked inputs. It is the one such result a build may read and stay clean, and it is capped at a fixed number of replays per re-derive, so a build cannot search a real input one bit at a time. Fixing a bug that needs the real input goes through the person, who turns that input into a new check. A **lookup** may browse; it is marked, and its findings reach a build only by a release from the person, or by making the build unclean.

**Marks persist in a Tool's source.** Approving an unclean build does not clean it. Every later rebuild that reads that source is unclean, until the Tool is re-derived from the intent and the checks alone.

**Standing authority binds to a Tool, a standing task, a workbench, or installed software, never to the Agent.** A **standing task** is a recurring task the person defined by name, with a fixed objective and limits. The Agent can start it and cannot change it. A grant to a Tool binds to three things: the Tool's identity in the State, the exact needs the person saw when they granted, and a clean build. A rebuild that keeps all three keeps the grant. A rebuild that changes the needs, or that read open input, does not: the person is asked again, and until they answer the earlier build stays live. A workbench ([ADR 0017](0017-a-pinned-catalog-and-workbenches.md)) also holds standing authority. It installs from the open web, so it is always marked and never a clean build. Its grants bind to its identity in the State and the exact needs of its profile, and a new need asks the person again. Reaching the hosts its profile lists is what its approval granted, and does not ask. Being marked, it acts on the person's accounts only through standing rules carried out by an adapter, or with the person's answer.

**Installed software holds standing authority too.** Its approval screen (ADR 0017) is its standing grant, and its standing rule for the hosts and folders that screen showed. The grant binds to its identity in the State and the needs the approval showed. Using what the approval showed never asks, and a new need asks the person again.

**A model request does not ask.** The broker makes it when the provider is cleared for everything the compartment holds and the model budget is not spent. The clearance and the budget are the standing authority for it, and each was the person's signature. A request to a provider that is not cleared is refused, and one over the budget asks.

**A task asks once for what it does across the boundary, and never twice for one act.**

- Where the exact content of an act is known when the task starts, the person sees it then, and that answer covers the act.
- Where the task composes the act later, it starts with no ask, and the act asks with its exact content when it is ready. A task that reads three mails and drafts three replies costs one ask: the replies, as a list ([ADR 0026](0026-asks-wait-and-take-one-of-three-answers.md)).
- A task on a logged-in site takes one answer for that site (ADR 0013).
- Any other act that crosses, composed after the task was marked, asks with its exact content, or uses an adapter rule for that source.
- An act the person answered leaves at once (ADR 0003).

**Work inside the boundary has a ceiling.** There is one default ceiling on the limits of tasks that do not cross: how many objects, and which parts of the person's Data. No setting changes it yet. A task proposed above the ceiling is authorized at its start, with its objective and limits shown in fixed words, by the confirm gesture.

**An Allowance is standing authority like any other.** A marked task spends from it only by the person's answer or through an adapter rule.

**The Agent proposes the limits of every task but a lookup**, whose limits are the fixed default. Those limits bound a task's mistakes. They do not bound a steered Agent: what bounds the Agent is that it holds no standing authority itself, that a conversation changes only by the person's act (ADR 0008), and the ceiling above.

We chose task principals because a single long-lived Agent collects every grant it was ever given, and a durable intent such as "help me manage my finances" is too broad to judge one action against. A task gives the gate and the monitor something narrow to check: this objective, these objects, until this time.

Revised after council rounds 2, 3, 4, and 5. The first version cut a marked context off from every crossing, including its own cloud model, and let it hand "a narrow result to a clean task". The second treated a connection to a site named in the task as safe, which let private data leave in a query string, and did not say who holds standing authority. Until round 5 every lookup asked before it started, every task that would cross asked at its start and again at its act, and the gate asked on every model request and every connection from installed software.

## Considered Options

- **One Agent principal that holds all authority, checked against durable intents (the earlier design).** Rejected: checking against broad intents either allows too much or asks too often, and one injection reaches everything the Agent can do.
- **A marked context loses every crossing (the round 1 text).** Rejected: a task that thinks with a cloud model would stop at the first web page or package it read.
- **All untrusted input is alike.** Rejected: a pinned package snapshot cannot be aimed at one person at one moment, and a live web page can.
- **Every lookup asks at its start (the round 4 text).** Rejected: the person typed the question, so the ask shows them their own words. With nothing but those words inside the lookup, there is nothing else for it to carry out.
- **A lookup from a conversation that is no longer clean asks (the first round 5 text).** Rejected: the ask shows the person the words they just typed, so it would be answered yes nearly every time, which [ADR 0024](0024-the-friction-bar-ranks-with-the-attack-list.md) counts as a defect. In two of the three reviewers' estimates it was most of the distance between the bar and the day of a person who is not a developer. The lookup holds nothing the conversation read, so the ask protected only the turn itself.
- **A lookup that knows the person, from Agent memory.** Rejected: its objective would no longer be the typed words, and every lookup would be a release.
- **Every task that will cross asks at its start (the round 4 text).** Rejected where the act is composed later: the act asks with its exact content anyway, so the start ask is a second ask for one act.

## Consequences

- Durable intents and temporary tasks are separate things. The person can authorize a task once and let it run inside its limits.
- A lookup does not know the person unless they tell it.
- A wrong Agent can send the turn the person just typed out as a lookup, though they meant it for something else. It leaves exactly as typed, once, and no other content leaves with it.
- In a conversation that is no longer clean, a steered Agent can do the same, and a turn typed about private data is more likely to say something private. A steered Agent can also ask the person to type something, and then send it.
- How a turn goes out is the conversation's choice, so a steered one can signal with it: whether it sends the turn, when, and whether it starts a new lookup or follows up a running one. The search service and the sites the lookup reads can see that signal.
- A real key the person typed or pasted into a file in a public project can be sent anywhere by an injected coding agent (ADR 0017).
- An admitted set can still contain hostile text. Who admits what, and how it is reviewed, decides how much that grading is worth.
- A steered Agent can start any standing task and can propose any ad hoc task. The first is bounded by that task's fixed limits, the second by the person's answer.
- The person cannot review code. When a rebuild asks again, they see the needs and the check results, and the grant's effect limits bound what the code can do.
