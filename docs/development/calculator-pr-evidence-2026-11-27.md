# Calculator PR evidence — November 27, 2026

The [Calculator validation record](calculator-validation-2026-11-27.md)
records the full Void build and desktop run: **316 passed, 0 failed**, exit
status 0, on runtime commit `d9eeceae81a205442f39dee1d23aeac48894705d`.
Its base was `3883ccb0ca29017867fe79e76f311dcd05859e07`; `origin/main`
remained there when preparing this pull request. Runtime files have not
changed since that validation.

The PR includes these copies of that run's evidence:

- [desktop.log](assets/calculator-2026-11-27/desktop.log)
- [results.json](assets/calculator-2026-11-27/results.json)

The original local logs remain unchanged. The published desktop log replaces
three occurrences of the host checkout path with `<checkout>`. The result
JSON is unchanged. Guest lab-account paths, loopback, and QEMU's guest-network
addresses are test context, not addresses on a person's network.

Before/after captures, mode views, and Dark/Light typing recordings are linked
from the validation record. The earlier visual review and documentation
handoff remain as written.

Before publication, Base helpers and Lever also passed their CI commands:
`cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
and `cargo test --locked`. Helpers passed 54 tests; Lever passed its client,
library, binary, and integration suites. Python compilation passed for Base,
lab, workflow, and shell packaging scripts. The previous full build and unit
test evidence remains applicable because runtime sources are unchanged.

The shared theme and UI libraries passed their locked release builds and
tests in Base's Void build root as well.
