# Fulcrum builds on its own, and CrowbarOS builds the commit it pins

**Date:** November 17, 2026. **Plan milestone:** [0 · Independent build](PLAN.md#implementation-milestones).

Fulcrum was split from CrowbarOS's `compositor/` directory. CrowbarOS was then
changed to build the Fulcrum commit it pins, instead of that directory. Run at
both ends, the same 36 checks on a TTY in the Void guest pass, and the two
builds produce byte-identical binaries. The move changed nothing that is built.

| Repository | Commit | What it is |
| --- | --- | --- |
| CrowbarOS | `30fadd8e88fbeb9991eb340f5dee27998b02c9fe` | The last commit with the compositor in `compositor/`. The baseline |
| Fulcrum | `f779d6f5c33807694de13a7062e36040194c65fc`, tag `import/crowbaros-30fadd8` | `git subtree split --prefix=compositor` of the baseline. Seven commits; its tree is `30fadd8:compositor` exactly |
| Fulcrum | `d11400fa36f8056b81023a5c71fffe7197e50d85` | What the import needed to stand alone. No Rust source changed. The pinned commit |
| CrowbarOS | `d9ed44646067902bccfc4c65ed9eb7da9411b9b7`, branch `compositor-from-fulcrum` | Deletes `compositor/`, pins Fulcrum `d11400f`, and builds it in the Void lab. ADR 0027, proposed |

## What was run

| Check | Where | Result |
| --- | --- | --- |
| `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked`, and clippy with `--features nested` | A fresh clone of Fulcrum `d11400f` on the tower (Arch, rustc 1.99.0) | Pass. 57 tests |
| The same, before `rustfmt.toml` was added | Fulcrum at the import tag | `cargo fmt --check` fails with 172 hunks. CrowbarOS's root `rustfmt.toml` sets `max_width = 120`, and rustfmt found it from the parent directory. Clippy and the tests passed |
| `lab/void/compositor_guest.py check` | CrowbarOS `30fadd8`, building `compositor/` | 36 passed, 0 failed. [Evidence log](assets/independent-build-2026-11-17/guest-check-crowbaros-30fadd8.log) |
| `lab/void/compositor_guest.py check` | CrowbarOS `d9ed446`, building Fulcrum `d11400f` as pinned, fetched from a local clone | 36 passed, 0 failed. [Evidence log](assets/independent-build-2026-11-17/guest-check-fulcrum-d11400f.log) |
| `python3 -m unittest discover -s lab/void -p "test_*.py"` | CrowbarOS `d9ed446` | Pass. 17 tests, 9 of them new for the pin |
| `git revert` of `d9ed446` | A scratch clone of the CrowbarOS branch | The result's tree is `97725bee`, the tree of `30fadd8`, so reverting the pin restores the baseline build |

Both guest runs built and tested in the Void build root with rustc 1.99.0
(Void Linux), and booted Linux 6.18.64 in QEMU with virtio-gpu and software
rendering. Both built the same binaries:

| Binary | sha256, at both commits |
| --- | --- |
| `crowbar-compositor` | `5d6aa9a455c643c467354f980a1694b25a780f638c97ab46aa982a195362dd89` |
| `crowbar-probe` | `0396f0a160f609414443921b68b281551d08c96506bee952430b80d74c03bc16` |

The source sat at `/src/compositor` for one build and at
`/src/.lab/fulcrum/d11400f…` for the other. The binaries are still identical,
because Cargo passes a workspace member's paths to rustc relative to the
workspace root.

## What was not run, or not done

- **The nested check, `checks/nested.sh`.** The tower has no `wayland-info`
  (Arch's `wayland-utils`), and the agent's shell had no Wayland session to nest
  in. Run it from a terminal in the session after installing `wayland-utils`.
- **Fulcrum's CI.** Fulcrum is not published, so `.github/workflows/ci.yml` has
  not run on GitHub. Its commands are the ones run above.
- **Fetching from the pin's repository.** The pin names
  `https://github.com/crowbaros/fulcrum.git`, which does not exist yet. The
  guest run set `FULCRUM_REPOSITORY` to the local clone. The commit ID, not the
  location, decides what is built.
- **Cross-linking the slice issues.** Issues 71, 78, 79, 87, 166, and 174 stay
  in CrowbarOS's tracker (ADR 0027). Each needs a note that its code is now in
  Fulcrum, once Fulcrum is published.
- **ADR 0027 is proposed, not accepted.** It needs Jim Martin's agreement
  before the CrowbarOS branch merges.
