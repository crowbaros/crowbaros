# Design: the gate, the monitor, and autonomy

**Status:** draft. Nothing in this document is built yet. Milestone 1 has only the
pieces this design builds on: Base generations, rollback, an apply lock, and the
ledger. Revised after council rounds [1](../reviews/council-round-1.md),
[2](../reviews/council-round-2.md), [3](../reviews/council-round-3.md), and
[4](../reviews/council-round-4.md). The
"approver" of the earlier
design is now two things, the gate and the monitor. What the design defends
against is in the [threat model](../threat-model.md).

## Summary

- **Security comes first, and it is deterministic.** No model is on the trust
  path. See [ADR 0003](../adr/0003-security-is-deterministic.md).
- **The gate allows; the monitor can only pause.** The **gate** is a deterministic
  rules engine and the only thing that allows an action. It checks every broker
  operation and every commit of staged writes. The **monitor** is a
  model that reviews actions afterwards. It can pause a task and raise a surprise.
  It can never allow.
- **Work happens in tasks.** A **task** is a fresh principal with a stated
  objective, effect limits, a budget, and an expiry. The Agent coordinates tasks
  and does not collect their authority. See
  [ADR 0011](../adr/0011-task-principals.md).
- **Effect limits bound every task and every grant:** which objects, how many, for
  how long, at what cost, and how often.
- **The undo boundary is the default for autonomy, not a safety property.** An
  effect needs no answer from the person only when it is within the task's limits,
  the system can undo it for a stated window, and it stays inside the fleet.
- **Acts on the person's behalf wait in the outbox** for a visible delay and can
  be cancelled until they leave.
- **Untrusted input costs a task authority.** Reading open untrusted input
  **marks** a context. A marked task keeps its model and loses its standing
  grants and rules to cross the undo boundary, which includes anything inside
  the fleet that cannot be undone. An act that depends on a result from a marked
  task is marked too.
- **Labels and marks survive every handoff and storage, and nothing crosses
  freely.** The Agent works by **plan** over handles, and the interpreter keeps
  the label and mark on every result, and on what can be observed about a
  result. The Agent does not read a result from a marked task. A private result
  reaches the Agent only by a **release** or inside a budget the person set. See
  [ADR 0008](../adr/0008-private-values-stay-out-of-the-agent-context.md).
- **Egress goes only to parties cleared for the data.** A model provider's
  default clearance is personal. A web site's is public.
- **Standing authority binds to a Tool or a standing task, never to the Agent.**
  An ad hoc task that needs to cross is authorized once, at its start. A task
  that crosses nothing has a ceiling the person set.
- **A "yes" comes from the person.** Only the shell delivers input. Everyday asks
  take a confirm gesture, and a change to standing authority needs a hardware
  root. See
  [ADR 0014](../adr/0014-a-yes-comes-from-the-person.md).
- **The gate explains itself to agents:** a dry run, refusals that name the rule
  and the limit, idempotency keys, and a handle's label and mark.
- **Intents change only through the person's own Agent, with the person's
  confirmation in trusted UI.** The OS enforces the Shaping contract.
- **Nothing goes live unconfirmed.** Every change to installed behaviour is shown
  as a preview, and one confirmation promotes exactly what was previewed.
- **Notify only on surprise.** "Mode" controls only how much the person is told.
- **Authority has its own domain.** Standing grants, standing rules, effect
  limits, Allowances, and clearances are not in the Crowbar State and are never rolled back
  with software. See [ADR 0012](../adr/0012-authority-outside-the-state.md).
- Everything goes in the **ledger**, with one-step revoke or undo.

## Agent, harness, model, and tasks

```mermaid
flowchart LR
    P[Person] <-->|Shaping, point-and-say| H
    subgraph ROLE["Agent (role defined by the OS)<br/>identity, signs ledger entries, starts tasks"]
        H["Harness<br/>default CrowbarOS harness,<br/>or an external one such as Grok Bot"]
        M["Model<br/>a cloud model,<br/>or open weights in the fleet"]
        H --> M
    end
    H -->|"harness protocol (MCP):<br/>propose intent edits, start tasks,<br/>write plans, request handles,<br/>ask for a preview, read the ledger"| OS
    subgraph OS["CrowbarOS (deterministic)"]
        SC[Shaping contract check]
        G[Gate]
        BR[Capability broker]
    end
    OS --> T1["Task<br/>objective, effect limits,<br/>budget, expiry"]
    OS --> T2[Task]
```

- **Agent** is the person's own principal. It has an identity, signs its ledger
  entries, edits intents with the person, and starts tasks. The role holds the
  authority, whatever harness fills it.
- **Harness** is whatever fills the role: the loop, tools, and conversation around
  a model. CrowbarOS ships a default harness. An external one can replace it. A
  harness can only propose.
- **Model** is what a harness thinks with. We assume the common case is a cloud
  model. Open weights on the person's own hardware (for example, a DGX Spark in
  the fleet) are the other case. See
  [ADR 0008](../adr/0008-private-values-stay-out-of-the-agent-context.md).
- **Agent memory** is what the Agent has learned about the person. It is the
  person's Data, not the harness's, so changing the harness keeps it.

### Tasks

Each piece of agent work is a **task**: a fresh principal with

- a stated **objective** ("prepare this month's tax documents"),
- **effect limits**: which objects, how many, for how long, at what cost, and how
  often,
