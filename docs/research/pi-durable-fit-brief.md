# Why Pi Durable is a strong fit for CrowbarOS (Milestone 3 reference brief)

**What this is.** A reference point for Milestone 3 (*The Agent at hand*) explaining why **Pi Durable** (`@earendil-works/pi-durable`) is the leading candidate for CrowbarOS's **Harness**, and naming the concrete OS-level tradeoffs Milestone 3 must verify before locking a decision record. It decides nothing on its own; the Harness decision gets its own ADR after research and grilling.

**Written:** November 18, 2026, while charting the Milestone 3 Wayfinder map.

**Read alongside:** [Vision](../VISION.md), [Lessons](../LESSONS.md), [Glossary](../../GLOSSARY.md), and [ADR 0032](../adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md).

---

## Core thesis

CrowbarOS does not treat the **Agent** as an app window. In the [vision](../VISION.md), the Agent is part of the OS: it runs as a system service from boot, the **person** summons it from anywhere with one compositor-owned gesture, it stays out of the way until called, and it acts with the person's permissions while pausing for a hand-verified "yes" before irreversible acts.

Pi Durable is built around almost the exact same shape: a single-writer durable daemon owning a local SQLite WAL store, where thin clients attach, watch, steer, and detach freely while turns, tool calls, compaction, and background tasks survive crashes, sleep, and restarts.

Adopting Pi Durable as CrowbarOS's Harness—**behind a narrow, CrowbarOS-owned local IPC boundary**—lets Milestone 3 focus on what is uniquely CrowbarOS (Fulcrum's summon and confirmation guarantees, the Shell's trusted surface, screen capture, OAuth/local Model access, and scoped OS reach) instead of spending months rebuilding durable agent-loop plumbing from scratch.

---

## Where Pi Durable aligns with CrowbarOS

### 1. Service-first lifecycle matches the Agent's place in the OS
- CrowbarOS runs Base services under `runit`, with the Compositor, Shell, and apps isolated across separate accounts ([ADR 0032](../adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md)).
- Pi Durable's single-writer daemon + multi-client subscription model maps 1:1 onto a single-person Desktop: the Harness runs from boot under `runit`, while the GPUI Agent surface can be summoned, dismissed, or even crash and restart without dropping in-flight Model turns, tool output, or queued work.

### 2. Honors "build what a person can touch first"
- The first lesson in [LESSONS.md](../LESSONS.md) is not to spend months building deep infrastructure before a person can use the Desktop for a day.
- Building a production-grade Harness from scratch—provider adapters (API keys, OAuth, local OpenAI-compatible endpoints), streaming state, background context compaction, crash recovery, structured cancellation, and tool replay safety—is a large project on its own.
- Pi Durable is MIT-licensed and ~15,000 lines of readable code intentionally structured so agents (and people) can read and extend it, which fits CrowbarOS's premise that each person grows their OS with the Agent's help.

### 3. Desktop-safe crash and cancellation semantics
- A Desktop sleeps, loses power, and reboots mid-task. Pi Durable's atomic checkpoints and default `replay: "unsafe"` tool policy mean an interrupted mutating act (a shell command, file write, or package install) is **never silently re-executed** after a crash; instead, the Harness reports the interruption and any committed partial output back to the Model.
- Its ownership tree separates **foreground** work (aborted when the person cancels the current turn) from **background** work (survives turn cancellation and continues while the surface is dismissed).

### 4. Natural seams for CrowbarOS's security lessons
[LESSONS.md](../LESSONS.md) names the guards to reach for when the Agent's reach creates real risk:
- **No Model on the trust path** & **A yes comes from the person's hand:** Pi Durable's `beforeTool` hook chain runs deterministic code before any tool executes, and `api.memo` persists the decision with the task. That provides a clean, crash-safe checkpoint to pause a mutating or privileged act, ask Fulcrum and the Shell for a hardware-verified confirmation from the person's own hand, and avoid re-asking on recovery.
- **Secrets stay out of the Agent's context:** Because Pi Durable decouples the Harness process from tool execution via `ExecutionEnv`, the Harness daemon can run under its own service account—owning the SQLite database, provider credentials, and confirmation hooks—while delegating file, shell, and app operations to a helper running as the person's account. A bad command executed as the person cannot tamper with the Harness, bypass `beforeTool`, or read provider keys.

### 5. Cache-friendly Desktop context
- Every time the person summons the Agent, Desktop state (focused window, workspaces, settings) may have changed.
- Pi Durable's dynamic `section()` prompt system rebuilds current context per turn while recording diffs in the transcript so prompt caches stay warm across turns.

### 6. Broad Model reach (API keys, OAuth, and local Models)
- Milestone 3 puts cloud API keys, OAuth flows, and local Models in scope. Building on `@earendil-works/pi-ai` (Pi Durable's Model layer) is an opportunity to inherit multi-provider support rather than hand-rolling every provider protocol—subject to primary-source verification of how `@earendil-works/pi-ai` handles OAuth and local OpenAI-compatible servers.

---

## Tradeoffs and conditions to verify in Milestone 3

Pi Durable is not a free lunch for a minimal Linux OS. The Milestone 3 research and grilling tickets must verify four concrete risks before committing an ADR:

1. **JavaScript runtime in a "kept small" Base.**
   - Base is plain Void Linux and every CrowbarOS-owned component so far is Rust. Pi Durable requires a JS runtime (Node.js 22.19+ or Bun) running from boot.
   - We need real numbers on Void package availability, rootfs footprint, boot/idle RAM, and bundling (shipping a single pinned bundle with no `npm` on the running OS).

2. **Experimental API churn.**
   - `@earendil-works/pi-durable` is explicitly experimental and evolving quickly.
   - **Architectural rule:** Do not leak Pi Durable or Chord internals into Fulcrum or the Shell. Put a narrow, CrowbarOS-owned Unix socket protocol between the Shell/Fulcrum and the Harness (*summon/context, submit, stream deltas, active tasks, request/deliver hand-verified confirmation, abort*). That keeps Pi Durable integral to Milestone 3 while preserving the freedom to upgrade cleanly or swap harnesses later.

3. **Roll-forward-only SQLite migrations vs. Btrfs rollback.**
   - Pi Durable's SQLite schema migrations are roll-forward only. Because Base uses Btrfs snapshots for rollback ([LESSONS.md](../LESSONS.md)), an OS upgrade that migrates the Harness database followed by a Btrfs rollback would leave an older Harness unable to open the newer database.
   - Milestone 3 must decide a clean mitigation (e.g., pre-migration database snapshots/backups or subvolume layout aligned with rollback).

4. **OAuth, local Models, and vision support in `@earendil-works/pi-ai`.**
   - Milestone 3 requires OAuth, local Models, and compositor screen capture (image input). Research must verify exactly what `@earendil-works/pi-ai` and `@earendil-works/pi-durable` support natively versus what CrowbarOS must build around them.
