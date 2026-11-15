# The approver checks alignment; security is deterministic

A small model can be wrong, and an injection can hide inside the action itself. So the system's safety does not depend on the approver. Security comes from deterministic parts: capability handles, plus an **undo boundary**. Effects inside the boundary stay within the person's **fleet** (the machines they own, joined on purpose), are recorded, and can be fully undone, so they just happen and go in the ledger. Effects that cross it (data leaving the fleet, acting as the person, money, credentials, hardware) need a standing grant, a standing rule, or the person's answer. The approver only answers "is this what the person asked for?", as defense in depth.

The earlier design sorted actions into three risk tiers and three modes. That is replaced by this single rule. "Mode" now controls only how much the person is told. No model is on the trust path, so state, apply, rollback, capabilities, and deterministic approval all work with no model at all.

## Consequences

- Calling a cloud model, or using an external harness that runs in someone else's cloud (such as Grok Bot), sends data out of the fleet, so it crosses the boundary. A model provider is a standing grant scoped by data label. Private data goes to a model running in the fleet (for example, open weights on the person's DGX Spark) unless the person allows otherwise.
- Labels start from where the data came from and carry over to anything derived from it. The agent may make a label stricter, but only the person can declassify.
- Gating egress needs data labels that follow the data, which is one reason the Data layer is typed and tracks provenance.
