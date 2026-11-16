# Crates

The Rust workspace for the parts of CrowbarOS that are new. The workspace root is
the repository root.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

| Crate | What it is | Milestone | State |
| --- | --- | --- | --- |
| [`crowbar-state`](crowbar-state/) | The Crowbar State document: schema, parsing, validation | 1 | Built |
| [`crowbar-apply`](crowbar-apply/) | The `crowbar` command: plan, apply, generations, rollback, drift, ledger | 1 | Built |
| [`crowbar-labels`](crowbar-labels/) | Labels, marks, and the origin table | 2, first deliverable | Skeleton |
| [`crowbar-gate`](crowbar-gate/) | The gate: the deterministic rules engine that allows or refuses | 2, part one | Skeleton |
| [`crowbar-broker`](crowbar-broker/) | The capability broker: handles, egress, adapters, the Tool contract | 2, part one | Handles and effect limits |
| [`crowbar-authority`](crowbar-authority/) | The authority domain and the ledger, stored outside the State and its generations | 2, part one | Standing grants and rules, clearances, the ceiling, revocation, and the ledger of decisions and effects |
| [`crowbar-plan`](crowbar-plan/) | Plans over handles and their deterministic interpreter | 2, part one | Skeleton |
| [`crowbar-sandbox`](crowbar-sandbox/) | Task principals as compartments in Linux sandboxes | 2, part one | Skeleton |

A skeleton crate holds a description and no code. Its boundary is a first guess
taken from the [Milestone 2 scope](../docs/milestones/milestone-2.md) and the
[design notes](../docs/architecture/). Merge, split, or rename the skeletons when
the code shows a better boundary.

Not yet given a crate, because their milestone is further away: the runner and
rehearsal, forks and change sets (Milestone 2, part two), the Data layer
(Milestone 3), the vault, the outbox, the monitor, and the harness protocol
(Milestone 5), and the fleet (Milestone 6). The shell (Milestone 4) starts from
[`apps/shell`](../apps/README.md).

The Milestone 2 fixture services, the red-team harness, and the attack runs go in
[`lab/adversarial`](../lab/adversarial/README.md).

The GPUI apps in `apps/` are not members of this workspace. They pin their own
dependency graphs.
