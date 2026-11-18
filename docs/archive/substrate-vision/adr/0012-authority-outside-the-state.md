# Authority is stored outside the Crowbar State and its generations

Standing grants, standing rules, effect limits, Allowances, and clearances live in their own **authority** domain with their own history. The vault and the ledger are separate domains too, and the Agent's memory is the person's Data. None of them is part of the Crowbar State, and none is rolled back with a generation. The State says what each Tool needs; the authority domain records what the person granted.

The earlier design put grants and rules in the State (`[capabilities]`, `[autonomy]`). Generations are builds of the State, so rolling back an unrelated software change would have restored a revoked grant, and a shared State would have carried authority that must never transfer.

**In the first version one machine holds authority for the fleet.** The **lead machine** holds the authority domain and puts promotes in order. The other machines work on leases from it that expire. A change to authority is made on the lead machine, or approved from the phone and recorded there.

## Considered Options

- **Every machine holds a copy of authority and they merge.** Rejected for the first version: a revoked grant must stop working everywhere, and a merge of offline copies cannot promise that. A lease that expires can.

## Consequences

- Undoing software can never increase authority. Rolling back to a generation whose Tools need a grant that has since been revoked leaves those Tools without it.
- The Crowbar State holds no authority, so sharing a part of it never transfers any. What is shared is chosen per Tool (ADR 0010).
- A machine that cannot reach the lead machine keeps working until its leases expire, and then stops crossing the boundary. How long a lease lasts is a trade between working offline and how fast a revocation takes hold.
- Losing the lead machine is a recovery case, and it is not designed yet.
- The Agent's memory is the person's Data, not the harness's, so changing the harness keeps it.
