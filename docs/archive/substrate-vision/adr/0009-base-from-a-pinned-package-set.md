# Base generations are built from a complete pinned package set

Each Base generation is built from empty out of a complete package set that the Crowbar State pins, and is read-only once built. The State owns every package in Base. The person's Data and the machine's own writable state live on separate subvolumes, outside every generation.

Milestone 1 instead snapshots the running root and converges the copy toward a State that lists only some packages. Council round 1 found that this model is the cause of most of its defects: a generation depends on the history of the root it was copied from, so it is not a build of the State; anything written between an apply and the reboot is left behind in the old generation; and taking over a file or package and later dropping it deletes it.

## Considered Options

- **Keep converging a snapshot and repair it (one reviewer's recommendation).** Rejected: the ownership rules needed to make a partly described system safe are where the defects are, and "the State is a lockfile" cannot hold while the State describes only a delta.

## Consequences

- The generation layout, boot menu, ledger, apply lock, and VM lab from Milestone 1 are kept. The plan and ownership logic is replaced.
- Building from empty needs a pinned repository snapshot, and a policy for how Base receives security updates.
- A read-only Base needs a defined place for each kind of writable state.
