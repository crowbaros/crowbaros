# CrowbarOS glossary

An operating system that ships a small, stable base which each person turns into their own OS by stating intents to an agent. Changes to a person's tools are checked in a private copy before they go live, and can be kept, undone, and shared.

## Describing the system

**Intent**:
A durable statement of what a person wants from their OS: their words plus the checks they confirmed. Together with the pinned build, intents are the source the rest of the system is derived from.
_Avoid_: Prompt, command, request

**Check**:
A scenario the person confirmed as part of an intent: events, and the effects expected from them, in a fixed format. It is shown in plain language that fixed templates produce from the scenario. It is frozen: only an intent edit can change it.
_Avoid_: Acceptance criterion, example, test

**Rehearsal**:
Running a Tool against simulated time, simulated events, and recorded inputs, with no real effects. Checks run in rehearsal.
_Avoid_: Simulation harness, dry run

**Runner**:
The part of Base that runs checks in rehearsal and signs the result for one exact build.
_Avoid_: Test harness

**Pinned build**:
The exact build of a Tool or of Base that the person accepted, identified by a content hash or a package repository snapshot.
_Avoid_: Artifact, binary

**Re-derive**:
Building again from unchanged intents, for example with a better model. It is accepted only when the frozen checks pass and a replay of recorded inputs matches.
_Avoid_: Regenerate, rebuild

**Shaping**:
The conversation in which the person's agent helps them work out what they want and turns it into an intent with checks. The OS sets the rules it must follow (checks exist and the person confirms them in trusted UI); the harness decides how the conversation goes.
_Avoid_: Prompting, onboarding, interview

**Crowbar State**:
The lockfile for a person's OS: it pins the intents, their frozen checks, and the exact builds. It holds no authority and no secrets.
_Avoid_: Config, OS definition

**OS definition**:
What a person carries to a new machine or shares: their intents and checks with the Crowbar State that pins the builds.
_Avoid_: Crowbar State, profile, dotfiles

**Generation**:
One numbered, recorded build of a layer from the Crowbar State, which the person can return to.
_Avoid_: Snapshot, version, revision

**Tool**:
A piece of generated software derived from intents. It may have a UI, a schedule, both, or neither.
_Avoid_: App, component, program

**Layer**:
One of three parts of a system that change and roll back on their own: Base, Environment, or Data.

**Base**:
The layer with the kernel, drivers, and core services. It changes rarely, and a change takes effect at boot.
_Avoid_: Root, system image

**Environment**:
The layer with the person's tools, services, desktop, and automations. Most agent work happens here, and a change takes effect when a fork is promoted.
_Avoid_: Userland, session

**Data**:
The layer with the person's own things. Rolling back Base or Environment never rolls it back.
_Avoid_: Home, user files

**Fork**:
A private copy of the Environment in which one task builds a change to installed behaviour.
_Avoid_: Branch, staging, sandbox

**Preview**:
The forked Tool running in the fork, shown to the person before they confirm. It has no effects outside the fork.
_Avoid_: Provisional edit, draft

**Change set**:
The Tools one promote changes, with the build of each that the task started from.
_Avoid_: Diff, patch, commit

**Promote**:
Making a confirmed change set live, as a new Environment generation. It is the only way the Environment changes, and it makes live exactly what was previewed.
_Avoid_: Deploy, merge, live apply

**Undo**:
Reverting one change set, so its Tools return to their earlier builds and later changes to other Tools stay.
_Avoid_: Revert, roll back (for one change)

**Roll back**:
Returning a whole layer to an earlier generation. For the Environment it is a recovery action.
_Avoid_: Undo, reset, restore

**Workspace**:
A task's private scratch area and its view of the Data it was given. Its writes to Data are staged there and committed together when the task finishes.
_Avoid_: Fork, sandbox, temp directory

**Catalog**:
CrowbarOS's record of the installed software a person can install, each entry pinned to its source by hash, with the needs the person sees. A person can add private entries of their own.
_Avoid_: App store, repository, package list