- a **budget**, and
- an **expiry**.

The Agent keeps the person's history and starts tasks. It does not hold the
authority of the tasks it has run. When a task ends, its handles end with it.

Durable intents and temporary tasks are separate things. "Help me manage my
finances" is an intent. "Prepare this month's tax documents, using these
accounts, without contacting anyone" is a task. An intent is too broad to judge
one action against. A task gives the gate and the monitor something narrow to
check: this objective, these objects, until this time.

A task's authority comes only from what the person has granted.

- **Standing authority binds to a Tool or a standing task, never to the Agent.**
  A **standing task** is a recurring task the person defined by name, with a
  fixed objective and limits. The Agent can start it and cannot change it.
- **An ad hoc task that needs to cross is authorized once, at its start.** The
  person sees its objective and limits in fixed words and answers once. That
  answer covers two things. The first is the task's fetches: GET, HEAD, and its
  model requests, to parties cleared for everything it holds. Those continue
  after a mark. The second is the acts whose exact content was shown at the
  start. Any other crossing act composed after the task was marked asks again
  with its exact content, or uses an adapter rule for that source.
- **Work inside the boundary has a ceiling.** The person sets a ceiling on the
  limits of tasks that do not cross: how many objects, and which parts of their
  Data. A task proposed above the ceiling is authorized at its start, like a
  task that crosses. Without it, "rewrite every file" would ask no one.
- **The Agent proposes the limits of every task.** Those limits bound a task's
  mistakes. They do not bound a steered Agent. What bounds the Agent is that it
  holds no standing authority itself, that its context is kept clean, and the
  ceiling. A steered Agent can start any standing task, which is bounded by that
  task's fixed limits, and can propose any ad hoc task, which is bounded by the
  ceiling or the person's answer.

Every task has a **workspace**: a private scratch area and a view of the Data it
was given. Its writes to Data are staged there and committed together when the
task finishes, inside its effect limits. Only a task that changes installed
behaviour (a Tool, a service, the desktop) also gets a **fork** of the
Environment. The person sees the result as a **preview**, and one confirmation
promotes exactly the build that was previewed. See
[ADR 0007](../adr/0007-fork-verify-promote.md) and
[state and apply engine](state-and-apply-engine.md).

### Untrusted input costs a task authority

**Untrusted input** is content written by someone other than the person or their
OS. Injection arrives this way. It is graded
([ADR 0011](../adr/0011-task-principals.md)):

- **Admitted** input comes from a pinned set the person admitted in advance: the
  Base package snapshot, and the dependency set with its documentation that build
  tasks may use. Reading it does not mark a context.
- **Open** input is everything else: web pages, mail, messages, shared intents,
  files from outside, and what the person pastes. It includes short strings: a
  file name, a mail subject, error text from a server. Reading it **marks** the
  context, and the mark follows everything the context emits.

A marked task keeps thinking and loses authority:

- It **keeps its model**, through a request the broker builds: to the provider
  the person chose, with the provider's own tools and retention off.
- It **loses every standing grant and standing rule to cross the undo
  boundary**: to act on the person's accounts, to send data out of the fleet, or
  to do anything inside the fleet that cannot be undone. From then on such an
  act needs the person's answer.
