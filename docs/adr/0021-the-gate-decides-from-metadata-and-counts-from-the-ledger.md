# The gate decides from metadata in a fixed order, counts from the ledger, and stands in for its signature with a value only it makes

**Status:** accepted on November 15, 2026, by Jim Martin, in the design session for [issue 75](https://github.com/crowbaros/crowbaros/issues/75). Revised on November 16, 2026, after council round 5: an ask over a limit is held and not refused ([ADR 0026](0026-asks-wait-and-take-one-of-three-answers.md)), a standing rule with a cap can fire without an ask, and three kinds of egress do not ask ([ADR 0011](0011-task-principals.md)). `crates/crowbar-gate` was built to the earlier text and changes through its own issues.

[ADR 0003](0003-security-is-deterministic.md) says the gate is deterministic and is the only thing that allows. [ADR 0011](0011-task-principals.md) says what crosses the undo boundary and when a marked act may cross. The [capability model](../architecture/capability-model.md) asks the gate for a dry run, refusals that name the rule and the limit, and idempotency keys. None of them says how the gate is built, what it decides from, what order it checks in, what it counts, or what the broker holds as proof that the gate allowed something. `crates/crowbar-gate` needs an answer to each, and the ledger keeps what it records. This record gives the answers.

## Shape

- **The gate is a pure library at Alpha.** Its decision is `decide(authority, usage, operation, now)`. Nothing inside it reads a file, a clock, or the network: the broker passes in the time, the use count, and the operation. It moves to a process of its own later.
- **`Allowed` stands in for a signature until then.** It is a value that only `crowbar-gate` can make and that cannot be copied. It covers one operation, and the gate makes it only after the ledger has recorded the decision. `HandleTable::issue` and `HandleTable::delegate` take one and refuse one that does not cover what they are asked to do. A key in the broker's own process would protect nothing, so the gate does not sign yet. Signing comes with the gate's own process, and settles open question 5 of the [gate design](../architecture/gate-monitor-and-autonomy.md).
- **The shared types live in `crowbar-capability`.** The broker depends on the gate, the gate on authority, and authority on neither. Principals, resources, rights, effect limits, handle ids, what the broker asks to issue, the gate's reasons, and idempotency keys are in that crate. `crowbar-broker` re-exports the ones it used to define.
- **The gate decides delegation.** Every copy gets its own `Allowed`, its ledger entry names its original, and every use of a copy counts against both the copy and the original. This closes question 1 of the capability model. The handle table keeps its own checks as a second line of defence, and runs them before it looks at the `Allowed`.

## The decision

- **One closed list of reasons.** A refusal names a rule and the limit that applied, never a value. An ask says why it asks. The gate, the ledger, and the harness share the list, and every refusal of the handle table maps onto it.
- **The gate decides only from metadata:** handles, labels, marks, limits, authority, and the declared shape of the operation. A check that would depend on a value the task cannot read, such as an amount in a handle, asks, and the person sees the value. The one exception is a standing rule with a cap, below. A **dry run** returns the same decision as the real operation, and counts no use, opens no ask, issues nothing, and adds nothing to the ledger.
- **A fixed order of checks.** The first check that fails decides, so a refusal never shows what a later check would have said. Any refusal wins over an ask.
  1. The handle: known, held by the caller, live, not expired.
  2. The right, and for a delegation, that the handle may be passed on, and not to the Agent.
  3. The effect limits, with a copy's uses also counted against its original.
  4. The label and mark at issuance: a Data handle's are at or above the join of its objects' origins. An object with no record is the unknown row ([ADR 0019](0019-the-origin-table-fails-closed.md)).
  5. The clearance, on egress: the compartment's whole label, from the broker's record of it, is at or below the party's clearance. For a logged-in task, a host on the shared list is cleared for what the task holds for its one site, and for nothing else, until the task ends ([ADR 0013](0013-agents-never-drive-the-persons-browser.md)).
  6. The undo boundary, with the standing grant and standing rule a crossing needs, the rule for every source that marked the act.
  7. If the result is an ask: whether it is held, and which answer it takes.
- **The undo boundary is a fixed table.** Each adapter operation declares whether it stays inside. An operation the table does not list crosses, and an egress always does.
- **Three kinds of egress cross on authority the person already gave, and do not ask** (ADR 0011):
  - a model request, when the provider is cleared for everything the compartment holds and the model budget is not spent;
  - installed software or a workbench reaching what its approval showed: the approval is its standing grant and standing rule;
  - a compartment that holds only public data and no login reaching any host on the open internet. Every outside party is cleared for public data, so the clearance check is the whole decision. A machine in the fleet or an address on the home network is not such a host.
- **An ask names the answer it takes** (ADR 0026): a plain answer when the effect stays inside the undo boundary, the confirm gesture when it crosses or the authority lasts only for this run, and a signature when the authority lasts for good.
- **A use declares** which objects it affects, and its cost: a fixed amount, or "depends on a value". A cost that depends on a value asks, unless a standing rule with a cap covers the act.
- **A standing rule with a cap fires without an ask when the amount is under the cap.** The gate still never reads the amount. It answers "allowed if within the cap", and the adapter checks the amount against the cap when it carries the act out. Over the cap, the adapter carries nothing out, and the act asks, with the amount shown. A dry run says only "allowed if within the cap". The gate checks the objects against the objects a Data handle names and against every "which objects" limit that applies, and counts them against "how many". A use that affects no object counts as one operation.
- **A standing grant on a set of Data objects covers a handle on some of them.** Any other resource must be the same.
- **Ask limits are constants for now.** A principal has 1 ask waiting at a time. A task pauses until it is answered. For a Tool, installed software, or a workbench, only the operation that asked waits. The limits are 10 an hour for one principal, and 30 an hour for the Agent and all its tasks together, and the asks that start tasks count. Over a limit, the gate still answers "ask", and marks the ask as held, with the limit it passed. A held ask waits like any other until it closes. It does not count against the hourly limits: those count the asks that were put to the person one at a time, so one principal's flood cannot use up the limit that the Agent and all its tasks share. An ask past both the Agent's limit and its own principal's goes in the Agent's line. The shell shows the asks held under one principal's limit as one line, and the asks held under the Agent's limit as one line for the Agent and all its tasks together. The person can open a line or dismiss it whole (ADR 0026). Nothing is refused for asking too often.

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
- **Every act whose cost depends on a value asks (the first version).** Rejected by round 5: "pay the electric bill, up to $150" asked every month, which is the ask the rule was written to remove.
- **Refuse an ask over the rate, and queue nothing (the first version).** Rejected by round 5: honest work was refused for how fast it asked, and a refusal counts against the friction bar as an ask does ([ADR 0024](0024-the-friction-bar-ranks-with-the-attack-list.md)).
- **The table names an unlisted operation as inside the boundary.** Rejected: a new adapter would cross without asking until someone listed it.
- **Copies need no `Allowed` of their own.** Rejected: the ledger could not show the chain, and nothing would stop a holder passing a handle on more widely than the gate would allow.

## Consequences

- What an ask shows, releases, authorization at a task's start, the ceiling, and budgets across tasks are [issue 103](https://github.com/crowbaros/crowbaros/issues/103). The gate's check on a commit of staged writes is [issue 90](https://github.com/crowbaros/crowbaros/issues/90). The label check on a handle that is not a Data handle is [issue 91](https://github.com/crowbaros/crowbaros/issues/91).
- Each adapter has to add its operations to the boundary table, and the checks and rehearsal work has to test that each row is true.
- Ask limits apply to every principal, not only to tasks. A Tool that floods the person is the same problem.
- **Whether a capped act went through is something the task can observe.** That is one bit about the amount for each attempt, and the rule's rate limit bounds the attempts.
- `standing_holder` in `crates/crowbar-gate` names a Tool, a workbench, and a standing task. Installed software is added, and a model request and a public compartment's egress stop asking.
- The ledger's format changed before anything was deployed: a refusal records a rule and a limit, not a string.
- The gate's proof is called `Allowed`, never an allowance: an Allowance is money the person sets aside.
- Machine clearances wait for Milestone 6.