**Workbench**:
A changeable Linux system for one project's dev tools, made from a profile the person approved once. Unlike a workspace, its writes go live, and what is installed inside it is not pinned.
_Avoid_: Dev box, dev container, workspace

## Authority and safety

**Principal**:
Anything that can hold authority: a person, an agent, a task, a Tool, installed software, or a workbench.

**Agent**:
The person's own principal that does Shaping, edits intents with them, and starts tasks. It is a role the OS defines, whatever harness fills it.
_Avoid_: Assistant, bot, AI

**Conversation**:
One exchange between the person and the Agent, run as a compartment of its own. Its label and mark change only when the person points it at something or puts something into it.
_Avoid_: Session, thread, chat, context

**Task**:
One piece of agent work run as its own principal, with a stated objective, effect limits, a budget, and an expiry.
_Avoid_: Job, session, run

**Standing task**:
A recurring task the person defined by name, with a fixed objective and limits. The Agent can start it and cannot change it.
_Avoid_: Automation, cron job, routine

**Build task**:
A task that writes or changes a Tool's code. It reads only the intent, the checks, the Tool's source, and admitted input, and has no web access.
_Avoid_: Coding agent, compile

**Lookup**:
A task that answers a question from the open web. It holds only the words the person typed and what it found, so it is public and marked.
_Avoid_: Research task, search, web task

**Plan**:
A small program the Agent writes whose variables are handles. A deterministic interpreter runs it, moves the actual values, and tracks the label and mark of every result. The Agent reads a value only where its label and mark allow.
_Avoid_: Script, workflow, prompt chain

**Bounded result**:
A choice from a list, or a number in a range, both fixed before the task read anything. It keeps the label and mark of what it was computed from.
_Avoid_: Typed result, safe value

**Broker**:
The part of Base through which a compartment reaches everything outside itself: Data, the network, devices, and time. It makes every outbound connection and holds the vault.
_Avoid_: Proxy, daemon

**Adapter**:
The part of the broker that speaks one protocol or one service and knows its operations, so a standing rule can name an operation and a credential never leaves the vault.
_Avoid_: Plugin, connector, integration

**Harness**:
Whatever fills the agent role: the loop, tools, and conversation around a model. CrowbarOS ships a default harness, and an external one (such as Grok Bot) can replace it. A harness can only propose; it never holds authority.
_Avoid_: Engine, backend, bot

**Model**:
What a harness thinks with, whether a cloud model or an open-weight model on the person's own hardware.
_Avoid_: Engine, AI

**Agent memory**:
What the Agent has learned about the person. It is the person's Data, not the harness's.
_Avoid_: Context, history

**Owner**:
The person a machine belongs to. The owner can always take full authority over it.
_Avoid_: User, admin, root

**Owner authority**:
Full, unrestricted control of the machine, which the owner takes deliberately. It is recorded in the ledger and can never be held by an agent.
_Avoid_: Root, sudo, admin mode

**Handle**:
An unforgeable, scoped, expiring right for one principal to use one resource or one value. It names the thing without revealing it.
_Avoid_: Permission, token, capability (when the specific object is meant)

**Delegable**:
Said of a handle its holder may pass on as a copy. The broker decides it at issuance. A copy is never delegable, and nor is a handle the Agent holds.
_Avoid_: Transferable, shareable

**Copy**:
A handle a holder passed on from a delegable handle, its **original**. It is narrowed to the original's rights and effect limits or fewer, its uses count against the original's limits, it carries the label and mark of the compartment that passed it on, and it ends when the original ends.
_Avoid_: Sub-handle, child handle, delegated token

**Effect limits**:
What a task or a grant may affect: which objects, how many, for how long, at what cost, and how often.
_Avoid_: Quota, envelope, scope (on its own)

**Gate**:
The deterministic rules engine that checks each broker operation, and each commit of staged writes, against handles, effect limits, standing grants, and standing rules. It is the only thing that allows an action.
_Avoid_: Approver, policy engine

**Dry run**:
Asking the gate what it would decide for an operation without carrying it out. The answer is the decision the real operation would get, and it changes nothing.
_Avoid_: Preflight, simulation, what-if