- The one exception is **a standing rule the person wrote for that source,
  carried out by a broker adapter that knows the operation** ("pay this bill on
  this site, up to this amount each month"). The limits in that rule are then the
  only protection, and the rule says so.
- **An act that depends on a result from a marked task is marked too**, so a
  steered choice cannot spend a standing rule.
- **A standing task started from a marked context runs marked.**
- **An Allowance is standing authority like any other.** A marked task spends
  from it only by the person's answer or through an adapter rule.

**Egress goes only to parties cleared for the data.** A compartment may connect
only to parties cleared for everything it holds. A web site is cleared for
public data by default. It may also receive back what it supplied, and what the
person released to it. So a compartment that has read private data cannot fetch
a stranger's page, and a research task that browses holds nothing above public.
The objective of a research task leaves the fleet with its first request. So
the ask that starts one is a release in full: it shows the exact text that may
leave, and the sites or the search service it may go to. A standing task for
research has a fixed objective, and any text the Agent adds to it is shown to
the person before it leaves. No principal may hold private
read, open untrusted input, and a connection to a party not cleared for that
data. The gate refuses the third.

**Build tasks are clean by construction.** A task that writes or changes a
Tool's code reads only the intent, the checks, the Tool's own source, and
admitted input. It has no web access. It sees results on synthetic inputs only:
a replay of real recorded inputs reaches it as pass or fail. That verdict is
computed from private, marked inputs. It is the one such result a build may
read and stay clean, and it is capped at a fixed number of replays per
re-derive, so a build cannot search a real input one bit at a time. Fixing a bug that
needs the real input goes through the person, who turns that input into a new
check. A research task may browse. It is marked, and its findings reach a build
only by a release from the person, or by making the build unclean.

**Marks persist in storage.** A Tool's source and an entry in Agent memory keep
the mark of whatever wrote them. Approving an unclean build does not clean it.
Every later rebuild that reads that source is unclean, until the Tool is
re-derived from the intent and the checks alone.

### Labels and marks survive every handoff

A **compartment** is one execution context: a task's model context and its
workspace, or a Tool's sandbox. Once it reads a value, it is at least at that
value's label and carries that value's mark, and so is everything it emits. See
[ADR 0008](../adr/0008-private-values-stay-out-of-the-agent-context.md) and
the [capability model](capability-model.md).

**Nothing a model does removes a label or a mark, and nothing crosses freely.**
Summarizing does not clean a value, starting a new task does not, and passing
through the Agent does not.

**The Agent works by plan over handles.** The Agent writes a **plan**: a small
program whose variables are handles. A handle names a value without revealing
it. A deterministic interpreter in the fleet runs the plan and moves the actual
values between Tools, broker operations, and model calls. It tracks the label
and mark of every result. That includes handles, and **bounded results**: a
choice from a list, or a number in a range, both fixed before the task read
anything.

- A plan may branch on a bounded result. Everything that follows the branch
  carries that result's label and mark.
- An act that depends on a result from a marked task counts as marked.
- **The Agent does not read a result from a marked task.** The plan branches on
  it inside the interpreter, and the shell shows it to the person. If a marked
  result does enter the Agent's context, that context is marked for as long as
  it lives, together with its history and any summary of it. A new request
  clears nothing. Only a fresh context with no marked input starts clean. A
  model's context cannot track what an act depends on, so the Agent's context is
  all or nothing.
- **What can be observed about a result is a result.** The size of a value, the
  number of handles a task returns, and whether a step succeeded or was refused
  carry the label and mark of what they were computed from. They are hidden from
  the Agent, or charged to the budget, like any other result.
- A private result reaches the Agent's context only by a **release**, or inside
  a budget the person set for that category. The budget is counted in bits
  across all tasks, and its default is zero. The shell can always show the
  person a result directly, without it passing through the Agent.
- A **release** is the person seeing the exact value in trusted UI and letting
  it pass to one named destination.

**Only the person's own words are the person's words.** What the person types
reaches the Agent as theirs. What arrives by voice is marked until the person
makes the confirm gesture, because a speaker can produce it. Content the person pastes or drops
arrives as a handle with its origin's label and mark, not as text in the Agent's
context. Every other string that the person did not type and a fixed template
did not produce is data with its origin's label and mark. The Agent sees what
the OS states about a handle (its kind, label, mark, and id, and its size only
where that does not depend on a private or marked value) and names the person
typed. Synthetic samples are made by deterministic code from the schema,
never by a model from real Data.

The Agent's own context holds handles, schemas, synthetic samples, results it is
allowed to read, and the person's own words. It is personal by default: what
the person types counts as personal by rule. A private value released into it
is released to the Agent's model provider as well, and the ask says so.

**Every outside party has a clearance.** The default for a cloud model provider
is personal. The person can raise it for one category of private data at a time.
The default for a web site is public. Content fetched under a login is private
to that site. A task may read a value and think with a
model only when that model's provider or machine is cleared for the value.

### The harness protocol

The OS exposes one **harness protocol**, built on MCP rather than invented. Through
it a harness can read intents and Shaping context, propose intent edits, start
tasks, write plans, request handles, ask for a preview, and read the ledger. It
cannot promote: the person's confirmation does. Any MCP-capable agent
can fill the Agent role.

The gate must explain itself to agents
([ADR 0003](../adr/0003-security-is-deterministic.md)). The protocol requires:

- a "would this be allowed?" dry run with no side effects, which must not itself
  reveal a value;
- refusals that name the rule and the limit;
- idempotency keys, so a resumed task knows what already happened;
- a way to read a handle's label and mark without reading its value.

An external harness gets no more trust than the default one. Connecting it is a
change to authority that the owner makes once. A harness that runs in someone else's
cloud, such as Grok Bot, is **outside the fleet**. Everything it sees has left the
fleet, so it sees only data at or below the clearance the person gave it. Grok Bot's own cloud
memory and its persistent cloud computer are part of that egress. See
[agent credentials prior art](../research/agent-credentials-prior-art.md).

### The Shaping contract

The OS owns the rules of Shaping. The harness owns the conversation.

- **Every intent has checks, and the person confirms them in trusted UI.** The
  shell shows the checks and takes the answer. A harness cannot assert "the person
  confirmed".
- **Every change is shown as a preview.** The forked Tool runs in the fork,
  marked as a preview, with no effects outside the fork. One confirmation
  promotes exactly the build that was previewed. Nothing goes live unconfirmed,
  however small.
- **A change of authority is confirmed as well.** An intent edit that asks for
  new authority needs the authority change confirmed with a hardware root. See
  "The person's answer" below.
- **An intent edit without confirmed checks is refused**, the same way an action
  without a handle is refused.
- Intents change only through the person's own Agent, and only with the person's
  agreement. Tasks, background agents, agents that come with shared Tools, and
  third-party agents are principals with handles. They can never edit intents.
- The Agent may **propose** a new intent ("You've muted Slack every morning this
  week. Make it an intent?"), but never adopts one silently.
- How the harness asks questions, how deep it digs, and its personality are up to
  the harness. Swapping harnesses changes the conversation, not the rules.

The first text said both that small edits "just happen, with the checks shown
afterward" and that an edit without confirmed checks is refused. After council
round 1 this note let an edit that adds no authority go live unconfirmed and
expire. Council round 2 found the hole in that: an edit that adds no authority
can still rewrite the code of a Tool that already holds grants. So "make the
text bigger" costs the person one confirmation of a preview, like every other
change.

## The gate and the monitor

Permission prompts fail in two ways. They interrupt people, and people learn to
click "Allow" without reading. Letting an agent approve itself fails because a
confused or manipulated agent will approve its own mistake, for example after a
prompt injection from a web page it read.

A model that approves does not fix this. A model can be wrong, and an injection
can hide inside the concrete action itself: a URL that carries encoded data, a
file's contents, a package's install script. So nothing that allows an action is a
model. The earlier design had one "approver" that was both the rules and a model
and signed every handle. That made a model an availability dependency for every
handle. It is now two parts with different powers.

```mermaid
flowchart LR
    AUTH[("Authority<br/>handles, effect limits,<br/>standing grants, standing rules")] --> G
    subgraph TK["Task (untrusted)"]
        M[Works in its workspace or fork]
    end
    M -->|"broker operation or commit only:<br/>'commit 212 moved photos',<br/>'connect api.x.org:443'"| G
    subgraph DET["Deterministic"]
        G{"Gate"}
    end
    G -->|allowed, signed| BR[Capability broker]
    BR -->|handle| M
    G -->|"crosses, no grant or rule"| ASK["Ask the person<br/>in trusted UI"]
    G -->|refused| X[Refused]
    G --> LED[(Ledger)]
    I[("Intents +<br/>the task's objective")] --> MON
    LED -->|"the action, after the gate"| MON
    M -. "its reasoning / argument<br/>(NOT forwarded)" .-x MON
    subgraph ISO["Monitor (separate principal, a model)"]
        MON["Does this action fit<br/>the intents and the task?"]
    end
    MON -->|"pause the task +<br/>raise a surprise"| P[Person]
```

### The gate

- **Deterministic.** It is a rules engine. It works with no model at all.
- **It checks every broker operation, and every commit of staged writes,**
  against the principal's handles, the task's effect limits, standing grants, and
  standing rules, and against the rules for labels, marks, and clearances.
- **It does not see inside a native sandbox.** A native process works inside the
  view it was given, and the kernel enforces that view. Writes made inside a view
  are staged, and the gate counts them when they are committed. See
  [ADR 0004](../adr/0004-capability-safe-tools.md).
- **It is the only thing that allows.** It signs what it allows, and the broker
  issues a handle only with that signature.
- **It reads authority; it does not write it.** Standing grants, standing rules,
  effect limits, and clearances live in the authority domain. Only the person
  changes them.
- **It is in the trusted base**, so it must stay small enough to test completely.

### The monitor

1. **Separate principal.** It has its own identity, it holds almost no authority,
   and it has no write access to a task's workspace or fork, or to the authority
   domain.
2. **Sees the action, never the argument.** It gets the person's intents, the
   task's objective and limits, and the concrete action in a structured form. It
   does not get the harness's explanation, so a manipulated harness cannot
   persuade it.
3. **Reviews after the gate.** No action waits for the monitor.
4. **Can pause, can never allow.** It can pause a task and raise a surprise for
   the person. It cannot issue, widen, or restore anything. A wrong monitor is
   therefore no worse than no monitor, apart from a pause the person has to
   clear.
5. **Runs in the fleet,** so what it reviews does not leave. It needs to be
   consistent, not clever.

The monitor comes late in the roadmap on purpose. The gate has to stand on its own
first.

## The undo boundary

The undo boundary is the **default for how much the agent does without asking**.
It is not a safety property. It answers one question: "does this need the person?"
The answer is no only when all three hold:

1. the effect is **within the task's limits**;
2. the system holds a **tested way to undo it, for a stated window**;
3. it **stays inside the fleet**.

```mermaid
flowchart TD
    A[Proposed action] --> Q0{"Within the task's<br/>effect limits?"}
    Q0 -- no --> REF["Refused.<br/>The task may ask the person<br/>to widen its limits"]
    Q0 -- yes --> Q1{"Undoable for a stated window,<br/>and stays inside the fleet?"}
    Q1 -- "yes, both" --> IN["<b>Inside</b><br/>Do it + record Data history<br/>+ ledger entry"]
    Q1 -- no --> QC{"Does it send data to a party<br/>not cleared for everything<br/>the task holds?"}
    QC -- yes --> REFC["Refused.<br/>Only a release by the person<br/>lets a value pass"]
    QC -- no --> QS{"Covered by the person's answer<br/>at the start of the task?<br/>A fetch (GET, HEAD, the task's own<br/>model request), or an act whose<br/>exact content was shown then"}
    QS -- yes --> Q3
    QS -- no --> QM{"Is the task marked, or does the act<br/>depend on a marked result?"}
    QM -- no --> Q2{"Covered by a standing grant<br/>and a standing rule, or an Allowance,<br/>within its rate limit?"}
    QM -- yes --> QK{"A rule for that source, carried out by<br/>an adapter that knows the operation,<br/>within its limits?"}
    QK -- no --> ASK["Ask the person,<br/>in plain language, in trusted UI,<br/>with the exact content"]
    QK -- yes --> Q3
    Q2 -- no --> ASK
    Q2 -- yes --> Q3{"Acts on the<br/>person's behalf?"}
    Q3 -- no --> OK["Do it within the scope of what<br/>covered it + ledger entry"]
    Q3 -- yes --> OUT["<b>Outbox</b><br/>waits for a visible delay,<br/>can be cancelled until it leaves"]
```

The start of a task is not in this diagram. A task that needs to cross, or that
is proposed above the person's ceiling for tasks that do not cross, is
authorized by the person once before it runs. A marked task's own model request
is a fetch: the broker builds it, to a provider cleared for everything the task
holds.

**Inside the boundary.** Examples: build a Tool in a task's fork; run its checks
in rehearsal; move the person's files within the task's limits (the writes are
staged, committed together when the task finishes, and the Data layer keeps the
earlier versions for its undo window).

**A change to installed behaviour is not decided by the boundary.** A new Tool, a
changed Tool, a service, the theme, and keybindings all go live only by a
preview and the person's confirmation.

**Being inside the fleet does not make an effect undoable.** These stay in the
fleet and still cannot be taken back, so they are treated as crossing:

- a notification held past the moment it mattered;
- a delete after its undo window has passed;
- a command to a device on the home network (a lock, a printer, a robot).

A read cannot be taken back either. A read inside a task's limits does not ask.
Instead the reader takes on the label and mark of what it read, and those decide
where anything it writes may go.

**Crossing the boundary.** Further examples:

- data leaving the fleet, including calls to a cloud model or an external harness
  such as Grok Bot;
- acting as the person (sending a message or email);
- spending money;
- reading, creating, or using credentials;
- changing hardware or firmware.

Anything that crosses is allowed only by standing authority or the person's
answer. Standing authority means both a standing grant (access to the thing) and
a standing rule (this kind of act does not ask). A marked task has lost both for
every crossing: acts on the person's accounts, sending data out, and effects
inside the fleet that cannot be undone. So has any act that depends on a marked
result. The exception is a rule the person wrote for that source, carried out by
a broker adapter that knows the operation. The answer the person gave at the
start of a task still covers its fetches and the acts whose exact content was
shown then. It does not cover an act the task composed after it was marked.

An action **outside the task's effect limits** also needs the person, but no
standing rule covers it: the gate refuses it, and the task may ask the person to
widen its limits. A task's limits are a ceiling, not a default.

The earlier design treated any recorded effect inside the fleet as safe to do
automatically. That fails because undo only helps a person who notices, and
"notify only on surprise" removes the noticing. Three things now carry the weight
instead: effect limits bound what a task can touch, a preview shows a change
before it is live, and the gate decides every crossing.

### Effect limits

Every task and every grant states what it may affect:

| Limit | Example |
| --- | --- |
| Which objects | "the photos in ~/Pictures/2026", not "the disk" |
| How many | "at most 500 files" |
| For how long | "until 18:00 today" |
| At what cost | "up to $2 of cloud model use" |
| How often | "at most five emails an hour to new recipients" |

"Sort my photos" is a task on the photos. The gate refuses an action outside the
limits, whatever the task's reasons. A task cannot widen its own. The Agent
proposes each task's limits, so they bound a task's mistakes, not a steered
Agent. For a task that crosses nothing, the person's ceiling bounds what the
Agent may propose without an ask.

For native code, each limit holds only where Linux or the broker can enforce it.
"Which objects" is the view the sandbox is given. "How many" and "how often"
hold on broker operations as they happen, and on writes inside a view when they
are committed, not at each write. "For how long" ends by stopping the sandbox.
[ADR 0004](../adr/0004-capability-safe-tools.md) has the table.

### The outbox

Acts on the person's behalf (messages, posts, and purchases where the seller
allows it) do not leave at once. They wait in the broker's **outbox** for a delay
the person can see, and can be cancelled until they leave.

- The Agent keeps working while they wait. Nothing blocks.
- The person reviews what is waiting in batches, not one prompt at a time.
- The person sets the delay per standing rule.

The outbox pushes "can be undone" a little way past the edge of the fleet: an act
that would be final is cancellable for its delay.

When the system asks, it says what will happen, not which permission is needed:
"Send this email to Sam?", with the exact message shown.

## The person's answer

Every crossing the gate does not already allow ends in the person's answer. That
answer is only worth something if nothing else can produce it
([ADR 0014](../adr/0014-a-yes-comes-from-the-person.md)).

- **Only the shell delivers input.** No principal can synthesize keyboard,
  pointer, or touch input to a surface the shell draws. Agents operate Tools
  through the UI description, never through input events. On Linux, Base has to
  close every route by which one process produces or reads another's input.
  [ADR 0014](../adr/0014-a-yes-comes-from-the-person.md) lists the known
  ones. A route that is not closed is a hole.
- **Everyday answers use the confirm gesture.** The shell draws the ask in system
  chrome. The person answers with a secure key press that the shell handles
  itself and never delivers to a Tool or app. Confirming a preview, releasing a
  value, authorizing an ad hoc task, confirming an undo, and letting an outbox
  item go are everyday answers.
- **An ask cannot be swapped or flooded.** An ask ignores the gesture for a
  moment after it appears, so one ask cannot be swapped under a key press. Asks
  are rate-limited per task and across all the Agent's tasks together, and the
  ask that starts a task counts. So neither one task nor a stream of new tasks
  can flood the person until they stop reading.
- **A change to standing authority needs a hardware root.** A new standing
  grant, rule, or standing task, wider limits on any of them, a higher ceiling
  for tasks that do not cross, a higher clearance, a declassify, a refilled
  Allowance, and owner authority each need a passkey, a security key, or
  approval on the phone. The approval is a signature over the exact content
  shown. The limits of one ad hoc task are not standing authority: authorizing
  it takes the confirm gesture.
- **Voice never counts as confirmation.** A voice can ask for something. It
  cannot say yes to an ask, because a speaker can produce it. Words that arrive
  by voice are marked until the person makes the confirm gesture, so nothing
  crosses for them and nothing they say is kept as the person's own.
- **The trusted UI and the confirm gesture are part of Base.** They cannot be
  themed, rebound, or replaced by anything in the Environment.

## Notifications

**Notify only on surprise.** Work the person asked for that stays inside the
boundary just happens. The result *is* the notification. A preview is part of
the conversation the person started, not an interruption. The person is
interrupted only for:

- something a background task did that the person did not directly ask for;
- anything blocked or refused;
- anything the monitor paused;
- a request to cross the boundary.

Everything else is in the ledger. The Agent can summarize ordinary activity on
request ("what changed this week?"). **Entries that matter for security are
rendered by the OS in fixed words**, not summarized by the Agent they describe. A
compromised Agent must not be the one reporting on itself.

**Mode** is only a verbosity setting: how much the person is told about actions
inside the boundary. It never changes what is allowed. Changing it is recorded in
the ledger.

## Standing rules and standing grants

These are two different things.

- A **standing grant** is long-lived access to a thing, held by a Tool or a
  standing task, never by the Agent. For example: "Morning Pages may read and write `~/Writing`", or "Morning Pages may
  reach cloud.example.org".
- A **standing rule** is the person's pre-approval, with a rate limit, for a
  narrow kind of act that crosses the boundary, so that act doesn't ask again.
  For example: "Morning Pages may sync to your Nextcloud at cloud.example.org, at
  most once every ten minutes."

An act that crosses needs both: the grant gives the access, and the rule lets
the act happen without asking. Both bind to a Tool or to a **standing task**,
never to the Agent ([ADR 0011](../adr/0011-task-principals.md)).

**A grant binds to three things:** the Tool's identity in the State, the exact
needs the person saw when they granted, and a clean build. A rebuild that keeps
all three keeps the grant. A rebuild that changes the needs, or that read open
input, does not: the person is asked again, and until they answer the earlier
build stays live. The person cannot review code. When a rebuild asks again, they
see the needs and the check results, and the grant's effect limits bound what
the code can do.

Both are recorded in the **authority domain**, with their effect limits. They are
not in the Crowbar State. The State says what each Tool *needs*; the authority
domain records what the person *granted*. Three consequences follow
([ADR 0012](../adr/0012-authority-outside-the-state.md)):

- **Undoing software never restores a revoked grant.** Rolling back to a
  generation whose Tools need a grant that has since been revoked leaves those
  Tools without it.
- **Authority never transfers.** A shared Tool or OS definition carries no
  grants and no rules. The person who receives it grants fresh.
- **Changing authority always asks the person**, in trusted UI, with a hardware
  root. No task and no Agent can write to the authority domain.

When the person approves the same kind of action repeatedly, the OS offers a
standing rule in fixed words:

> You've approved "Morning Pages may sync to your Nextcloud at cloud.example.org"
> 3 times. Always allow it? It will still stop if it tries to sync anywhere else,
> or more than once every ten minutes.

An accepted rule is narrow, names its scope and rate limit, and can have an
expiry. Rules and grants can be revoked in one step from the ledger.

## Money

Spending money crosses the undo boundary. Two mechanisms cover it.

### Budgets

A budget is a **cap the person sets in the authority domain**. For example: "Claude, up to $20 a
month." What data may go to that provider is its clearance, not the budget.
Reaching the cap is a boundary event, so it asks
the person. A task's own budget is part of its effect limits and can only be
smaller. CPU and memory used inside the fleet are only resource limits and never
ask.

### Allowances

An **Allowance** is a standing amount of money that the person sets aside, in a
separately funded account whose limit the card issuer or wallet enforces. It is
standing authority like any other. A Tool, a standing task, or a task the
person authorized can spend inside it without asking. A marked task spends from
it only by the person's answer or through an adapter rule, and most purchases
read a seller's page first. Refilling it always asks.
For example: "Here is $200. Use it for what you need."

This is not the same as `allowance` in OpenAI and Stripe's Agentic Commerce
Protocol, where it is a one-time limit on a single checkout. Ours is a standing,
funded pot. Prior art and sources are in
[agent payments prior art](../research/agent-payments-prior-art.md).

Design rules:

- **The issuer enforces the cap, not our software.** Card issuers and wallet
  operators already check limits when a payment is authorized. The cap holds even
  if CrowbarOS is compromised.
- **The bound is as strong as the account behind it.** A card's authorization cap
  is not an exact settlement limit: the settled amount can differ from the
  authorized one. So the Allowance is funded separately, and what the person put
  in that account is what the bound rests on. A capped card that debits the
  person's main bank account is weaker. Visa's rules make the cardholder
  responsible for an agent's purchases "as if the Cardholder initiated" them, and
  no network has a dispute reason for agent purchases, so "we can dispute it" is
  not a safety net.
- **No task ever sees the card number.** The broker holds the card details in
  the vault and fills them in, as with any other secret. This is stricter than most
  products today, which hand the agent the card number.
- **Refilling is out of every agent's reach.** The broker never exposes "raise
  limit", "create card", or "fund". Some issuer tool sets include those calls;
  the broker must not pass them through.
- **Every purchase is a ledger entry**, with the intent and the task it served.
  Where the seller allows it, a purchase waits in the outbox like any other act
  on the person's behalf.

Candidates to evaluate as the first backing, in order:

1. **Privacy.com:** an issuer-capped card that an individual can get through an
   API today. The broker holds the API key and sets a lifetime limit equal to the
   Allowance. For the bound to hold, fund it from a dedicated account.
2. **Link's agent wallet:** tokens for merchants that accept them, with its own
   server-side caps.
3. **Coinbase Agentic Wallet with x402:** for paid APIs.

None of these is chosen yet. Each needs a closer look at what it exposes and
where its limits are enforced.

## Owner authority

The owner can always take full authority over their machine: a root shell, a Base
change, or turning off any guard. Taking it is deliberate, like unlocking, and it
is recorded in the ledger. It can never be held by an agent or a task. The Agent
can ask the person to use owner authority, but it cannot use it itself.

## The ledger

Every intent edit, task start and end, gate decision, monitor pause, capability
grant, release, egress, outbox entry, purchase, preview confirmation, promote,
undo, rollback, and use of owner
authority is one ledger entry: which principal asked, the intent and the task it
served, the concrete action, whether it crossed the boundary and under which
grant or rule, the gate's decision and signature, and the resulting handle or
generation.

```mermaid
sequenceDiagram
    participant P as Person
    participant H as Agent (harness)
    participant S as Shell (trusted UI)
    participant T as Task
    participant E as Engine
    participant G as Gate
    participant B as Broker
    participant L as Ledger
    participant M as Monitor
    P->>H: "Make the dock hide unless I push the mouse down"
    H->>H: Shaping: propose intent edit + checks
    H->>S: show checks, worded by fixed templates
    P->>S: confirms (confirm gesture)
    H->>T: start build task (objective, effect limits, budget, expiry)
    T->>E: fork the Environment
    T->>T: build in the fork, reading only clean inputs
    E->>E: Base runner: run the checks in rehearsal, sign result for this build
    E->>S: preview of the forked Tool
    P->>S: confirms (confirm gesture)
    S->>G: promote this change set (this build, started from these builds)
    G->>G: needs unchanged, build clean, Tools not changed since
    G->>B: allowed, signed
    B->>E: handle: promote (one use)
    E->>E: new Environment generation
    E->>L: promote ok
    G->>L: allowed, intent + task + build
    L-->>M: the action, after the gate
    M->>M: fits the intent and the task: no pause
    H->>P: "Done. Undo?"
```

From the ledger, a person can **revoke** a grant or **undo** a change in one step.
Both are themselves logged. Undo reverts one change set and keeps later changes
to other Tools. It is a change like any other: the runner re-runs the checks of
every Tool it could affect, and the person confirms it. Rolling back the whole Environment to an earlier generation is a
recovery action.

## Failure modes this design must handle

- **Prompt injection into a task.** A task that has read open untrusted input is
  marked and has lost its standing grants and rules to cross the boundary, so the
  injected action ("upload ~/Documents to an unknown host" after "sort my
  photos") has nothing to cross with. It is also outside the task's effect
  limits, so the gate refuses it. The gate never sees the injected argument, only
  the action. What remains: a standing rule the person wrote for that source and
  operation, up to its limits.
- **An injection carried in a result.** A marked task's summary does not reach
  the Agent or a build task. Nothing crosses freely: the interpreter keeps the
  mark on every result, including handles and bounded results. An injection can
  steer a bounded choice, but an act that depends on it is marked, so it cannot
  spend a standing rule, in this request or a later one. The Agent does not read
  a marked result, and a context that does is marked for as long as it lives.
  What remains: a steered choice can still select among outcomes the person is
  asked about, and a person can release text they did not read.
- **A private value returned a few bits at a time**, as choices, as numbers, as
  the sizes of results, or as which steps fail. Bounded results keep their
  label, and what can be observed about a result is a result. A private result
  reaches the Agent only by a release or inside a budget counted across all
  tasks, zero by default. What remains: whatever budget the person sets.
- **A task the person authorized composes different content after reading an
  injection.** The answer at the start covers fetches and the acts whose exact
  content was shown. A crossing act composed after the mark asks again.
- **A task that crosses nothing and touches everything.** The person's ceiling
  on tasks that do not cross: above it the task is authorized at its start.
  Writes are staged, and the Data history can undo them for its window.
- **A research task carries the Agent's personal context out in its search.**
  The ask that starts the task shows the exact text that may leave and where it
  may go.
- **A build searches a real input through replay verdicts.** A replay reaches a
  build only as pass or fail, capped per re-derive.
- **An injection in a file name, a mail subject, an error message, or a paste.**
  Every string the person did not type and a template did not produce is data
  with a mark. A paste arrives as a handle. What remains: what the person types
  themselves after reading something hostile.
- **A video or a speaker tells the Agent to do something, or to remember
  something.** Words that arrive by voice are marked until the person makes the
  confirm gesture. Voice never answers an ask.
- **An injection that writes code into a Tool with grants.** Build tasks read
  only admitted input and synthetic results. A grant binds to a clean build with
  unchanged needs, so a rebuild that read open input asks the person again, and
  the mark persists in the Tool's source. What remains: hostile text inside an
  admitted set.
- **A steered Agent uses the person's standing authority.** The Agent holds
  none. It can start a standing task, which has a fixed objective and limits, or
  propose an ad hoc task, which the person authorizes at its start. What remains:
  everything the standing tasks allow, as often as their rate limits allow.
- **An agent, a Tool, or a harness answering its own ask.** Only the shell
  delivers input, Base closes the Linux routes around that, asks take the
  confirm gesture, and changes to standing authority need a hardware root. What
  remains: a compromised Base, and an input route Base did not close.
- **A task floods the person with asks, or swaps an ask under a key press.** Asks
  are rate-limited per task and across all the Agent's tasks, and an ask ignores
  the gesture for a moment after it appears. What remains: a person trained to say yes by honest asks.
- **An unwanted action that is within the limits.** Effect limits bound how much
  it can touch. Changes to installed behaviour were previewed and confirmed
  before they went live. The monitor may pause the task. What is left is an
  effect inside the boundary, which can be undone for its window.
- **Monitor mistakes.** If it misses something, the outcome is the same as having
  no monitor, because it never allowed anything. If it pauses wrongly, the person
  clears the pause.
- **Monitor unavailable.** Nothing waits for it. The gate runs unchanged.
- **A wrong rule in the gate, or a rule the person granted too widely.** The gate
  is on the trust path, so this is the failure that matters most. Effect limits
  and rate limits bound each grant, the outbox delays acts on the person's behalf,
  and the Allowance is bounded by the account behind it even if our software
  fails.
- **Many small steps that add up to an egress**, for example copying data step by
  step into a folder that some other process uploads. A compartment that has read
  private data is at that label from then on, and so is everything it emits,
  including the files in that folder. A principal that reads them enters the
  label and takes on any mark, and the broker checks the label at egress. For software the broker
  cannot see inside (a native app, a browser), the check is per principal and per
  destination, not per value.
- **An effect inside the fleet that cannot be undone.** It is treated as crossing:
  it needs a standing grant and a standing rule, or the person's answer.
- **Two tasks changing the Environment at once.** Each has its own fork. A promote
  carries a change set: the Tools it changes, and the build of each that the task
  started from. Two tasks on unrelated Tools both promote, and the runner re-runs
  the checks of every Tool the combined result could affect. The person is asked
  only when two change sets touch the same Tool or a check fails.
- **The build that goes live is not the build that was tested or previewed.** The
  runner signs the result for one content hash, the preview is bound to that
  hash, and a promote of anything else is refused.
- **Rollback that would restore authority.** It cannot. Authority is outside the
  State and its generations.
- **A malicious or broken external harness.** It can only propose. It cannot
  assert that the person confirmed, because confirmation happens in trusted UI,
  and it cannot produce input.
  Environment safe mode can always start the default harness instead. See
  [state and apply engine](state-and-apply-engine.md).

## Open questions

1. **Monitor model.** Which small model is good enough, how do we evaluate it (a
   fixed test set of intent, task, and action triples, including adversarial
   ones), and how small can it be?
2. **Monitor with no model in the fleet.** Is there simply no monitor, or may a
   cloud model review actions with the private values removed?
3. **Deriving the boundary.** How does the gate know that an effect has a tested
   undo and what its window is? Does each broker interface declare it, and who
   tests the declaration?
4. **Showing effect limits.** When an ad hoc task is authorized at its start,
   how does the person see its limits at a glance in fixed words, and what are
   the units for "how many" across different kinds of objects?
5. **Signatures.** What key material does the gate hold, and how is it protected
   from tasks (a separate user ID, a TPM-bound key)? What does the broker check?
6. **Outbox rules.** What is the default delay? May a rule set no delay? Does an
   act the person just answered "yes" to still wait? When the person lets a
   waiting act go, how long does that answer last, and does it cover the next act
   of the same kind? Does pausing a task hold its outbox entries? What happens to
   purchases a seller will not let us delay?
7. **Too much asking.** Previews, releases, task authorizations, and asks all
   end at the person. How many a day before they stop reading them?
   [Milestone 2](../milestones/milestone-2.md) counts them over a set of ordinary tasks.
8. **Long content in an ask.** How does the shell show exact content that is too
   long for one screen, without training the person to skip it?
9. **Agents and Base.** Base changes need owner authority. May a task prepare an
   exact Base change that the owner approves and a narrow service applies, or
   must the person run the privileged step themselves?
10. **A stop switch.** One deterministic action that stops every task, revokes
    every handle, and shows what was running.
11. **Harness protocol scope.** Which MCP tools and resources does the protocol
    expose, and how does it version them?
12. **Allowance backing.** Which issuer or wallet backs the first Allowance, may
    an Allowance purchase start a recurring charge, and how does the broker fill
    card forms safely on sites that block agents?
13. **Notification verbosity.** What are the levels, where is the mode setting
    stored, and what does "surprise" mean for a background task that runs for
    days?
14. **The ceiling.** What is the default ceiling for tasks that do not cross,
    and in what units does the person set it?
15. **The budget for private results.** In what units does the person set it, and
    how do they see what it has spent?
