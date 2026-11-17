# Security is deterministic; a gate allows, a monitor can only pause

A model can be wrong, and an injection can hide inside the action itself. So nothing that allows an action is a model. A deterministic **gate** checks every broker operation, and every commit of staged writes, against handles, **effect limits** (which objects, how many, for how long, at what cost, how often), standing grants, and standing rules, and signs what it allows. A **monitor**, a model isolated from the Agent, reviews actions against the person's intents and the task. It can pause a task and raise a surprise, but it can never allow anything, so the system is sound with no model at all.

The **undo boundary** is the default for how much the agent does without asking, not a safety property. An effect needs no answer from the person only when it is within the task's limits, the system holds a tested way to undo it for a stated window, and it stays inside the fleet. Everything else needs standing authority (a standing grant for the access and a standing rule for the act) or the person's answer. Acts sent on the person's behalf under a standing rule (messages, posts, purchases where the seller allows it) wait in an **outbox** for a visible delay and can be cancelled until they leave. An act the person answered leaves at once: the delay exists so the person can catch what they never saw, and they saw this one.

**Security and low friction both have to pass.** Every rule here is also held to the friction bar, and where no design passes both, the Agent hands the work back to the person ([ADR 0024](0024-the-friction-bar-ranks-with-the-attack-list.md)).

The gate does not see what a native process does inside the view it was given; the kernel enforces that view (ADR 0004). Whether an answer really came from the person is decided by ADR 0014.

Revised after council rounds 1, 2, 3, and 5. The earliest version had one "approver" that was both the rules and a model, signed every handle, and treated any recorded effect inside the fleet as safe to do automatically. Until round 5 every act on the person's behalf waited in the outbox, including one the person had just answered.

## Considered Options

- **Undoable means safe to do automatically (the earlier decision).** Rejected: undo only helps a person who notices, and effects inside the fleet can still be impossible to take back (a notification held too long, private data already read, a delete after its window).
- **One approver that both decides and judges alignment.** Rejected: it made a model an availability dependency for every handle, which contradicts "no model on the trust path".
- **Three risk tiers and three modes.** Rejected earlier; "mode" controls only how much the person is told. The three answers to an ask in [ADR 0026](0026-asks-wait-and-take-one-of-three-answers.md) are not risk tiers: which one an ask takes follows from two facts the gate already has, whether the effect crosses the undo boundary and whether the authority lasts.

## Consequences

- Every grant and every task must state its effect limits, so the gate has something to enforce. Limits are a ceiling: the gate refuses anything outside them, and only the person can widen them.
- Calling a cloud model, or using an external harness in someone else's cloud, is egress and crosses the boundary. A model provider is a party with a clearance per label (ADR 0008). A model request does not ask while the provider is cleared for everything the compartment holds and the model budget is not spent: the clearance and the budget are the standing authority for it (ADR 0011).
- A standing grant gives access to a thing. A standing rule lets a kind of act happen without asking. An act that crosses needs both.
- Standing rules carry rate limits. Standing grants and rules bind to a Tool, a standing task, a workbench, or installed software, never to the Agent (ADR 0011).
- The gate must explain itself to agents. The harness protocol requires a "would this be allowed?" dry run with no side effects, refusals that name the rule and the limit, idempotency keys so a resumed task knows what already happened, and a way to read a handle's label and mark without reading its value. A dry run must not itself reveal a value.
- The outbox adds a delay to acting as the person under a standing rule. The person sets it per rule.
- A task that has read open untrusted input loses its standing grants and rules to cross (ADR 0011).
