//! Plans over handles and their deterministic interpreter.
//!
//! **Skeleton: no code yet.** Milestone 2, part one.
//!
//! A plan is a small program the Agent writes whose variables are handles. The
//! interpreter runs it, moves the actual values, and tracks the label and mark
//! of every result, of what can be observed about a result, and of every act
//! that depends on one. This is how private values stay out of the Agent's
//! context.
//!
//! Decision: ADR 0008 in `docs/adr/`.
//! Design: `docs/architecture/capability-model.md`, "Plan over handles".
//! Scope: `docs/milestones/milestone-2.md`.