**Idempotency key**:
A key a task attaches to an operation so that the operation is decided and carried out once. A repeat with the same key gets the first decision again. The same key on a different operation is refused.
_Avoid_: Request ID, nonce

**Monitor**:
A model, isolated from the Agent, that reviews actions against the person's intents and the task. It can pause a task and raise a surprise; it can never allow.
_Avoid_: Approver, reviewer, guard

**Authority**:
The record of what the person has granted: standing grants, standing rules, effect limits, Allowances, and clearances. It has its own history and is never rolled back with software.
_Avoid_: Permissions, policy, config

**Fleet**:
The set of machines a person owns and has deliberately joined together, linked by encrypted connections. Each machine has its own clearance.
_Avoid_: Estate, cluster, devices

**Lead machine**:
The one machine in a fleet that holds the authority domain and puts promotes in order. The others work on leases from it that expire.
_Avoid_: Primary, server, master

**Clearance**:
The highest label a machine in the fleet, or an outside party such as a model provider or a web site, may hold. For private data it names the categories. A model provider's default is personal; a web site's is public.
_Avoid_: Trust level, tier

**Undo boundary**:
The default line for what the agent does without asking. An effect within the task's limits is inside it when the system can undo it for a stated window and it stays inside the fleet; otherwise it crosses, and needs standing authority (a standing grant for the access and a standing rule for the act) or the person's answer. An effect outside the task's limits is refused.
_Avoid_: Risk tier, safety boundary

**Outbox**:
Where acts sent under a standing rule wait for a visible delay before they leave the fleet. They can be cancelled until they leave. An act the person answered leaves at once.
_Avoid_: Queue, drafts

**Standing grant**:
Long-lived access to a thing, held by a Tool, a standing task, a workbench, or installed software, never by the Agent, and recorded in the authority domain. For a Tool it binds to the Tool's identity, the needs the person saw, and a clean build. For a workbench it binds to its identity and the needs of its profile, and for installed software to its identity and the needs its approval showed.
_Avoid_: Standing rule, permanent permission

**Standing rule**:
A person's pre-approval, with a rate limit, for a narrow kind of act that crosses the undo boundary, so that act doesn't ask again. It is held by a Tool, a standing task, a workbench, or installed software, never by the Agent. An act that crosses needs both the grant (access) and the rule (no ask).
_Avoid_: Standing grant, allowlist

**Refusal**:
The gate's no to an operation. It names the rule and the limit that applied, never a value, and nothing happens.
_Avoid_: Denial, permission denied, block, error

**Ask**:
A question the broker puts to the person in trusted UI, with an ID and exact content. It waits for the person, never takes the keyboard from their work, and lasts as long as its task. The shell draws it, and a plain answer, the confirm gesture, or a signature answers it, by what is at stake.
_Avoid_: Prompt, dialog, popup, permission request

**Plain answer**:
Any ordinary input in trusted UI. It answers an ask about something that stays inside the undo boundary, such as keeping a preview.
_Avoid_: Click, OK button, tap

**Signature**:
An answer made with a passkey, a security key, or the phone, over the exact content shown. It answers an ask for authority that lasts.
_Avoid_: Approval, password, 2FA

**Friction bar**:
The most asks and refusals a person may meet in one task and in one day. A rule that makes ordinary work miss it is redesigned.
_Avoid_: Livability threshold, prompt budget

**Armed**:
The state of an ask that is visible, on top, holding the keyboard, and steady for long enough that the confirm gesture can answer it.
_Avoid_: Active, ready

**Confirm gesture**:
The secure key press, taken by the compositor and never delivered to any client, with which the person answers an ask for something that crosses the undo boundary, or for authority that lasts only for this run. With no ask showing it locks the session, and on the lock screen it begins unlock.
_Avoid_: Click, OK button, voice confirmation

**Compositor**:
The CrowbarOS program that owns the screen and the input devices, written in Rust on Smithay. It is part of Base. It delivers input only as the shell directs, keeps the confirm gesture from every Tool, and offers no Tool a way to produce input or read the screen.
_Avoid_: Window manager, Hyprland

