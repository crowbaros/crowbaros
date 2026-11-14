# CrowbarOS (agent-first)

An operating system that ships a small, stable base which each person turns into their own OS by stating intents to an agent. Every change can be kept, undone, and shared.

## Describing the system

**Intent**:
A durable statement of what a person wants from their OS: their words plus the checks they confirmed. Intents are the source the rest of the system is derived from, and they change as the person's needs change.
_Avoid_: Prompt, command, request

**Check**:
A concrete, plain-language example the person confirmed as part of an intent, which any build from that intent must satisfy.
_Avoid_: Test, acceptance criterion

**Re-derive**:
Building the Crowbar State again from unchanged intents, for example with a better model, offered to the person as a reviewable diff.
_Avoid_: Regenerate, rebuild

**Shaping**:
The conversation in which the person's agent helps them work out what they want and turns it into an intent with checks. The OS sets the rules it must follow (checks exist and the person confirms them); the harness decides how the conversation goes. Its depth scales with how unclear and how far-reaching the request is.
_Avoid_: Prompting, onboarding, interview

**Crowbar State**:
The exact, reproducible description of a system, compiled from intents. It is a lockfile for the OS.
_Avoid_: Config, OS definition

**OS definition**:
What a person carries to a new machine or shares: their intents, optionally with the Crowbar State that pins them exactly.
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
The layer with the person's tools, services, desktop, and automations. Most agent work happens here, and a change takes effect live.
_Avoid_: Userland, session

**Data**:
The layer with the person's own things. Rolling back Base or Environment never rolls it back.
_Avoid_: Home, user files

## Authority and safety

**Principal**:
Anything that can hold authority: a person, an agent, or a piece of generated software.

**Agent**:
The person's own principal that does Shaping and edits intents with them. It is a role the OS defines, and it holds the authority, whatever harness fills it.
_Avoid_: Assistant, bot, AI

**Harness**:
Whatever fills the agent role: the loop, tools, memory, and conversation around a model. CrowbarOS ships a default harness, and an external one (such as Grok Bot) can replace it. A harness can only propose; it never holds authority.
_Avoid_: Engine, backend, bot

**Model**:
What a harness thinks with, whether an open-weight model on the person's own hardware or a cloud model.
_Avoid_: Engine, AI

**Owner**:
The person a machine belongs to. The owner can always take full authority over it.
_Avoid_: User, admin, root

**Owner authority**:
Full, unrestricted control of the machine, which the owner takes deliberately. It is recorded in the ledger and can never be held by an agent.
_Avoid_: Root, sudo, admin mode

**Handle**:
An unforgeable, scoped, expiring right for one principal to use one resource.
_Avoid_: Permission, token, capability (when the specific object is meant)

**Fleet**:
The set of machines a person owns and has deliberately joined together, linked by encrypted connections. Everything inside the fleet counts as local.
_Avoid_: Estate, cluster, devices

**Undo boundary**:
The line between effects the system can fully undo (inside the fleet, recorded) and effects it cannot (data leaving the fleet, acting as the person, money, credentials, hardware). Crossing it is always gated.
_Avoid_: Risk tier (as the primary axis)

**Approver**:
An isolated principal that checks whether a concrete action matches the person's intent. It is an alignment check, not the security boundary.
_Avoid_: Reviewer, guard

**Standing grant**:
A long-lived handle that a principal holds and that is recorded in the Crowbar State.
_Avoid_: Standing rule, permanent permission

**Standing rule**:
A person's pre-approval for a narrow class of actions, so those actions don't ask again.
_Avoid_: Standing grant, allowlist

**Powerbox**:
Trusted system UI where the person's choice of a thing is itself the grant of a handle to that thing.
_Avoid_: Permission prompt, picker

**Label**:
A mark on a piece of data saying how private it is. It comes from where the data came from, and anything derived from that data carries it too.
_Avoid_: Tag, classification

**Declassify**:
Making a label less strict. Only the person can do it, because it lets data cross the undo boundary.
_Avoid_: Unlabel, relabel

**Allowance**:
A standing amount of money that the person sets aside for their agent, in a separately funded account whose limit the card issuer or wallet enforces. The agent can spend inside it without asking, while refilling it always asks. It is not a single checkout's spending limit.
_Avoid_: Budget (for money), wallet, spending limit

**Vault**:
Where secret values live, held by the broker and referred to by name. Agents, harnesses, models, and tools never see the values.
_Avoid_: Keychain, secret store

**Ledger**:
The append-only history of every apply, rollback, grant, and approval decision.
_Avoid_: Log, audit trail
