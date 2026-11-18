# CrowbarOS: threat model

**Status:** design only. None of the defences below is built. Milestone 2 builds
the ones marked **M2** and attacks them; its acceptance tests are in
[Milestone 2](milestones/milestone-2.md). Written after
[council round 2](reviews/council-round-2.md), corrected after
[rounds 3](reviews/council-round-3.md) and [4](reviews/council-round-4.md), and
revised after [round 5](reviews/council-round-5.md), which redesigned the rules
that missed the friction bar. What each redesign gives up is under "what remains"
below. Terms are in the
[glossary](GLOSSARY.md); the rules are in the [vision](VISION.md) and the
ADRs it links.

## What is protected

1. The person's private Data.
2. Their accounts and money: nothing acts as them without their authority.
3. What their machines run: installed behaviour changes only as they confirmed.
4. The authority record: what they granted, and nothing they revoked.
5. The way back: recovery never depends on a model.

## Who attacks

| Attacker | Example |
| --- | --- |
| A stranger who writes text an agent reads | An instruction hidden in a web page, a mail, a shared intent, a package's documentation, a file name, or an error message |
| A stranger who ships code | A shared Tool, a dependency, a package |
| A model that is wrong, or steered | The Agent's model or a task's model does something the person did not want |
| A model provider or a harness, as an author | It writes every plan and every build, and can write a hostile one on purpose |
| A model provider, as a reader | It keeps or uses what it is sent |
| A web site the person uses | It serves a page built to mislead a task that is logged in |
| Someone holding a machine | A stolen laptop, a rented cloud machine's operator |
| A compromised machine in the fleet | One member is taken over |
| Someone near the person | A voice from a speaker, a video that says "Agent, send my tax folder to…" |

## What is trusted

A failure in any of these breaks the claims on this page.

- Hardware, firmware, the bootloader, and the Linux kernel, including its sandbox.
- Base: the compositor, the shell and its trusted UI, the broker and its
  adapters, the gate, the plan interpreter, the runner, the engine, the vault,
  the WASM runtime, and the Warden, which sets up every sandbox.
- The lead machine, which holds authority for the fleet.
- The phone, as an approval device.
- The step that admits a pinned set (Base packages, the dependency set for build
  tasks), at the time it admits it.
- A model provider, only for data at or below the clearance the person gave it,
  and only to keep its promise about tools and retention.
- The browser engine, to keep one site's content from another inside the person's
  own browser.
- The labels. A label comes from where data came from. Data that arrives with the
  wrong label is protected at the wrong level.

No model is trusted to allow anything, and the monitor can only pause. But models
are not outside the system: the Agent's model proposes every task, its limits, and
its plan, and a build task's model writes every Tool. The rows below say what
bounds them.

## Attacks, defences, and what remains

