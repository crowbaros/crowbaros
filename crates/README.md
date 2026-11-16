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
| [`crowbar-capability`](crowbar-capability/) | The terms the broker, the gate, and the ledger share: principals, resources, rights, effect limits, handle ids, and the gate's reasons | 2, part one | Built |
| [`crowbar-gate`](crowbar-gate/) | The gate: the deterministic rules engine that allows, refuses, or asks | 2, part one | Decisions on issue, delegate, use, and egress; dry runs; the use count and idempotency keys from the ledger |
| [`crowbar-broker`](crowbar-broker/) | The capability broker: handles, egress, adapters, the Tool contract | 2, part one | Handles and effect limits, issued and passed on only with the gate's `Allowed` |
| [`crowbar-authority`](crowbar-authority/) | The authority domain and the ledger, stored outside the State and its generations | 2, part one | Standing grants and rules, clearances, the ceiling, revocation, and the ledger of decisions and effects |
| [`crowbar-plan`](crowbar-plan/) | Plans over handles and their deterministic interpreter | 2, part one | Skeleton |
| [`crowbar-sandbox`](crowbar-sandbox/) | The Warden: every compartment in its own Linux sandbox | 2, part one | Sandbox, budgets, and stop, as a library |

## The Warden's tests need root

The Warden runs as root
([ADR 0016](../docs/adr/0016-the-warden-starts-every-compartment.md)), so the
tests in `crowbar-sandbox` that start real compartments are ignored by a plain
`cargo test`. Nobody needs root on their own desktop for them:

```sh
lab/void/warden_check.py       # build them, run them as root in a fresh Void lab guest
```

CI runs the same tests under `sudo`. On a machine where root is yours to use,
with cgroup v2 and a kernel that has Landlock:

```sh
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' \
  cargo test -p crowbar-sandbox --test compartments -- --ignored
```

They make a cgroup at `/sys/fs/cgroup/crowbar-warden-test`, keep their record of
user IDs in `/run/crowbar-warden-test`, and use user IDs from 1,900,000,000 up.
One test sets the machine's `kernel.core_pattern` for as long as it runs, and
puts it back.
The test that measures disk I/O needs its files on a disk: where `/tmp` is in
memory, set `CROWBAR_SANDBOX_TEST_DIR` to a directory that is not.

## Skeletons

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
