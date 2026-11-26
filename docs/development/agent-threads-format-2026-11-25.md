# Agent thread client formatting — November 25, 2026

PR #410's Lever job failed `cargo fmt --check` on `28bd772` in three
client files. This also reproduced locally. The earlier validation record's
formatting claim did not hold for that submitted revision; this record corrects
it without rewriting the original evidence.

`83760dd138ae9423a8e3d2466bd2df0e89868f6f` applies rustfmt to those files. There is no behavior change.
The existing formatting gate fails before this correction and passes afterward.

Verification ran on that commit, based on main
`da162fa793260a6563a50855165222764393132c`:

- In `lever/`, `cargo fmt --check` and
  `cargo clippy --locked --all-targets -- -D warnings` passed.
- `cargo test --locked` passed all **93 tests**.
- Full `python3 lab/void/desktop_check.py` rebuilt the image and passed
  **189 checks, 0 failures**. [desktop.log](assets/agent-threads-format-2026-11-25/desktop.log)
  and [results.json](assets/agent-threads-format-2026-11-25/results.json).

No compositor-covered paths changed; the earlier full 46-check compositor
result remains the evidence for this PR. Original captures and the feature's
validation limits are in the [original record](agent-threads-2026-11-25.md).
Text evidence copies remove host paths, host CPU model details, terminal color
codes, and trailing whitespace; original local logs remain unchanged.

Model and harness: GPT-6-astra, Codex harness in M3 Code.
