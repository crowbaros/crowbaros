# Each compartment has a budget in the compositor, and the trusted parts keep a reserve

**Status:** accepted on November 17, 2026, by Jason Alexander, who directed a coding agent to decide [issue 174](https://github.com/crowbaros/crowbaros/issues/174) and carry it out. Jim Martin had not reviewed it when it was accepted.

Every connection and every shared-memory pool costs the compositor a descriptor, and its process has a fixed number of them. Issue 174 found that nothing capped either. One compartment could take every descriptor, and then the compositor accepted nothing more: not another compartment, not the shell, not the Warden. It lasted until the Warden stopped that compartment. A connected client that sent a descriptor could also lose it silently, because the kernel drops a descriptor the receiver has no room for.

Issue 174 asked three questions. This record answers them for Fulcrum, which now owns the wire layer where the descriptors arrive ([ADR 0028](0028-the-compositor-is-written-from-the-ground-up.md)).

## The decision

- **Where the limit is: in the compositor, for each compartment and each client.**
  - Each compartment may hold 16 connections and 512 descriptors in all. That counts connections, pools, and descriptors a client sent that no message has taken yet.
  - Each client may hold 128 pools and 512 MiB of shared memory. One pool is at most 256 MiB.
  - At most 64 descriptors may wait in one connection for a message to take them.
- **A reserve for the trusted parts.** A compartment's client is accepted, and a compartment's pool is made, only while more than 64 descriptors are left. The shell and the Warden are accepted down to the last one.
- **The compositor raises its own descriptor limit** to its hard limit when it starts, so these budgets are a small part of what it has.
- **What happens at the limit.**
  - A connection over its compartment's budget is accepted and closed at once, so it waits in no queue. The client sees its connection end.
  - A pool over a budget is a protocol error, `no_memory`, which ends that one client.
  - A message whose descriptors the kernel dropped ends its connection, instead of handing a later message the wrong descriptor.
  - Nothing over a budget affects another client.
- **Who holds the numbers: the compositor, for now.** They are Fulcrum's constants (`src/limits.rs`). The Warden already holds each compartment to a budget. Passing a compartment's own numbers through the Warden's registration is a later change, which needs no change here.

## Considered Options

- **Only a higher descriptor limit.** Rejected: it delays the exhaustion and does not prevent it. One compartment can still take everything the higher limit gives.
- **Limit each client, not each compartment.** Rejected: a compartment can open many clients, so a per-client limit alone does not bound what one compartment holds.
- **End the whole compartment at its limit.** Rejected: that is the Warden's decision, not the compositor's. The compositor refuses what is over the budget, and the client that asked for it ends.
- **Refuse a connection by not accepting it.** Rejected: the connection then waits in the listener's queue, and the next accept takes it anyway.

## Consequences

- Issue 174's criteria are met by Fulcrum's `tests/exhaustion.rs`:
  - a compartment that opens connections without end is stopped at its budget;
  - the shell, the Warden, and another compartment are still accepted, and a connected client of another compartment can still send a buffer;
  - a client's pools are bounded by count and by bytes;
  - the reserve holds when few descriptors are left.
- A legitimate app with more than 16 connections, or more than 512 MiB of shared memory, is refused. No app the Alpha desktop runs comes near either. A budget can grow in a pull request of its own.
- The numbers are the compositor's until the Warden passes a compartment's own.
