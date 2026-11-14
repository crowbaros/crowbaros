# agent-first: Crowbar State and the apply engine

The Rust workspace and VM lab for agent-first CrowbarOS. Start with the
[vision](../docs/agent-first/VISION.md) and the
[Milestone 1 record](../docs/agent-first/milestone-1.md).

| Path | Contents |
| --- | --- |
| `crates/crowbar-state` | The Crowbar State document: schema, parsing, validation |
| `crates/crowbar-apply` | The `crowbar` command: plan, apply, generations, rollback, drift, ledger |
| `lab/void_lab.py` | Void Linux VM lab: fetch, rootfs, image, start, ssh, stop, milestone |
| `lab/host/` | Rootless rootfs and disk-image build scripts (run inside `unshare -r`) |
| `lab/states/` | The three Milestone 1 state files |

```sh
cargo test --workspace                 # unit tests
cargo clippy --workspace --all-targets -- -D warnings
lab/void_lab.py milestone              # full VM loop; evidence goes to ../.lab/void/
```
