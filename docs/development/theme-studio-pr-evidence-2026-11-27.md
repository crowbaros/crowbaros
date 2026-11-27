# Theme Studio PR evidence — November 27, 2026

The [validation record](theme-studio-validation-2026-11-27.md) records the
full Void build and desktop run: **335 passed, 0 failed**, exit 0, on
`93377585be5fa36d1b0bc29b74dae26fb6653488`. The full compositor run passed
**47 checks, 0 failed**, exit 0; its source and later changes are explained
in that record. Both ran on main `7a2423fd69234bf263bcc2fa57232975a643fc21`.
Main remained there when preparing this PR, and its CI was green. Runtime
files have not changed since validation.

Published evidence:

- [Desktop log](assets/theme-studio-2026-11-27/desktop.log)
- [Desktop results](assets/theme-studio-2026-11-27/results.json)
- [Compositor evidence log](assets/theme-studio-2026-11-27/compositor.log)

Original local evidence stays unchanged. These copies replace three host
checkout-path occurrences in the desktop log and four in the compositor log
with `<checkout>`. The results JSON is unchanged. Guest account paths,
loopback, and QEMU's guest-network addresses describe disposable lab guests.
The Agent checks' generated secret markers and stub credentials are test
fixtures, not account credentials.

The validation record links the before/after captures, Dark and Light
native workflows, compact layout, keyboard focus, error recovery, independent
review, and documentation handoff. No runtime edits followed those checks,
so the existing component and full graphical evidence remains applicable.

Implementation and validation: GPT-6 Astra through the Codex harness in M3 Code.