**Trusted UI**:
Everything on screen that Base draws and no client can imitate or cover: what the shell draws, and the frames the compositor draws.
_Avoid_: System chrome, system UI

**Shell**:
The Base program that draws the trusted UI other than frames: the bar, the dock, the launcher, an ask's content, the lock screen, and a Tool's UI from its description.
_Avoid_: Desktop environment, panel

**Frame**:
What the compositor draws around a window or an ask, which no client can draw or cover.
_Avoid_: Marking, mark, decoration, border

**Powerbox**:
Trusted UI where the person's choice of a thing is itself the grant of a handle to that thing.
_Avoid_: Permission prompt, picker

**Label**:
How private a piece of data is: public, personal, or private, in that order. Private data also carries a category, such as messages, health, or finance. A label comes from where the data came from.
_Avoid_: Tag, classification

**Category**:
What private data carries to say what kind of private it is, such as messages, health, finance, or the one site it was fetched from under a login. A value can carry several. A party is cleared for private data only in the categories its clearance names.
_Avoid_: Tag, class, label (for the category alone)

**Origin table**:
The deterministic rules that give every object its label and mark from where it came from: typed text, a paste, a file, mail, a web page, admitted input, or a stored result. An origin no rule covers is private and marked. No model takes part.
_Avoid_: Classifier, labeling policy

**Compartment**:
One execution context: a conversation, a task's model context and workspace, or one running copy of a Tool or of installed software. Each start makes a new compartment, so one principal can have several at once, such as a live Tool and its Preview. Once it reads a value it is at that value's label and carries its mark, and so is everything it emits.
_Avoid_: Session, quarantine, instance

**Sandbox**:
The Linux confinement around one compartment's processes. The compartment is the thing confined, and the sandbox is what confines it.
_Avoid_: Container, jail

**Wide compartment**:
A compartment the owner gave everything they have: all their Data, the network, and the devices they name. It never holds what the trusted parts of Base hold, so it cannot answer its own ask.
_Avoid_: YOLO mode, full access, unconfined, developer mode

**Warden**:
The part of Base that starts and stops every compartment, on the broker's orders alone. Nothing else starts a principal's process.
_Avoid_: Launcher, spawner, supervisor

**Untrusted input**:
Content written by someone other than the person or their OS. It is _admitted_ when it comes from a pinned set the person admitted in advance (the Base package snapshot, the dependency set for build tasks), and _open_ otherwise (web pages, mail, shared intents, files from outside). What the person pastes keeps the mark of where it was copied from. It includes short strings: a file name, a mail subject, error text from a server.
_Avoid_: External data, tainted input

**Mark**:
The record that a value was produced by a context that had read open untrusted input. A marked task keeps its model and loses its standing authority to cross the undo boundary, and an act that depends on a marked result is marked. Nothing a model does removes a mark, it persists in storage, and a context that has read a marked value stays marked for as long as it lives.
_Avoid_: Taint, flag

**Release**:
The person letting one exact value pass to one named destination despite its label, by answering an ask that shows it or by moving it there with their own hand. The value keeps its mark.
_Avoid_: Declassify, approve, share

**Declassify**:
Making the label on stored data less strict. Only the person can do it.
_Avoid_: Release, unlabel, relabel

**Task browser context**:
A fresh browser context for one task, holding none of the person's sessions. On a logged-in site it reaches only that site, after one answer from the person. The person's own browser profile is never driven by an agent.
_Avoid_: Agent browser, headless browser

**Allowance**:
A standing amount of money that the person sets aside for their agent, in a separately funded account whose limit the card issuer or wallet enforces. A Tool, a standing task, or a task the person authorized can spend inside it without asking, while refilling it always asks. It is not a single checkout's spending limit.
_Avoid_: Budget (for money), wallet, spending limit

**Vault**:
Where secret values live, held by the broker and referred to by name. Agents, harnesses, models, and tools never see the values.
_Avoid_: Keychain, secret store

**Ledger**:
The append-only history of every apply, promote, rollback, grant, and gate decision.
_Avoid_: Log, audit trail
