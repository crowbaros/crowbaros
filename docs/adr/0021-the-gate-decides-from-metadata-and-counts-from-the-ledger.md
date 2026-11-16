# The gate decides from metadata in a fixed order, counts from the ledger, and stands in for its signature with a value only it makes

**Status:** accepted on November 15, 2026, by Jim Martin, in the design session for [issue 75](https://github.com/crowbaros/crowbaros/issues/75).

[ADR 0003](0003-security-is-deterministic.md) says the gate is deterministic and is the only thing that allows. [ADR 0011](0011-task-principals.md) says what crosses the undo boundary and when a marked act may cross. The [capability model](../architecture/capability-model.md) asks the gate for a dry run, refusals that name the rule and the limit, and idempotency keys. None of them says how the gate is built, what it decides from, what order it checks in, what it counts, or what the broker holds as proof that the gate allowed something. `crates/crowbar-gate` needs an answer to each, and the ledger keeps what it records. This record gives the answers.

## Shape

- **The gate is a pure library at Alpha.** Its decision is `decide(authority, usage, operation, now)`. Nothing inside it reads a file, a clock, or the network: the broker passes in the time, the use count, and the operation. It moves to a process of its own later.
- **`Allowed` stands in for a signature until then.** It is a value that only `crowbar-gate` can make and that cannot be copied. It covers one operation, and the gate makes it only after the ledger has recorded the decision. `HandleTable::issue` and `HandleTable::delegate` take one and refuse one that does not cover what they are asked to do. A key in the broker's own process would protect nothing, so the gate does not sign yet. Signing comes with the gate's own process, and settles open question 5 of the [gate design](../architecture/gate-monitor-and-autonomy.md).
- **The shared types live in `crowbar-capability`.** The broker depends on the gate, the gate on authority, and authority on neither. Principals, resources, rights, effect limits, handle ids, what the broker asks to issue, the gate's reasons, and idempotency keys are in that crate. `crowbar-broker` re-exports the ones it used to define.
- **The gate decides delegation.** Every copy gets its own `Allowed`, its ledger entry names its original, and every use of a copy counts against both the copy and the original. This closes question 1 of the capability model. The handle table keeps its own checks as a second line of defence, and runs them before it looks at the `Allowed`.

## The decision

- **One closed list of reasons.** A refusal names a rule and the limit that applied, never a value. An ask says why it asks. The gate, the ledger, and the harness share the list, and every refusal of the handle table maps onto it.
- **The gate decides only from metadata:** handles, labels, marks, limits, authority, and the declared shape of the operation. A check that would depend on a value the task cannot read, such as an amount in a handle, asks, and the person sees the value. A **dry run** returns the same decision as the real operation, and counts no use, opens no ask, issues nothing, and adds nothing to the ledger.
- **A fixed order of checks.** The first check that fails decides, so a refusal never shows what a later check would have said. Any refusal wins over an ask.
  1. The handle: known, held by the caller, live, not expired.
  2. The right, and for a delegation, that the handle may be passed on, and not to the Agent.
  3. The effect limits, with a copy's uses also counted against its original.
  4. The label and mark at issuance: a Data handle's are at or above the join of its objects' origins. An object with no record is the unknown row ([ADR 0019](0019-the-origin-table-fails-closed.md)).
  5. The clearance, on egress: the compartment's whole label, from the broker's record of it, is at or below the party's clearance.
  6. The undo boundary, with the standing grant and standing rule a crossing needs, the rule for every source that marked the act.
  7. If the result is an ask: the ask rate.
- **The undo boundary is a fixed table.** Each adapter operation declares whether it stays inside. An operation the table does not list crosses, and an egress always does.
- **A use declares** which objects it affects, and its cost: a fixed amount, or "depends on a value", which asks. The gate checks the objects against the objects a Data handle names and against every "which objects" limit that applies, and counts them against "how many". A use that affects no object counts as one operation.
- **A standing grant on a set of Data objects covers a handle on some of them.** Any other resource must be the same.
- **Ask rate limits are constants for now.** A principal may have 1 ask open at a time and 10 an hour. The Agent and all its tasks together may have 30 an hour, and the asks that start tasks count. Over a limit, the gate refuses and names the limit and when its window resets. Nothing is queued.

## State

- **The gate counts uses from the ledger.** It rebuilds the count from the ledger's history when it starts, then adds each entry it writes, through the same function. A restart does not reset a limit such as "5 an hour". A ledger entry for a decision names the handle, its original, the task, how many objects it affected, its cost, and its idempotency key. An entry also records when a principal's open ask closes.
- **Idempotency keys come from the task, one set per task, and the ledger records them** with a digest of the operation. A repeated key gets the first decision again and counts nothing. An allowed repeat carries no `Allowed`, so nothing is carried out twice. A key reused on a different operation is refused. An operation without a key is decided and counted each time.
- **The gate fails closed.**
  - An unknown handle, principal, or operation is refused. An unknown operation is one whose name is not `service.operation`.
  - With no authority record to read, every crossing asks, and no party is cleared above public, because the person may have lowered a clearance. A refusal stays a refusal.
  - A ledger that cannot be added to refuses every operation. Dry runs still answer.

## Considered Options

- **The gate signs with a key at Alpha.** Rejected: in the broker's process a key protects nothing that an unforgeable value does not, and choosing the key material is a decision for the gate's own process.
- **The gate as a process now.** Rejected for Alpha: the rules are the hard part, and a pure function can be tested completely.
- **The gate keeps its count in memory only.** Rejected: a restart would reset every rate limit.
- **A value-dependent check reads the value inside the gate.** Rejected: a dry run could then be used to learn the value a little at a time (attack A19).
- **The table names an unlisted operation as inside the boundary.** Rejected: a new adapter would cross without asking until someone listed it.
- **Copies need no `Allowed` of their own.** Rejected: the ledger could not show the chain, and nothing would stop a holder passing a handle on more widely than the gate would allow.

## Consequences

- What an ask shows, releases, authorization at a task's start, the ceiling, and budgets across tasks are [issue 103](https://github.com/crowbaros/crowbaros/issues/103). The gate's check on a commit of staged writes is [issue 90](https://github.com/crowbaros/crowbaros/issues/90). The label check on a handle that is not a Data handle is [issue 91](https://github.com/crowbaros/crowbaros/issues/91).
- Each adapter has to add its operations to the boundary table, and the checks and rehearsal work has to test that each row is true.
- Ask rate limits apply to every principal, not only to tasks. A Tool that floods the person is the same problem.
- The ledger's format changed before anything was deployed: a refusal records a rule and a limit, not a string.
- The gate's proof is called `Allowed`, never an allowance: an Allowance is money the person sets aside.
- Machine clearances wait for Milestone 6.