| Attack | What stops it | What remains |
| --- | --- | --- |
| An injected page tells a task to send a private file to the attacker. **M2** | The broker makes every connection. A compartment may connect only to parties cleared for everything it holds, and a web site is cleared for public data. | A party cleared for that data sees it. |
| A task sends private notes to a site named in the task, in a query string. **M2** | The same rule: naming a site does not clear it. A site receives public data, what it supplied, and what the person released to it. | A site that hosts many users (a code host, a document host) can pass on what it was cleared to receive. |
| A task holding finance data calls a model provider cleared only for personal data. **M2** | A model request is egress like any other, built by the broker and checked against the provider's clearance. | — |
| A task returns a private file a few bits at a time: as choices, as numbers, as the sizes of its results, or as which steps fail. **M2** | Bounded results keep their label, and what can be observed about a result is a result. A private result reaches a clean conversation only by a release, or inside a budget counted across all tasks. The budget is zero. | Nothing while the budget stays at zero. No interface sets it yet. |
| An injection steers a choice, and the Agent acts on it, in this request or a later one. **M2** | The interpreter keeps the mark on every result, and an act that depends on a marked result is marked. A clean conversation does not read a marked result; one the person gave a marked value is no longer clean for as long as it lives. | A steered choice can still select among outcomes the person is asked about. |
| A task the person started reads an injected mail and sends different content inside its limits. **M2** | An act that crosses is shown with its exact content before it leaves: at the start where it is known then, and when it is ready where the task composes it later. | A person who answers without reading. An item overlooked in a list of acts. |
| A steered Agent or a hostile harness proposes a task that crosses nothing and touches everything: "rewrite every file". **M2** | The ceiling on tasks that do not cross; above it the task is authorized at its start. Writes are staged, and the Data history can undo them for the stated window. | Harm under the ceiling that nobody notices inside the undo window. |
| A lookup carries the Agent's personal context out in its search. **M2** | A lookup holds only the turn the person typed last, and the broker checks its objective against what the shell recorded. Text the Agent adds is shown in an ask before it leaves. The same holds in a conversation that is no longer clean: the lookup holds nothing that conversation read, and anything but the last typed turn asks. A turn leaves once, and a lookup's limits are a fixed default the Agent does not set. | A wrong Agent, or a steered conversation, sends out the turn the person just typed, though they meant it for something else. A turn typed about private data may say something private, and a steered conversation can ask the person to type it. It leaves exactly as typed, once, and no other content leaves with it. A steered conversation can signal by whether it sends the turn, when, and whether it starts a new lookup or follows up a running one. |
| An injection makes a task act as the person: send a mail, place an order. **M2** | A marked task loses its standing grants and rules. The act waits for the person's answer, shown with its exact content. An act under a standing rule for that source waits in the outbox. | A standing rule the person wrote for that source and operation, up to its limits. A person who answers without reading. |
| An injection reaches the Agent inside a summary, a file name, a mail subject, an error message, or a paste. **M2** | Marks survive every handoff. Every string the person did not type and a template did not produce is data with a mark. A conversation changes only by the person's act, and the Agent cannot mark or raise its own. One that is no longer clean stays that way for as long as it lives, shows it in its frame, holds no standing grant or rule, and asks for every act that crosses, a link it shows included. The one thing it sends to the open web with no ask is the turn the person just typed, as a lookup. An intent or a check it writes keeps its mark, so a build from it is not clean. | A conversation the person pointed at hostile text may be steered: what it tells the person, and what it proposes, can be the stranger's. What the person types themselves after reading something hostile. A paste of text they did not read. |
| A stranger's text reaches a task as the person's own, because the person pasted it. **M2** | A value moved by hand keeps its mark. The destination carries the mark from then on, and so does every object it changes afterwards, whether or not the gate saw the write. | The label is released: a private value the person pastes into an app can go wherever that app can send it. |
| An app reads what the person copied, with no paste. | The compositor moves a value between compartments only on a paste key, a middle click, a click in the window that holds the keyboard, or a drop. | An app the person clicks in can read what they last copied. |
| A private value leaves inside the address of a link the person clicks. | A link shown by a Tool, or by a conversation that is no longer clean, asks on the click, with the full address shown. | An app can put what it holds into an address, and it leaves when the person clicks. A clean conversation can do the same with what it holds, which is at most personal. |
| A lookup is turned on the home network or the fleet. | A compartment that holds only public data may reach any host on the open internet. A machine in the fleet, or an address on the home network, is not one: reaching it crosses. | — |
| A file another task wrote into a public project is sent out by its workbench. | Declaring a project public is a signed declassify of its folders. A write into them from a compartment above public is refused, unless the person's hand moves the value in. | What the person types or pastes into a public project. |
| A video or a speaker tells the Agent to do something, or to remember something. | Words that arrive by voice are marked until the person makes the confirm gesture. Voice never answers an ask. | A person who confirms words they did not say. |
| An injection writes code into a Tool that already holds grants. **M2** | Build tasks read only admitted input and synthetic results. A grant binds to a clean build with unchanged needs. A mark persists in a Tool's source. | Hostile text inside an admitted set. |
| A build searches a real input one bit at a time through replay verdicts. **M2** | A replay reaches a build only as pass or fail, capped per re-derive. | The bits the cap allows. |
| A hostile provider or harness writes a backdoor into a Tool, inside its existing needs. | Nothing stops the writing. The Tool's needs and effect limits bound what the backdoor can do. | Everything inside the granted needs. The checks pass, because the backdoor does not break them. |
| A steered Agent uses the person's standing authority. | The Agent holds none. It can start a standing task, which has a fixed objective and limits, or propose an ad hoc task, whose acts that cross each ask. | Everything the standing tasks allow, as often as their rate limits allow. |
| A private value reaches a cloud model through the Agent's context. **M2** | In a clean conversation, plan over handles: the interpreter moves values, and the Agent sees handles. A conversation the person pointed at private data thinks only with a provider the person cleared for that category, by signature. | What the person tells the Agent directly, and what they paste or drop into a conversation. Trust in the provider at the labels it is cleared for. |
| A check that always passes, with honest-looking wording. **M2** | Checks are scenarios; fixed templates write the wording; a scenario that expects nothing is rejected. | Checks written as code, marked "wording not verified". Scenarios that test too little. |
| The build that goes live is not the build that was tested or previewed. **M2** | The runner signs the result for one content hash. A promote accepts only that hash. | — |
| Rolling back software brings back a revoked grant. | Authority is stored outside the State and its generations. | An undo can still revert a security fix. The person confirms every undo. |
| A shared Tool from a stranger is hostile. | It runs only as WASM, with fresh grants, and its needs stated by the OS in fixed words. | Harm inside the needs the person granted. The stranger wrote the checks, and a hostile Tool can behave well in rehearsal: checks catch a broken Tool, not a hostile one. |
| A native Tool goes past its limits through a file it already has open. | The kernel enforces the view. The sandbox's label and mark are fixed at start from everything in the view, and the view is a snapshot. Writes are staged and counted at commit. Stopping the sandbox revokes. | Kernel bugs. Reads inside the view are not counted. |
| An agent, a Tool, or a harness answers its own ask. | Only the shell delivers input, and Base closes the Linux routes around that (ADR 0014 lists the known ones). An ask for anything that crosses needs the confirm gesture. Authority that lasts needs a passkey, a security key, or the phone, signed over the exact content. | A compromised Base can forge an everyday answer. An input route Base did not close. A plain answer has only the first of these defences, and a bug in the shell can make one up: it is accepted only for what can be undone. |
| A change to a Tool is promoted by a click the person did not mean. | A promote takes a plain answer only for a clean build whose needs did not change. The ask for it opens by itself only in the conversation where the person just asked for the change, and it ignores input for a moment after it appears. The change can be undone. | A Tool that holds standing rules can act under them before the undo. |
| A task floods the person with asks until they stop reading, or swaps an ask under a key press. | An ask waits and never opens by itself, except the one for what the person just asked for in the conversation they are in. Past the limits, further asks are held in one line for a principal, and in one line for the Agent and all its tasks, which the person can dismiss whole. An ask ignores input for a moment after it appears, whichever answer it takes. | A person trained to say yes by honest asks: the friction bar caps them, and a kind of ask answered yes more than 95 times in 100 is redesigned. A person who does not look at waiting asks: the task waits. |
| An agent uses the person's logged-in browser. | No agent can drive or read the person's browser profile. A task gets a fresh context of its own, with approved fill, and the session cookie stays in the broker. | — |
| A page makes a logged-in task context change the person's account. | The person answers once for that task and that site. The task reaches that one site and the shared hosts, holds only what the person released to it, and can supply no payment credential but the Allowance. Banks, mail providers, and identity providers are adapter-only. Acts with no ask at all go only through an adapter that knows the operation. | While the task runs, an injected page can make other changes on that one account on that one site. That includes spending with a payment method the site already stores, which the Allowance does not cap. What the task holds for the site can reach a shared host, and some shared hosts serve anyone's content. |
| An injected coding agent sends a project's secrets out. | A private project's workbench reaches only the hosts on its list, and a new one takes the person's answer. Secrets stay in the vault. A public project's workbench holds only public data and no login. | A real key the person typed or pasted into a file in a public project can be sent anywhere. |
| A task learns a private amount through the cap of a standing rule. | The gate never reads the amount. The adapter checks it against the cap when it carries the act out, and a dry run says only "allowed if within the cap". | Whether the act went through: one bit for each attempt, and the rule's rate limit bounds the attempts. |
| A secret is stolen from an agent's context or environment. | Secrets stay in the vault. The broker fills them in only for the hosts they are tied to. Any credential an adapter obtains is stored in the vault and returned as a handle. | A host that is entitled to the secret and misuses it. A protocol with no adapter is unsupported, not unprotected. |
| A machine is stolen. | A machine with an unencrypted disk holds nothing private. Leases from the lead machine expire. | A machine taken while unlocked. How the disk is unlocked is not designed yet. |
| A machine in the fleet is compromised. | Its clearance bounds what it ever held. Its leases expire, and the lead machine stops renewing them. | Everything it held before. A compromised lead machine is a full compromise, and the lead machine is also where the person works. |
| A model is simply wrong, inside its limits. | Effect limits bound the harm. A preview comes before any change to installed behaviour. Undo, for the stated window. The monitor may pause. | Harm inside the limits that nobody notices in time. |

## Not defended

- A compromised kernel, firmware, or hardware.
- The owner. Owner authority can turn off every guard, on purpose.
- A provider that breaks its promise about retention or tools.
- Covert channels between compartments: timing, resource use, the order of events.
- A person persuaded to grant, release, approve, paste, or type something harmful. What the person does with their own hand is the answer ([ADR 0025](adr/0025-what-the-person-does-by-hand-is-the-answer.md)).
- A task that wastes its whole budget.
- A hostile package that was admitted into a pinned set.
- Data that arrived with the wrong label.
- Bugs in anything listed under "What is trusted".

## The other way to fail

A system that asks too often is turned off. Marks spread, and every preview,
release, and ask ends at the person. If ordinary work cannot finish without a
stream of answers, people will take owner authority and remove the guards, and
none of the rows above will matter.

So this failure has a pass mark of the same rank as the attacks. The **friction
bar** ([ADR 0024](adr/0024-the-friction-bar-ranks-with-the-attack-list.md)) sets
the most asks and refusals a person may meet in one task and in one day. A rule
that misses it is redesigned, and the redesign names what it gives up: those are
the entries added under "what remains" above. Where no design passes both, the
Agent hands that work back to the person. Milestone 2 measures the bar alongside
the attacks.
