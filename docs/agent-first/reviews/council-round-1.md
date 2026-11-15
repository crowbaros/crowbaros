# Council round 1: agent-first vision and Milestone 1

**Date:** November 14, 2026. **Reviewed:** branch `agent-first` at commit `b4581f2`.

Three models reviewed the branch independently from one brief, with no opinion
supplied, and were asked to reason from first principles and from their own
experience as agents. Jim Martin then answered twelve questions drawn from their
reports. This page records the scores, what they found, what was decided, and the
ideas that are not decided yet.

| Delegate | Vision and design | Milestone 1 as a foundation |
| --- | --- | --- |
| Claude Opus | 6/10 | 5/10 |
| Claude Fable | 6/10 | 5/10 |
| GPT Sol | 7/10 | 6/10 |

All three read the documents and the Rust code, ran the unit tests, and reproduced
defects in scratch copies. None booted the VM.

## What all three would keep

- No model on the trust path.
- The broker makes every outbound connection and holds the secrets.
- The Allowance, with a limit the issuer enforces.
- The Agent as a role, with the harness and model swappable.
- Owner authority, and recovery that needs no model.
- Linux for drivers.
- The candour of the Milestone 1 "Not verified" list.

## What they found wrong in the vision

| Finding | Raised by |
| --- | --- |
| "Undoable" was treated as "safe to do automatically". Undo only helps a person who notices, and effects inside the fleet can be impossible to take back. | All three |
| Checks carried the design and had no executable meaning, which put a model on the acceptance path for a re-derive. | All three |
| Labels do not survive a model's context: one private value read, and everything written after is private. | All three |
| The text contradicted itself: small edits "just happen" yet unconfirmed intents are refused; every handle needs an approver signature yet the system works with no model. | All three |
| The roadmap tested the least risky layer first. | All three |
| No story for many agents changing one OS at once. | Opus, Fable |
| WASM's benefit was overstated: the sandbox and the broker's binding do the enforcing. | Opus, Fable, and Sol in part |
| Shared intents are an injection channel. | Opus, Fable |
| The fleet was one flat perimeter. | Sol, Fable |
| Grants lived in the State, so a software rollback could restore a revoked grant. | Fable |
| The thesis overreached: bespoke software is a long tail, and the browser was missing. | Fable |
| Durable intents and temporary tasks need separate representations. | Sol |

## Decisions

Jim answered yes to all twelve, and added that security needs to be the top
priority.

| # | Question | Decision | Recorded in |
| --- | --- | --- | --- |
| 1 | Scope of the thesis | Bespoke software is a long tail. The browser and native apps are first-class. | [Vision](../VISION.md), Thesis |
| 2 | Source of truth | Intent, checks, and the pinned build together. | [ADR 0001](../../adr/0001-intent-is-the-source.md) |
| 3 | What a check is | An executable test, frozen at confirmation, run in rehearsal. | ADR 0001 |
| 4 | The undo boundary | An autonomy default. Effects are authorized by effect limits. Acts on the person's behalf wait in an outbox. | [ADR 0003](../../adr/0003-security-is-deterministic.md) |
| 5 | How the Environment changes | Fork, verify, promote, with one fork per task. | [ADR 0007](../../adr/0007-fork-verify-promote.md) |
| 6 | Where the thinking happens | Assume a cloud model. Private values stay out of its context. | [ADR 0008](../../adr/0008-private-values-stay-out-of-the-agent-context.md) |
| 7 | One Agent or many task principals | The Agent coordinates task principals. Authority shrinks on untrusted input. | [ADR 0011](../../adr/0011-task-principals.md) |
| 8 | Tool runtime | The contract is the broker protocol. WASM is one runtime. | [ADR 0004](../../adr/0004-capability-safe-tools.md) |
| 9 | Base engine | Build each generation from a complete pinned package set. | [ADR 0009](../../adr/0009-base-from-a-pinned-package-set.md) |
| 10 | Next milestone | A fix-up, then an adversarial slice. | Vision, Roadmap (M1b, M2) |
| 11 | Sharing default | The pinned build with its intent and checks. Shared words are untrusted input. | [ADR 0010](../../adr/0010-share-the-pinned-build.md) |
| 12 | Approver, authority, fleet trust | A gate that allows and a monitor that can only pause. Authority outside the State. A clearance per machine, with the phone as an approval device. | ADR 0003, [ADR 0012](../../adr/0012-authority-outside-the-state.md), ADR 0008 |

Two choices were made while writing these up and were not put to Jim as questions:

- **Provisional edits.** To resolve the contradiction about small edits, an edit
  that adds no authority applies at once, marked unconfirmed, and expires unless
  the person confirms it. An edit that asks for new authority always waits. Opus
  and Fable each proposed a form of this.
- **The word "rehearsal".** The reviewers said "simulation harness". "Harness"
  already means what fills the Agent role, so the glossary uses "rehearsal".

