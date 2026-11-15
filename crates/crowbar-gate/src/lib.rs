//! The gate: the deterministic rules engine that is the only thing that allows
//! an action.
//!
//! **Skeleton: no code yet.** Milestone 2, part one.
//!
//! The gate checks each broker operation, and each commit of staged writes,
//! against the principal's handles, the task's effect limits, standing grants,
//! standing rules, and the rules for labels, marks, and clearances. It signs
//! what it allows. It reads authority and never writes it. It works with no
//! model at all, and it is in the trusted base, so it must stay small enough to
//! test completely.
//!
//! Decisions: ADR 0003 and ADR 0011 in `docs/adr/`.
//! Design: `docs/architecture/gate-monitor-and-autonomy.md`, "The gate".
//! Scope: `docs/milestones/milestone-2.md`.