## Milestone 1 defects

Fixed in this round. The five code fixes each have a unit test. A fresh VM run
(22 of 22 checks) shows the main loop still works after them; it does not exercise
the fixes themselves.

| Defect | Raised by |
| --- | --- |
| A managed path through an absolute symlink wrote outside the new generation. Paths now resolve inside the generation, and the protected-path check runs on the resolved path. | All three |
| A failure after the default was written deleted the new generation and left the boot menu pointing at it. Apply now has a commit point. | All three |
| A mount target with whitespace or a newline injected fields or lines into fstab. | Sol, Fable |
| One torn ledger line made the ledger unreadable for good. | Fable |
| Two applies at once could pick the same generation id. There is now a lock. | Opus, Fable |
| The boot-affecting flag was printed, not checked, though the write-up called it an automated check. | Sol, Fable |
| The committed evidence log had been edited after the run. It is now a byte-for-byte copy of the run log. | Opus, Fable |

Left for Milestone 1b, because building generations from a pinned package set
(ADR 0009) replaces the code they are in:

| Defect | Raised by |
| --- | --- |
| Every apply leaves behind what was written to `/home` and `/var` between the apply and the reboot. | Opus, Fable |
| Declaring a base file or package and later dropping it deletes it (`/etc/sudoers`, `openssh`). | Opus |
| A declared package that is already installed is never recorded as owned. | Sol |
| Omitting `[services]` plans to disable every service. | Fable |
| A successful apply is not checked against the State afterwards, so a package script can undo a managed file unnoticed. | Sol |
| Drift accepts a service link that is broken or points at the wrong service, and sees only declared items. | Sol, Fable |
| There is no way to update Base packages. | Opus, Fable |
| Package scripts run with the host's `/dev`, `/proc`, and `/sys` bound in. | Opus, Fable |
| No automatic fallback. The planned probation boot needs GRUB to write to Btrfs, which one reviewer says it cannot do; this is untested here. | Opus |
| A generation id is reused after a cleanly failed apply. | Found while fixing |

## Where the reviewers disagreed

- **Base engine.** Sol would repair it. Opus and Fable would replace the
  convergence core. Decided for replacement (question 9).
- **The approver.** Opus would move it to crossings covered by a grant and to
  surprise detection. Fable would make it a monitor that can pause and never
  allow. Decided for Fable's form, which covers Opus's two uses (question 12).
- **Tool UI.** Opus wants a constrained web subset in place of a new declarative
  vocabulary. The others took no position. Not decided.
- **Void and runit.** Opus would re-open the choice against what the Environment
  needs. Fable says inference machines need not run CrowbarOS Base. Not decided.

## Ideas not decided yet

These are in the vision's open questions. The reviewers' own words are in their
reports; the short form:

- **A first-class browser**, and how approved fill and sessions work in it.
- **Trusted confirmation and human presence**: a "yes" bound to exact content, with
  a hardware root, that an agent or a speaker cannot produce.
- **A stop switch**: stop every task, revoke every handle, show what was running.
- **Durable tasks**: surviving a reboot, and knowing whether an effect was
  committed, refused, or unknown.
- **Selective undo** that keeps later, independent work.
- **Patching generated Tools**: a dependency inventory for each Tool, and an
  automatic rebuild that re-runs its checks.
- **Base security updates**, boot integrity, and disk encryption.
- **Loss and recovery**: lost machines, failed disks, a lost vault, a compromised
  fleet member, and backups that are not sync.
- **Life without a model** for a person who is not a developer.
- **The UI vocabulary** and **the init system**, as above.
- **A person who is not a developer** using it.

Ideas from the agents' side that the decisions did not cover:

| Idea | Raised by | Reviewer's confidence |
| --- | --- | --- |
| Every change offers a typed preview, and commits exactly what was previewed; a stale preview is refused. | Sol, Opus | Would stake the design on it |
| One structured, queryable view of the system and its events, with typed errors and suggested fixes, in place of scraping command output. | Opus, Sol | Would stake the design on it |
| "Why is this here?" as a query: any file, process, package, or handle answers with its intent, generation, actor, and time. | Opus, Fable | Would stake the design on it |
| "Would this be allowed?" as a query, with structured refusals. | Opus, Fable | Would stake the design on it |
| Results that say committed, refused, still running, or unknown, and that tell stale from current. | Sol | Would stake the design on it |
| A queue of questions for the person, so asking never blocks the work. | Opus | Speculative |
| Tasks as durable objects that another harness or model can resume. | Opus | Speculative |
| Checks that keep running as monitors after promotion. | Fable | Speculative |
| Remote inference with attestation counted as a fleet member. | Fable | Speculative |
| Build Tools mostly by assembling vetted parts, generating only small adapters. | Sol | Speculative |
| A record of real interactions, kept under privacy controls, to test a re-derive against actual use. | Sol | Speculative |
